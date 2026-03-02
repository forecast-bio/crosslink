---
title: Signing & Trust Model Design Analysis
tags: [security, architecture, design, signing, trust]
sources: []
contributors: [maxine-at-forecast--noether]
created: 2026-03-02
updated: 2026-03-02
---

# Signing & Trust Model Design Analysis

Design analysis for GH #106 (agent key isolation) and GH #107 (allowed_signers trust model), synthesized from a full review of the implementation and issue discussion.

## Current Architecture

Crosslink uses SSH Ed25519 keys for three signing purposes:

1. **Event signing** — each event envelope is canonicalized and signed (`crosslink-event` namespace)
2. **Comment signing** — canonical format of author+id+content signed (`crosslink-comment` namespace)
3. **Git commit signing** — hub branch commits signed via git's native SSH signing config

Keys are generated at `agent init`, stored in `.crosslink/keys/{agent_id}_ed25519` (mode 0600, dir mode 0700), and the public half is published to `trust/keys/{agent_id}.pub` on the hub branch. A driver runs `crosslink trust approve` to copy the public key into `trust/allowed_signers`, which is the master verification file.

## GH #106: Agent Key Isolation

### Threat

All agents running as the same OS user can read each other's private keys under `.crosslink/keys/`. An agent could sign as another agent.

### Analysis of Approaches

| Approach | Isolation Strength | Complexity | Platform Support | Verdict |
|----------|-------------------|------------|------------------|---------|
| Container isolation | Strong (filesystem boundary) | Low (already building PR #150) | Linux + Docker | **Best path** |
| OS user separation | Strong (kernel-enforced) | High (uid/gid management, sudo) | Unix | Operationally painful |
| Key derivation (HKDF) | Medium (no key files, but secret storage problem) | Medium | Any | Moves the problem |
| Encrypted key storage | Medium (symmetric key needs protecting) | Medium | Any | Moves the problem |
| Process sandboxing (seccomp/Landlock) | Strong | Very high | Linux only | Wrong abstraction level |

### Recommendation: Container Isolation + Current Permissions

Container-based execution (PR #150, the `crosslink container` command) is the right answer for multi-agent isolation. Each container gets its own filesystem with only its own key material. The bootstrap flow (`crosslink agent bootstrap`) already supports this: it initializes identity, generates a key, and publishes the public half to the hub — all within the container's filesystem.

For non-containerized agents (single-machine dev workflows), the current permissions model (0600/0700) is adequate because:

- **The threat is academic in practice.** If an attacker has same-user code execution, reading an SSH key is the least of your problems — they can modify the codebase directly, access credentials, etc.
- **Signing is for attribution, not authorization.** The system's goal is *proving who did what*, not *preventing actions*. Even if Agent A could forge Agent B's signature, the hooks and policy system constrain what actions agents can take. The damage from signature forgery is limited to muddying the audit trail.
- **OS-user isolation adds operational friction disproportionate to the risk.** Managing per-agent UIDs, file ownership, and group permissions for a CLI tool creates more problems than it solves.

### What Not To Build

- **Key derivation / HKDF**: Eliminates key files but requires a "master secret" stored somewhere — you're just moving the trust boundary. The container boundary is cleaner.
- **HSM / hardware-backed keys**: Appropriate for production PKI, overkill for a development coordination tool.
- **Encrypted-at-rest keys**: Requires a decryption secret per agent, which has the same isolation problem as the key itself.

## GH #107: Trust Model and Nomenclature

### The Core Tension

"allowed_signers" is SSH's native term for the verification file. But in crosslink's context, it implies an authorization guarantee ("these agents are *allowed* to sign") that's stronger than what's actually provided ("these are the agents we *know about* and can verify"). Anyone with hub branch access can modify the file.

### The Discussion (Summarized)

dollspace-gay's observation on the GH issue is correct: adding crypto-on-crypto (signed commits for allowed_signers modifications, detached .sig files) just pushes the trust boundary back without eliminating it. The people using crosslink already share repo access and must trust each other to some degree. maxine-at-forecast agrees and notes the real value is in nomenclature — helping downstream users understand what guarantees the system actually provides.

### What the System Actually Provides

The signing system provides **three guarantees**:

1. **Attribution**: A signed event/comment proves which key produced it. If you trust that Agent X holds Key K, you know Agent X authored the content.
2. **Integrity**: The signature proves content hasn't been modified since signing. Tampered content fails verification.
3. **Auditability**: The git history on the hub branch records who modified trust state and when. Combined with commit signing, you can trace trust changes.

What it does **not** provide:

- **Authorization**: The trust file doesn't prevent any action. It's a verification input, not an access control list.
- **Integrity of the trust list itself**: Anyone with hub write access can modify it. The protection is git history audit, not cryptographic.

### Recommendation: Rename + Lifecycle Metadata

**1. Rename `allowed_signers` to `known_signers`**

This is a modest but worthwhile change. It accurately communicates that the file is a registry ("we know these keys") rather than an ACL ("these keys are permitted"). The SSH verification tooling accepts any filename via the `allowedSignersFile` config — no protocol constraint here.

Scope of change: rename in `signing.rs`, `sync.rs`, `trust.rs`, and the hub branch layout. The `ssh-keygen -Y verify` calls pass the path dynamically, so it's purely a naming change.

**2. Add `valid-after` timestamps to key entries**

SSH's allowed_signers format natively supports `valid-after="YYYYMMDD"` constraints. Adding these at approval time provides basic key lifecycle tracking with zero custom code — ssh-keygen's verification will automatically reject signatures made before the key was trusted.

Format: `{principal} valid-after="20260302" ssh-ed25519 AAAA...`

**3. Don't add integrity protection beyond git commit signing**

The suggestion to add a detached `known_signers.sig` or require admin-signed commits for trust changes is engineering effort that doesn't change the fundamental trust model. Git commit history (which is already signed) is the audit trail. An external auditor can independently verify the commit chain. Layering another signature on top is redundant.

**4. Key rotation workflow (lightweight)**

For when keys need to be rotated (compromise, agent decommission):

```
crosslink agent rotate-key        # Generate new key, publish new pub
crosslink trust approve <agent>   # Driver approves new key
crosslink trust revoke-key <agent> <old-fingerprint>  # Remove old key entry
```

The old key entries should be *commented out* (with revocation timestamp) rather than deleted, preserving the ability to verify historical signatures. SSH's `valid-before` constraint handles this: `{principal} valid-before="20260302" ssh-ed25519 OLD_KEY...`

## How #106 and #107 Interact

Container isolation (#106) strengthens the trust model (#107) by making the registry's role clearer:

- When agents run in separate containers, each only has its own private key.
- The `known_signers` file becomes the *only* way to verify other agents' signatures — it's clearly a phonebook, not a permissions file.
- Key publishing (`trust/keys/{agent_id}.pub`) is the discovery mechanism for inter-container identity.
- The approval workflow (`trust approve`) is the driver vouching for an agent's identity — "I set up this container, and this is its key."

This is a clean separation: **containers provide isolation** (agents can't access each other's keys), **known_signers provides verification** (agents can verify each other's signatures), and **git history provides audit** (humans can review who trusted whom and when).

## Concrete Next Steps

### Low effort, high value (do now)
- Rename `allowed_signers` to `known_signers` across codebase and hub layout
- Add `valid-after` timestamp to entries created by `trust approve`
- Comment-out (with `valid-before`) instead of delete on `trust revoke`

### Medium effort, builds on existing work (do when containers land)
- Ensure container bootstrap produces isolated key material (already does via `agent bootstrap`)
- Document the trust model clearly in the multi-agent guide (what's guaranteed vs. not)

### Not recommended (over-engineering for the use case)
- Detached signature file for known_signers
- Admin-key-signed modifications to trust state
- HKDF key derivation replacing SSH keys
- Custom PKI or certificate chain
