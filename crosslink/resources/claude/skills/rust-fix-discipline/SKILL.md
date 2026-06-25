---
name: rust-fix-discipline
description: Use when applying fixes to Rust code identified by an audit, code review, lint sweep, failing-test triage, or any "fix this list of findings" task. Enforces the discipline of "fix the cause the system architecture calls for, not the symptom" — explicit category choice (root-cause vs. local correctness vs. annotation; defer is forbidden) for each finding, refuses cosmetic conformance and band-aids, forbids unilateral workspace-level changes from a leaf-crate context, and requires honest verification reporting. Pair with `rust-quality` and `rust-gpu-discipline`, which encode the rules being enforced. Trigger when the user says "fix the audit findings", "address the review comments", "fix the clippy errors", "make the failing tests pass", "/fix", or dispatches a subagent to apply fixes from an audit document.
---

# Rust Fix Discipline

## Why this skill exists

Fix-mode work has its own failure modes, distinct from new-code failure modes. The naive prompt *"fix this list of findings"* produces cosmetic conformance: `#[allow]` blankets that close the lint and leave the bug, empty `// SAFETY:` comments that satisfy the grep but say nothing, `panic!` → `Err` swaps that change observable behaviour without anyone noticing, surface-level changes that close the audit item without fixing the cause. This skill exists to fight that.

The model's failure mode in fix-mode is the same shape as in new-code mode (path of least resistance), but the surface looks different: instead of stubs and CPU detours, it's silenced lints and band-aided sites. The countermeasures are also similar: pre-commitment (pick a category before applying), negative-space modeling (forbidden patterns for fixes), mechanical verification, and honest reporting.

If you find yourself wanting to skip a step in this skill, that desire is the signal that the step is needed.

---

## The meta-rule

**Fix the cause the existing system architecture calls for, not the symptom the audit describes. Closing the audit item is the side effect, not the goal.**

If you can't tell what the architecture calls for, surface that uncertainty rather than guess.

---

## Step 1 — Read the context (REQUIRED, BEFORE PICKING A CATEGORY)

Before applying any fix:

1. **Read the audit finding's full notes** — the why, the action, the file:line, and any cluster mentions. Don't fix from a one-line summary.
2. **Read ≥100 lines of context** around the cited site. The reason the bug is there is rarely visible in the immediate surroundings.
3. **Read the relevant skill section on the *category* of finding.** A SAFETY-comment finding pulls in `rust-quality` §5 (Unsafe code). A CPU-fallback finding pulls in `rust-gpu-discipline` §3 (Device-error policy). Don't fix on the audit's summary alone.
4. **Find call sites.** For any change to a `pub` item or function signature, search the workspace. If call sites would break, that informs the fix shape — sometimes the fix is *"I can't change this without breaking 12 call sites; this is now a coordination request."*
5. **Scan for sibling patterns.** When the audit flags one site of a pattern, scan the file for siblings. The fix is the cluster, not the line.
6. **Reassess severity.** If your context-reading shows the audit overstated, misclassified, or got the failure mode wrong, **say so explicitly** before picking a category. Common cases: the audit called something a "silent CPU fallback" but reading the code shows it returns `Err` cleanly; the audit flagged `pub` fields on a value-type snapshot where direct access is intentional; the audit flagged a panic that's actually unreachable given a guard upstream; the audit flagged a print that the test harness already surfaces. In these cases, the right action is often documentation-only, a smaller follow-up, or no-op rather than the audit's stated fix. Downgrade the working severity in your output (🔴 → 🟡, or "no fix needed — audit was wrong here") and explain the reassessment in one or two sentences.
7. **Verify stdlib API stability before applying recommended migrations.** Audits sometimes recommend switching to a stdlib API ("X is stable since Rust 1.Y") based on docs.rs lookups that don't reliably distinguish "tracking issue exists" from "feature is stable on the target rustc." `rustc --version` and a minimal `cargo check` probe are the ground truth — if the migration fails to compile with `error[E0658]: use of unstable library feature` or similar, the audit was wrong about stability. Don't bypass via `#[allow(unstable_features)]` (that's Step 3 #1, blanket suppression) or unilaterally bump MSRV to nightly (that's Step 4, workspace coordination). Document the constraint with an inline comment naming the tracking issue, and either keep the existing approach (if no better stable alternative) or file a Step 4 coordination request (e.g., MSRV bump) when the unstable feature is genuinely the right answer.

If any of these steps surfaces something that contradicts the audit's recommended action, that's the right place to deviate. The audit was a static observation, not a binding spec.

---

## Step 2 — Pick a category (THE THREE-WAY DECISION TREE)

For every finding, explicitly pick one of the three categories below. **Picking is the work, not paperwork.** Justify the pick in one or two sentences before showing the diff.

> **There is no Category D ("defer").** It existed in earlier versions of this skill and was removed because subagents used it as an escape hatch from cost-of-implementation work. See "What if the right fix can't be applied unilaterally?" below for the replacement mechanism.

### Category A — Root-cause fix

Change the underlying architecture, API, or invariant so the class of finding can't recur.

*When to use*: the finding is a symptom of architectural drift; the workspace would benefit from the structural fix.

*Examples*:

- 600+ `T::from(x).unwrap()` panic vectors → one missing `cast<T>(v) -> Result<T>` helper in the foundation crate + a workspace sweep to use it.
- 65+ "CPU fallback" sites → one missing device-error policy (per `rust-gpu-discipline` §3) + `Err` returns at all sites.
- 14 hand-rolled `Display + Error` impls → one `thiserror` migration per crate.
- A `Clone::clone` that contains `.expect()` → wrap the inner field in `Arc` so clone becomes infallible.

### Category B — Local correctness fix

Keep the architecture, fix this exact site properly, with full reasoning written down.

*When to use*: the architecture is correct and this site is the only thing wrong.

*Examples*:

- A kernel-launch `unsafe { … }` block gets a `// SAFETY:` comment that names the **specific** invariant for that call (kernel arity, buffer lifetimes, alignment, grid coverage) — not a copy-paste boilerplate.
- An `assert!` in a public constructor becomes `if … { return Err(...) }` with the error variant chosen to match the existing taxonomy.
- A `pub` field becomes `pub(crate)` after verifying no external call site reads it.

### Category C — Annotation fix

The finding is genuinely wrong here, and a narrow suppression with a justifying comment is the right answer.

*When to use*: the lint is correct in 90%+ of cases but happens to be wrong here, and the suppression is documented.

*Examples*:

- `#[allow(clippy::approx_constant)]` on a test that hardcodes `3.14` to verify a placeholder-detector.
- `#[allow(clippy::too_many_arguments)]` on an FFI signature that mirrors an upstream C API and can't be reduced — comment says so.

*Required*: scoped narrowly (item-level, not crate-level) and accompanied by a comment naming why the lint is wrong here. A bare `#[allow]` without a comment is forbidden — see Step 3.

### What if the right fix can't be applied unilaterally?

**There is no Category D — defer.** Findings that require workspace-level changes (Step 4 territory: new variants in shared error enums, new workspace deps, cross-crate API breaks) do not become a deferred decision — they follow a different mechanism:

- **You still apply the fix in your crate's scope** (Category A or B as applicable).
- **You file a Step 4 `requires-coordination` block** in your report describing the cross-crate work that the orchestrator must do separately.
- **If a binding policy decision is missing as a prerequisite** (e.g., the workspace hasn't yet adopted a `cast<T>` helper but every fix needs it), you do not defer — you escalate to the orchestrator: *"Cannot proceed without prerequisite ADR/decision X."* That is a different shape from "defer for user input." It names what is missing rather than offering options.

The discipline is that **the policy IS the decision**. `rust-gpu-discipline` §3 (PyTorch parity) and `rust-quality` conventions (use `thiserror`, return `Result`, etc.) are binding. The model already has the answer to most "should we do X or Y?" questions: do what the policy/PyTorch says. Cost-of-implementation is a scheduling concern (Step 5 — scope per work unit), not a category concern. If a fix is "too much work" it is the *orchestrator's* problem to re-bundle or re-schedule, not the subagent's to surface as a question.

### Picking is the work

The category choice is the substance of fix discipline, not a paperwork step. Picking C when A was right is the failure mode this skill exists to fight. **Wanting to "defer" when the policy already gives the answer is the related failure mode that motivated removing Category D entirely.** If you find yourself reaching for C frequently, or wanting a "this is too much work" escape, you are probably wrong about the category — re-read Step 1 (Read the context, including step 6 severity reassessment) before forcing the call.

---

## Step 3 — Forbidden fix-mode patterns

Concrete patterns that constitute taking the lazy path. Write none of them.

1. **The blanket suppression.** `#[allow(clippy::unwrap_used)]` at crate root, `#![allow(unsafe_code)]` without per-module overrides, removing a `print_stderr` lint by adding `#[allow(clippy::print_stderr)]` instead of removing the print. If a lint is right in 90% of cases and wrong in 10%, allow narrowly per-item with a comment.
2. **The empty SAFETY comment.** `// SAFETY: this is fine`, `// SAFETY: trust me`, `// SAFETY: see above`. The `rust-quality` rule is *"name the invariant and prove it locally."* A comment that doesn't name the invariant fails worse than the missing comment did, because it now also lies.
3. **The TODO masquerading as a fix.** `// TODO: handle errors properly`, `// FIXME: should return Result`. If you wrote the TODO, you didn't fix it; you logged that you didn't.
4. **The single-site fix in a cluster.** When the audit identifies a pattern (e.g., 15 unsafe blocks in `bf16.rs` with no SAFETY), fixing one is wrong. The first move on encountering a cluster is to scan the file for all instances and treat them as one work unit — even if the audit only flagged one line. If the cluster exceeds the work unit's scope, that triggers Category D, not *"I'll do one and call it done."*
5. **The breaking change disguised as cleanup.** Removing `pub`, narrowing a function's signature, changing a return type from `T` to `Result<T>`. These break call sites. **Required before applying**: search the workspace, prove call sites still compile, OR provide a backward-compatible accessor with the same shape, OR call out the breaking change explicitly in the report.
6. **The architectural change made unilaterally from a leaf crate.** Don't author a `cast<T>(v)` helper inside `ferrotorch-llama`; that helper belongs in `ferrotorch-core`. Don't add a new `GpuError` variant from inside a crate that consumes `GpuError`; that's a coordination request. Workspace-level changes need workspace-level review. See Step 4.
7. **The semantic change unannounced.** Replacing `panic!` with `Err(...)` *changes externally observable behaviour* — callers using `catch_unwind` see different state; callers that didn't will now see propagation. Replacing `eprintln!` with `tracing::warn!` requires a `tracing` subscriber in the consumer or the message vanishes. Every behavioural change gets a line in the report: *"this is now a returned error, not a panic."*
8. **The "tests pass" terminal state.** Tests passing is necessary, not sufficient. The report has to say *why* the fix is correct, not just *"no regressions."* Existing test coverage may not exercise the buggy path (the workspace audit found GPU tests that don't construct GPU devices).
9. **The cargo-cult fix.** *"The audit said `pub` fields are bad, so I made them private with getters"* — for a value-type snapshot whose entire purpose is field access. Read the audit's *why*, not just the *what*. Sometimes `#[non_exhaustive]` is the right answer; sometimes accessors are; sometimes the audit was wrong.
10. **The optimistic verification.** *"I didn't run cargo because the box doesn't have it; assuming it builds."* If you can't verify, **say so** in the report — don't claim success. (See Step 6.)
11. **The unverified call-site claim.** *"No callers outside this crate — verified via grep."* without showing the command and its output. Self-reported verification claims for cross-crate impact are exactly the failure mode that ships breaking changes through per-crate test suites. Step 7's `Call-site impact` line requires the literal grep command + the one-line result. A narrative claim is an overclaim.
12. **The deferral as escape hatch.** *"I'll defer this for coordination because [scope / cost / cross-crate impact]."* "Defer" is no longer a category in this skill (Category D was removed — see Step 2). The lazy-path failure mode it created — using "defer" to escape doing the harder, correct work that policy already prescribes — is now an explicit forbidden pattern. If the discipline (especially `rust-gpu-discipline` §3 PyTorch parity and `rust-quality` conventions) has already given the policy answer, you apply the fix or surface the missing prerequisite. You do not say "this is a design question for the user" when the policy *is* the design decision. Cost and scope go to Step 5 (work-unit scope); cross-crate impact goes to Step 4 (coordination block). Neither is a deferral.

---

## Step 4 — Coordination boundaries

Three classes of change a per-crate subagent must **not** make unilaterally. For these, file a `requires-coordination` block in the report; do not apply.

- **Workspace-level**: `[workspace.lints]` table, MSRV bump, new workspace dep, new workspace member, new feature flag with cross-crate reach.
- **Cross-crate API**: changing a `pub` item in crate A whose call sites span crates B, C, D. Even a "harmless" rename is a coordination event.
- **Shared abstractions**: a new variant in a workspace error enum, a new trait, a new helper that should live in the foundation crate but is being authored from a leaf.

  **Shared error enums are particularly sensitive.** When a workspace has a single shared error type (e.g. `FerrotorchError`, `MyAppError`) that ~all crates `use`, adding a variant changes *every consumer's pattern-matching surface*. Even a `_ =>` fallback arm in a downstream `match` may suddenly catch new cases that callers wanted to handle explicitly. Treat any new variant as requiring (a) an explicit cross-crate decision, (b) a workspace-wide audit of `match <SharedError> { ... }` sites, and (c) a `cargo build --workspace` after the change. Prefer extending an existing variant's structured payload over adding a new variant when the new error category is a refinement of an existing one.

The coordination request describes: what you'd add, why, who would consume it, what the alternatives are. The orchestrator handles those in a separate pass *before* the affected crate-level fixes proceed.

Without this discipline, you get five subagents independently inventing five flavours of the same helper in five different crates.

---

## Step 5 — Scope per work unit

Don't dispatch *"fix all findings in crate X."* Bundle:

- **By category and physical proximity.** 5–10 SAFETY-annotation fixes in one file = one bundle. A `T::from().unwrap()` sweep in one file = one bundle.
- **One bundle per architectural decision.** The device-error policy migration (`rust-gpu-discipline` §3) is one bundle. The error-type strategy is another. Each gets its own focused subagent so the design conversation has space.
- **Single-finding bundle for high-stakes.** Security findings (path traversal, DoS), `Clone::clone` redesigns, lying-success stubs, policy decisions. The design conversation is the work; let it have its own bundle.

When orchestrating multiple subagents:

- **Phase 1 (workspace decisions)**: a small number of dedicated subagents, each on one design question. Produces ADR-style outputs that drive Phase 2.
- **Phase 2 (mechanical sweeps)**: many subagents executing decided policy. SAFETY-annotation passes per crate. `T::from().unwrap()` → `cast()?` per crate.
- **Phase 3 (focused redesigns)**: one subagent per high-stakes finding. Each gets its own bundle and the output is a careful PR-shaped diff, not a sweep.

**Cost-of-implementation is the orchestrator's concern, not the subagent's.** When a subagent finds that the right fix exceeds the dispatch's scope, they must surface this *explicitly* as either (a) a Step 4 coordination block describing cross-crate work, or (b) an escalation — *"this fix requires prerequisite ADR/decision X."* The orchestrator then re-bundles or schedules. **Subagents do not get to decide via "defer" — there is no defer category** (per Step 3 #12). The category they pick (A / B / C) is what they apply within scope; everything else is escalation, not deferral.

---

## Step 6 — Verification mandate

For every fix, before reporting:

- `cargo build -p <crate>` — must pass.
- `cargo clippy -p <crate> --all-targets --all-features -- -D warnings` — must pass; if any new `#[allow]` was introduced, justify in the report.
- `cargo test -p <crate>` — must pass, doctest count included; if a public item was modified, the corresponding doctest must still compile.
- For `unsafe` changes: `cargo +nightly miri test -p <crate>` — must pass; if nightly isn't available on this box, **say so explicitly**.
- For changes to `pub` API: `cargo build --workspace` — must pass to confirm cross-crate call sites still compile.

If any step was skipped, **say so explicitly** in the report rather than implying success. The honest underclaim is always better than the unverified overclaim.

### The workspace-build requirement is non-negotiable for `pub` API changes

Per-crate verification is necessary but **not sufficient** when a change crosses crate boundaries. `cargo test -p crate-A` will pass even if your edit broke `crate-B` — because the per-crate test only compiles `crate-A`. For any change to a `pub` item's signature, return type, or trait bounds, `cargo build --workspace` (or `cargo build -p` for every crate that depends on the changed crate) is required.

If a dispatch prompt, operator instruction, or scope constraint tells you to skip the workspace build for a `pub` API change, that instruction is **wrong** — surface the conflict in your report before applying the change. Examples of dispatch overrides that are not authoritative:

- *"Do not run anything that compiles the whole workspace."* — fine for non-pub changes; **must be lifted** when a `pub` item's signature changes.
- *"Per-crate cargo invocations only."* — same.

The skill mandate wins. Per-crate verification + literal grep evidence (Step 7) for cross-crate call sites is the minimum bar; targeted reverse-dependency `cargo build -p` invocations are the operationally cheap alternative to a full workspace build when the workspace is large.

### `#[non_exhaustive]` is a workspace-build event

Adding `#[non_exhaustive]` to a `pub` struct or enum is semver-minor in spirit (no fields or variants change), but it **does break external code** in two ways:

- **On structs**: external code using struct-literal syntax `Foo { field1: ..., field2: ... }` will fail to compile. Only `Foo::new(...)` constructors, `Foo::default()`, and accessors continue to work.
- **On enums**: external code with exhaustive `match` on the enum will fail to compile without a `_ =>` arm. Existing exhaustive matches break.

Treat `#[non_exhaustive]` additions to `pub` items as requiring the workspace-build mandate above, even though no fields or variants change. The grep-evidence requirement (Step 7) applies with a *type-specific* search:

- For structs: `grep -rn 'Foo {' --include='*.rs' /workspace/root/ | grep -v 'crate-under-edit/'` — finds struct-literal construction sites.
- For enums: `grep -rn 'match.*Bar\|Bar::' --include='*.rs' /workspace/root/ | grep -v 'crate-under-edit/'` — finds match sites that may be exhaustive without a fallthrough.

The same goes for **removing** `#[non_exhaustive]` (rare, but the inverse change can also surprise consumers).

---

## Step 7 — Per-fix output template

Each fix entry:

```
### Finding: <audit reference, e.g., "🔴 #2 in ferrotorch-gpu/kernels.rs">
**Severity reassessment**: <"audit severity correct"
                            OR "downgraded from 🔴 to 🟡 because <X>"
                            OR "no fix needed — audit was wrong about <Y>">
**Category**: A (root-cause) | B (local correctness) | C (annotation)
**Justification**: <one or two sentences: why this category, not the others>
**Cluster awareness**: <"this fix also addresses N sibling sites at lines X, Y, Z"
                       OR "unique to this site, sibling cases checked at A, B, C"
                       OR "sibling cases at X, Y, Z exceed scope; deferred">
**Behavioural change**: <"none" OR specific external observable change>
**Call-site impact**: <REQUIRED: include the literal grep command run AND a
                       one-line summary of its output. Self-reported claims
                       like "verified via grep" without showing the command
                       and result do not satisfy this requirement.
                       Canonical command shape (use this template; adjust
                       only the symbol and the workspace path):
                         `grep -rn 'Symbol' --include='*.rs' /workspace/root/ \
                          | grep -v 'crate-under-edit/'`
                       The `--include='*.rs'` filter is required (skip target/
                       and other build artifacts); the trailing `grep -v` is
                       required (we want callers OUTSIDE the crate-under-edit).
                       Examples:
                       - "Ran `grep -rn 'Foo::new' --include='*.rs' /home/me/repo/ \
                          | grep -v 'foo-data/'`: 0 hits outside this crate."
                       - "Ran `grep -rn 'Foo::new' --include='*.rs' /home/me/repo/ \
                          | grep -v 'foo-data/'`: 3 hits in foo-vision/src/x.rs:26
                          and foo-train/src/y.rs:14,42; verified all compile via
                          `cargo build -p foo-vision -p foo-train`.">
**Diff**: <minimal>
**Verification**:
  cargo build:                            PASS / FAIL / NOT RUN (reason)
  cargo clippy -- -D warnings:            PASS / FAIL / NOT RUN
  cargo test:                             PASS / FAIL / NOT RUN
  cargo +nightly miri test (if unsafe):   PASS / FAIL / NOT RUN
**Requires coordination**: yes / no — and if yes, what
```

---

## Step 8 — Honest reporting vocabulary

Use:

- **"Applied root-cause fix"** — only when the architecture is actually changed, not just the site.
- **"Applied local correctness fix"** — site-only, with reasoning.
- **"Suppressed via narrow allow + comment"** — for Category C, with the comment quoted in the report.
- **"Cannot proceed — prerequisite missing: <ADR or coordination decision>"** — used when a binding workspace decision is missing and is needed before this fix can land. Names the specific missing prerequisite. **Never used to escape cost-of-implementation work** (per Step 3 #12).
- **"Filed Step 4 coordination block: <cross-crate work>"** — used when the fix is applied locally but downstream callers, shared error enums, or workspace deps need follow-up work scheduled separately. The fix lands in this dispatch; the follow-up is a separate dispatch.
- **"Behavioural change: <X>"** — explicit line for any externally observable change.
- **"Verified: <list>; not verified: <list with reason>"** — split honestly.

Forbidden vocabulary:

- **"Fixed the lint"** — vague; describe the actual change and why it's correct.
- **"Tests still pass"** — necessary, not sufficient; describe why the fix is correct.
- **"Should be safe"** — say *why*, citing the invariant.
- **"Following the audit recommendation"** — describe the actual change you made, not who suggested it.
- **"Closed the finding"** — the closure is a side effect; describe what changed.

---

## Cross-references

The rules being enforced live in two skills, which must be loaded alongside this one:

- **`rust-quality`** — universal Rust discipline (API design, error handling, lint posture, unsafe SAFETY discipline, testing, docs).
- **`rust-gpu-discipline`** — GPU-specific discipline. **§3 is the device-error policy** (PyTorch parity, hard requirement) that all CPU-fallback fixes must conform to. This skill enforces the choice between fix categories; the policy itself is in §3.

When a finding cites a rule from one of those skills, read the skill's section before fixing. Don't fix from the audit's one-line summary.
