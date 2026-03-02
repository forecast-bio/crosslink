# Design: Container-Based Agent Execution

**GH Issue:** [#110](https://github.com/forecast-bio/crosslink/issues/110)
**Status:** Draft v3
**Last updated:** 2026-03-02
**Depends on:** [Event-Sourced Coordination](DESIGN-EVENT-SOURCED-COORDINATION.md) (Phase 2+)

---

## 1. Problem Statement

The current `crosslink kickoff` workflow spawns background agents in tmux sessions. This works on Linux/macOS but creates three fundamental problems:

1. **Platform lock-in.** tmux doesn't exist on Windows. Users must run through WSL, which prevents native Windows tooling (VS Code extension, PowerShell workflows) from managing agents directly.

2. **Interactive trust bottleneck.** Every launched agent requires a human to `tmux attach` and approve Claude's initial trust prompt before work begins. With 3+ agents, this becomes a serial chore that defeats the purpose of parallel autonomous execution.

3. **Terminal multiplexer coupling.** The kickoff skill is tightly bound to tmux's session/pane model. VS Code extension, CI runners, and remote execution would each need their own integration layer.

### What we want instead

A container is a natural sandbox for autonomous code agents:

- **`--dangerously-skip-permissions` becomes safe.** The container *is* the permission boundary. The agent can freely read/write files, run builds, execute tests — its blast radius is the container, not the host. No interactive trust approval needed.
- **Hooks still enforce policy inside the container.** Even with `--dangerously-skip-permissions`, crosslink hooks gate git mutations. The agent can commit (gated by active issue) but cannot push, rebase, or force-reset. The human remains the gatekeeper for anything leaving the local machine.
- **Cross-platform.** Docker runs on Windows, macOS, and Linux. Same `crosslink kickoff` command everywhere.
- **Disposable.** If an agent corrupts its environment, destroy the container. The code is on a git-backed mount — nothing of value lives only in the container.

---

## 2. The Human-in-the-Loop Model

This is the core workflow constraint. Subagents never push to the remote. The human is always the final reviewer.

### Current effective workflow (manual)

```
1. Human tells head Claude: "implement X"
2. Head Claude creates feature branch + worktree
3. Head Claude (or kickoff agent) implements the feature
4. Head Claude commits to the branch
5. Human reviews the diff
6. Human pushes:  git push -u origin feature/x
7. Head Claude creates the PR:  gh pr create ...
```

This works well. The design preserves it exactly — containers just make step 3 autonomous and parallel.

### Proposed workflow (container-based)

```
1. Human tells head Claude: "kickoff X"
2. Head Claude creates feature branch + worktree(s)
3. Head Claude builds container image (if needed) and starts task container
4. Inside the task container, agents work autonomously:
   - Lead agent reads KICKOFF.md and starts implementing
   - Lead agent may spawn sub-agents (fork claude processes in separate worktrees)
   - All agents: read/write files freely (shared mounts)
   - All agents: commits gated by crosslink issue (hooks enforced)
   - All agents: push is BLOCKED (hook tier 1, always)
   - Lead agent writes DONE to .kickoff-status when all work is finished
5. Head Claude on host detects completion, reviews the diff
6. Head Claude presents summary to human
7. Human pushes the branch
8. Head Claude creates the PR

At no point does any automated process push to the remote.
```

### Why this works

The permission model has three layers:

| Layer | What it controls | Who enforces it |
|-------|-----------------|-----------------|
| **Container boundary** | File system, network, processes | Docker (the agent can't touch the host beyond its mounts) |
| **Crosslink hooks** | Git mutations, issue tracking | `work-check.py` running inside the container (same hooks, same config) |
| **Human gatekeeper** | Push to remote, PR creation, merge | The human, assisted by head Claude on the host |

The container handles blast radius. The hooks handle policy. The human handles trust.

---

## 3. Architecture

### 3.1 The task container model

The container is not a per-agent sandbox — it's a **shared workspace** where the entire agent swarm operates. One container per kickoff task. Multiple `claude` processes run inside the same container, each in its own worktree, coordinating through the crosslink hub branch.

```
Host machine
├── ~/.claude/.credentials.json         (mounted read-only → auth only)
├── project/
│   ├── .crosslink/
│   │   ├── .hub-cache/                 (mounted read-write → shared coordination)
│   │   └── hook-config.json            (copied into container at init)
│   ├── .worktrees/
│   │   ├── feat-implement-x/           (mounted read-write → /workspaces/feat-implement-x)
│   │   ├── feat-subtask-a/             (mounted read-write → /workspaces/feat-subtask-a)
│   │   └── feat-subtask-b/             (mounted read-write → /workspaces/feat-subtask-b)
│   └── .git/                           (mounted read-write → git objects shared)

Task container (single, long-lived)
├── /workspaces/
│   ├── feat-implement-x/               (lead agent worktree)
│   ├── feat-subtask-a/                 (sub-agent worktree)
│   └── feat-subtask-b/                 (sub-agent worktree)
├── /home/agent/.claude/                (writable, CLAUDE_CONFIG_DIR)
│   └── .credentials.json              (copied from host mount at start)
├── /host-auth/.credentials.json        (read-only bind mount from host)
├── crosslink, cargo, node, etc.        (toolchains installed at first start)
└── claude processes (N concurrent)
    ├── Agent 1: lead agent in feat-implement-x
    ├── Agent 2: subtask-a agent in feat-subtask-a
    └── Agent 3: subtask-b agent in feat-subtask-b
```

**Why one container, not N:** A Docker storm of containers adds management overhead for no benefit. The agents don't need isolation from each other — they need isolation from the host. Inside the container, crosslink's lock system prevents agents from stepping on each other, and each agent works in its own worktree. The container is the blast radius boundary around the whole swarm.

### 3.2 Mount strategy

**Worktree mounts (read-write).** Each worktree directory is bind-mounted into the container under `/workspaces/`. Head Claude on the host sees all file changes in real-time. `.kickoff-status` sentinels work without any sync. Git history is preserved because the mount is a real git worktree.

**Git objects mount (read-write).** The project's `.git/` directory is mounted so that all worktrees share git objects (this is how git worktrees work — the worktree's `.git` file points to the main repo's `.git/worktrees/` entry). Without this, `git commit` inside the container would fail.

**Hub cache mount (read-write).** The main repo's `.crosslink/.hub-cache/` is mounted into the container. All agents (host and container) share the same coordination worktree. This works because:
- The event-sourced model ensures each agent writes only to its own directory (`agents/{id}/*`)
- Compaction uses a lease to prevent concurrent compactors
- `git fetch` and `git push` on the hub branch are atomic operations
- The hub cache already handles concurrent access from multiple worktree agents on the host — the container is no different

**Auth mount (read-only, credentials only).** Only `~/.claude/.credentials.json` is mounted, not the entire `~/.claude/` directory. Claude CLI writes extensively during sessions (history, session-env, file-history, statsig, telemetry, cache, etc.) and a read-only mount of the full directory would crash immediately. Instead:

1. Mount `~/.claude/.credentials.json` read-only at `/host-auth/.credentials.json`
2. Set `CLAUDE_CONFIG_DIR=/home/agent/.claude` (writable dir inside container)
3. The entrypoint copies `.credentials.json` from the read-only mount into the writable config dir
4. Each `claude` process writes its session state to the writable config dir freely
5. On token refresh, the refreshed token lives only inside the container — if the container is destroyed, the host's original credentials remain untouched

**Hook config (copied, not mounted).** `crosslink init --force` inside the container writes hooks from the embedded binary. The host's `hook-config.json` is copied (not mounted) because `crosslink init` may need to write to it during setup. The copy inherits the host's policy (tracking mode, blocked commands, etc.).

### 3.3 Container image

Rather than auto-generating Dockerfiles per-project (complex, fragile), use a single **crosslink base image** with a layered toolchain install:

```dockerfile
FROM ubuntu:24.04

# Core tooling (python3 required for crosslink hooks)
RUN apt-get update && apt-get install -y \
    git openssh-client curl ca-certificates jq python3 \
    && rm -rf /var/lib/apt/lists/*

# Non-root agent user
RUN useradd -m -s /bin/bash agent
USER agent
WORKDIR /home/agent

# Claude CLI
RUN curl -fsSL https://claude.ai/install.sh | sh

# Crosslink binary (copied from host or downloaded)
COPY --chown=agent crosslink /usr/local/bin/crosslink
COPY --chown=agent entrypoint.sh /usr/local/bin/crosslink-entrypoint.sh
RUN chmod +x /usr/local/bin/crosslink-entrypoint.sh

WORKDIR /workspaces
ENTRYPOINT ["/usr/local/bin/crosslink-entrypoint.sh"]
```

**Why not auto-generate Dockerfiles?**

The original issue (#110) proposed detecting `Cargo.toml` / `package.json` / etc. and generating project-specific Dockerfiles. After review, a runtime detection approach is simpler and more maintainable:

1. Auto-generation means maintaining N Dockerfile templates that drift from upstream toolchain images
2. Users who need custom images can provide their own (see `container.image` config)
3. Runtime toolchain install adds ~30-60s to first container start but the image layer is cached after that
4. A single base image means `crosslink` can ship one Dockerfile, not a template engine

### 3.4 Toolchain detection and install

At container start, the entrypoint script detects the project and installs toolchains:

```bash
#!/bin/bash
set -euo pipefail
# crosslink-entrypoint.sh

# --- Auth setup ---
# Copy credentials from read-only host mount into writable config dir
mkdir -p /home/agent/.claude
if [ -f /host-auth/.credentials.json ]; then
    cp /host-auth/.credentials.json /home/agent/.claude/.credentials.json
fi
export CLAUDE_CONFIG_DIR=/home/agent/.claude

# --- Toolchain detection ---
# Scan the first mounted workspace for project files
WORKSPACE=$(ls -d /workspaces/*/ 2>/dev/null | head -1)
if [ -n "$WORKSPACE" ]; then
    if [ -f "$WORKSPACE/Cargo.toml" ]; then
        echo "Detected Rust project, installing toolchain..."
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --quiet
        source ~/.cargo/env
    fi

    if [ -f "$WORKSPACE/package.json" ]; then
        echo "Detected Node project, installing toolchain..."
        curl -fsSL https://deb.nodesource.com/setup_22.x | sudo bash -
        sudo apt-get install -y nodejs
    fi

    if [ -f "$WORKSPACE/pyproject.toml" ] || [ -f "$WORKSPACE/requirements.txt" ]; then
        echo "Detected Python project, installing toolchain..."
        sudo apt-get install -y python3-pip python3-venv
        pip install --user uv
    fi

    if [ -f "$WORKSPACE/go.mod" ]; then
        echo "Detected Go project, installing toolchain..."
        curl -fsSL https://go.dev/dl/go1.23.linux-amd64.tar.gz | sudo tar -C /usr/local -xzf -
        export PATH=$PATH:/usr/local/go/bin
    fi
fi

# --- Run the actual command ---
exec "$@"
```

**Toolchain caching:** After first run, `crosslink container snapshot` can commit the running container as a project-specific image (e.g., `crosslink-agent:myproject`). Subsequent starts skip toolchain install entirely. This is a Phase 2 optimization.

### 3.5 Container lifecycle

```
crosslink kickoff "feature description"
  │
  ├─ 1. /featree creates worktree(s) + branch(es) + agent identities
  │
  ├─ 2. Build/pull container image (if not cached)
  │     └─ crosslink container build [--force]
  │
  ├─ 3. Start task container (single container for the swarm)
  │     └─ docker run -d \
  │          --name crosslink-task-feat-implement-x \
  │          -v .worktrees/feat-implement-x:/workspaces/feat-implement-x \
  │          -v .git:/repo/.git:rw \
  │          -v .crosslink/.hub-cache:/repo/.crosslink/.hub-cache:rw \
  │          -v ~/.claude/.credentials.json:/host-auth/.credentials.json:ro \
  │          crosslink-agent:latest \
  │          claude --dangerously-skip-permissions -- "$(cat KICKOFF.md)"
  │
  ├─ 4. Inside container: lead agent may spawn sub-agents in additional worktrees
  │     (each sub-agent is another `claude` process in the same container)
  │
  ├─ 5. Monitor from host: poll .kickoff-status or docker logs
  │
  └─ 6. On completion: review diff, present to human
```

**Sub-agent spawning inside the container:** The lead agent inside the container can create additional worktrees and launch sub-agents using the same crosslink skills (`/featree`, then running `claude` directly). Each sub-agent gets its own worktree directory under `/workspaces/`, its own agent identity, and its own crosslink issue. The lead agent coordinates them through the hub branch — same as the host model, but all within the container.

This means the `/kickoff` skill inside the container doesn't need Docker — it just forks `claude` processes directly. The container is already the sandbox.

---

## 4. Permission Gating Inside Containers

### 4.1 `--dangerously-skip-permissions` + hooks = controlled autonomy

Claude's `--dangerously-skip-permissions` flag skips the interactive trust prompt but does NOT skip hooks. Hooks are configured in `.claude/settings.json` and execute regardless of permission mode. This is the key insight that makes the container model work.

**What the agent can do freely:**
- Read any file in `/workspace`
- Write/edit any file in `/workspace`
- Run `cargo test`, `npm test`, `cargo clippy`, etc.
- Run `crosslink` commands (create issues, comment, sync)
- Run `git status`, `git diff`, `git log`, `git show`

**What the agent can do with an active crosslink issue:**
- `git commit` (gated by tier 2 in `work-check.py`)

**What the agent can NEVER do (tier 1 block, all modes):**
- `git push` (any form)
- `git merge`, `git rebase`, `git cherry-pick`
- `git reset`, `git checkout .`, `git restore .`, `git clean`
- `git stash`, `git tag`, `git am`, `git apply`
- `git branch -d`, `git branch -D`, `git branch -m`

These blocks are in the hook, not in Claude's permission system. They fire even with `--dangerously-skip-permissions`. The agent literally cannot push — the hook exits with code 2 and the tool call is rejected.

### 4.2 Hook installation in containers

When `crosslink init --force` runs inside the container (as part of `/featree`), it writes:
- `.claude/hooks/work-check.py` — the gate
- `.claude/hooks/crosslink_config.py` — shared library
- `.claude/settings.json` — hook configuration
- `.crosslink/hook-config.json` — policy (copied from host or inherited)

The hooks are embedded in the crosslink binary (`include_str!`), so no network fetch is needed. The container just needs `crosslink` and `python3`.

### 4.3 Additional container-specific restrictions

Beyond the standard hook gates, containers can optionally restrict:

**Network access:**
- Default: full network access (agents may need docs, crate registries, npm)
- Restricted: `--network=none` for maximum isolation (breaks package install, doc lookup)
- Future: allowlist specific domains via Docker network policy

**Resource limits:**
- Memory: dynamically allocated based on host (total RAM minus 2GB reserve, minimum 4GB)
- CPU: no limit (all cores available, OS scheduler handles sharing)
- Disk: no explicit limit (worktree is on host filesystem)

These are configured via `hook-config.json` (overrides auto-detection):

```json
{
  "container": {
    "memory": "auto",
    "cpus": "auto",
    "network": "bridge",
    "extra_mounts": [],
    "extra_env": {}
  }
}
```

Set explicit values to override auto-detection: `"memory": "32g"`, `"cpus": 8`.

---

## 5. CLI Commands

### 5.1 Container image management

```bash
# Build the crosslink agent base image
crosslink container build [--force] [--tag <tag>]

# Use a custom Dockerfile instead of the built-in one
crosslink container build --dockerfile path/to/Dockerfile

# List built images
crosslink container images
```

`crosslink container build`:
1. Writes the embedded Dockerfile to a temp directory
2. Copies the current crosslink binary into the build context
3. Runs `docker build -t crosslink-agent:latest .`
4. On `--force`, rebuilds from scratch (`--no-cache`)

### 5.2 Container lifecycle (low-level)

```bash
# Start a task container (usually called by kickoff, not directly)
crosslink container start <worktree-path> [--name <name>] [--prompt <file>]

# List running task containers
crosslink container ps

# Stream logs from a container
crosslink container logs <name> [--follow]

# Drop into a running container for debugging
crosslink container shell <name>

# Stop a container gracefully (all agents inside stop)
crosslink container stop <name>

# Remove a stopped container
crosslink container rm <name>

# Stop + remove
crosslink container kill <name>

# Snapshot a running container as a cached image (saves toolchain install)
crosslink container snapshot <name> [--tag <tag>]
```

`crosslink container start`:
1. Resolves worktree path to absolute, finds main repo root
2. Reads `KICKOFF.md` from worktree (or uses `--prompt`)
3. Builds `docker run` command with mounts (worktree, .git, hub-cache, credentials), env, resource limits
4. Starts container in detached mode
5. Writes container ID to `.crosslink/container-id` in the worktree
6. Returns container name

`crosslink container ps`:
- Runs `docker ps --filter label=crosslink-agent`
- Cross-references with worktree paths and crosslink issues
- Shows: container name, status, issue(s) being worked, uptime, resource usage

### 5.3 Updated kickoff skill

The `/kickoff` skill gains a `--container` flag (which becomes the default when Docker is available):

```
/kickoff "implement batch retry" --container
/kickoff "implement batch retry" --tmux        (explicit fallback)
/kickoff "implement batch retry"               (auto-detect: container if Docker available, else tmux)
```

The kickoff flow changes at step 6 only — instead of `tmux new-session`, it runs `crosslink container start`. Everything else (worktree creation, prompt generation, monitoring, reporting) stays the same.

**Key difference from tmux mode:** The KICKOFF.md prompt tells the lead agent it's inside a task container and can spawn sub-agents by forking `claude` processes directly (no Docker, no tmux). The lead agent has `--dangerously-skip-permissions`, so sub-agents inherit the same. All processes share the container's hooks and policy.

### 5.4 Updated check skill

The `/check` skill gains container awareness:

```
/check                    (auto-detect: checks containers and tmux sessions)
/check feat-batch-retry   (check specific agent by name)
```

For container agents:
- Reads `.kickoff-status` (same sentinel file, same filesystem)
- Shows `docker logs --tail 80` instead of tmux pane capture
- Reports container resource usage (`docker stats --no-stream`)

---

## 6. The Detached Container Mode (Future)

The default mode uses a shared worktree mount. For remote deployment or maximum isolation, a detached mode clones the repo inside the container:

```bash
crosslink kickoff "feature" --detached
```

**Detached flow:**
1. Container clones the repo (or receives a shallow clone)
2. Container creates its own branch
3. Agent works entirely inside the container filesystem
4. On completion, agent pushes the branch to remote (in this mode only, push is allowed)
5. Head Claude on the host fetches the branch and reviews

**Why defer this:** The shared mount model is simpler, faster (no clone step), and supports the human-in-the-loop review workflow naturally (human sees files in real-time). Detached mode is for a different use case — remote/cloud execution where shared filesystems aren't available. It requires:
- Allowing push in the container (different hook config)
- Handling the clone + branch setup
- A pull-based review instead of filesystem-based

This is explicitly out of scope for v1. Noted here so the architecture doesn't preclude it.

---

## 7. Integration with Event-Sourced Coordination

The container model and the event-sourced hub model ([DESIGN-EVENT-SOURCED-COORDINATION.md](DESIGN-EVENT-SOURCED-COORDINATION.md)) are complementary:

| Concern | Hub model handles | Container model handles |
|---------|------------------|------------------------|
| Issue state sync | Event logs + compaction on hub branch | Container runs `crosslink sync` against the hub branch |
| Lock coordination | First-claim-wins via events | Container agent claims locks the same way any agent does |
| Agent identity | `agent.json` + SSH keys | `crosslink init` + `crosslink agent init` inside container |
| Heartbeats | `agents/{id}/heartbeat.json` | Container daemon emits heartbeats (or entrypoint does periodic sync) |
| Permission policy | Hook config on hub branch | Hook config mounted/copied into container |

**Phase 4 of the hub design** (container bootstrap) is subsumed by this design. `crosslink agent bootstrap` already handles the container agent setup:

```bash
# Inside container entrypoint:
crosslink agent bootstrap /workspace $REPO_URL $AGENT_ID --branch $BRANCH
```

For the shared-mount model, the simpler path works:
```bash
# Inside container, worktree already mounted:
crosslink init --force
crosslink agent init $AGENT_ID
crosslink sync
```

The hub branch is the coordination mechanism. The container is the execution environment. They don't overlap.

---

## 8. Dockerfile Embedding and Distribution

### 8.1 Embedded in the binary

Like hooks and slash commands, the Dockerfile and entrypoint script are embedded in the crosslink binary via `include_str!()`:

```rust
// In container.rs or init.rs
const DOCKERFILE: &str = include_str!("../../resources/container/Dockerfile");
const ENTRYPOINT: &str = include_str!("../../resources/container/entrypoint.sh");
```

`crosslink container build` extracts these to a temp directory, copies the running crosslink binary into the build context, and runs `docker build`.

### 8.2 User customization

Users can override the built-in Dockerfile:

```bash
# Use a custom Dockerfile
crosslink container build --dockerfile ./my-agent.Dockerfile

# Or set it in config permanently
# .crosslink/hook-config.json
{
  "container": {
    "dockerfile": ".crosslink/Dockerfile"
  }
}
```

If `.crosslink/Dockerfile` exists, `crosslink container build` uses it instead of the embedded default. This lets users add project-specific toolchains, private registries, custom certificates, etc.

### 8.3 Pre-built images

For CI or teams that don't want to build locally:

```json
{
  "container": {
    "image": "ghcr.io/forecast-bio/crosslink-agent:latest"
  }
}
```

When `container.image` is set, `crosslink container start` pulls the image instead of building. This is the recommended path for teams — build once in CI, use everywhere.

---

## 9. Graceful Fallback

```
crosslink kickoff "feature"
  │
  ├─ Docker available? ──yes──► Container mode (default)
  │
  └─ no
      │
      ├─ tmux available? ──yes──► tmux mode (current behavior)
      │
      └─ no
          │
          └─ Foreground mode (blocking, interactive)
```

**Docker detection:** `docker info` (not just `which docker` — Docker Desktop may be installed but the daemon not running).

**Foreground mode:** As a last resort, run the agent in the current terminal. This is blocking (the human can't do anything else) but works everywhere. Useful for Windows without Docker or single-task workflows.

The fallback is automatic but can be overridden:
```bash
crosslink kickoff "feature" --container   # fail if Docker unavailable
crosslink kickoff "feature" --tmux        # fail if tmux unavailable
crosslink kickoff "feature" --foreground  # always works
```

---

## 10. Scope and Phasing

### Phase 1: Core container execution (v0.5.0)

1. Embed Dockerfile + entrypoint in crosslink binary
2. `crosslink container build` — build the base image with staleness detection
3. `crosslink container start` — start a task container with correct mounts (worktree, .git, hub-cache, credentials-only)
4. Dynamic resource allocation based on host capabilities
5. `crosslink container ps` / `logs` / `stop` / `rm` / `shell` — lifecycle management
6. Update `/kickoff` skill to use containers when Docker is available
7. Update `/check` skill for container awareness
8. Container config section in `hook-config.json`
9. Verify hooks fire correctly inside container (especially `work-check.py` tier 1 blocks)
10. Verify no remote credentials leak into container (defense in depth beyond hooks)

### Phase 2: Swarm and polish (v0.5.x)

11. Sub-agent spawning inside the container (lead agent forks `claude` processes, polls `.kickoff-status`)
12. `crosslink container snapshot` — cache toolchain-installed image
13. Foreground mode fallback (no Docker, no tmux)

### Phase 3: Detached mode (v0.6.0+)

13. `--detached` flag for fully isolated containers with their own clone
14. Push allowance in detached mode (different hook profile)
15. Pull-based review workflow

### Non-goals (for v0.5.0)

- Docker-in-Docker (sub-agents fork processes, not containers)
- Custom network policies (full network access is fine for now)
- GPU passthrough (not needed for code agents)
- Remote Docker hosts (local Docker only)
- Per-agent resource limits (container-level limits cover the swarm)

---

## 11. Design Decisions

### D1: Shared mount, not clone-inside-container

**Decision:** Default mode bind-mounts the worktree into the container.

**Rationale:** The human-in-the-loop review model depends on seeing file changes in real-time. With a shared mount, `git diff` on the host shows what the agent has done at any moment. No sync step, no pull step, no waiting. The `.kickoff-status` sentinel works because it's on the same filesystem.

**Tradeoff:** The agent and host share a filesystem, so a malicious agent could theoretically write outside `/workspace` if Docker's bind mount configuration were wrong. Mitigated by: Docker's default mount isolation, the agent running as a non-root user inside the container, and the human reviewing all changes before pushing.

### D2: Single base image, not per-project Dockerfiles

**Decision:** One crosslink base image with runtime toolchain detection.

**Rationale:** Auto-generating Dockerfiles means maintaining N templates that drift from upstream. A single base image with an entrypoint that installs toolchains on first run is simpler, and the toolchain install cost is a one-time ~60s hit that can be cached.

**Tradeoff:** First container start is slower. Acceptable for a workflow where containers run for minutes to hours.

### D3: Hooks enforce policy, not Docker restrictions

**Decision:** Git mutation blocking is done by crosslink hooks inside the container, not by Docker capabilities or seccomp profiles.

**Rationale:** The hook system already exists, is well-tested, and is the same enforcement mechanism used on the host. Duplicating it at the Docker level adds complexity and a second source of truth. If the hooks are somehow bypassed, the agent still can't push because it doesn't have git credentials for the remote (only the hub branch credentials are configured).

**Tradeoff:** If someone replaces `work-check.py` inside the container, the agent could run blocked commands. Mitigated by: the container image is built from the crosslink binary (trusted), hooks are regenerated by `crosslink init`, and the host's hook-config is mounted read-only.

### D4: `--dangerously-skip-permissions` is the only viable path

**Decision:** Container agents use `--dangerously-skip-permissions`, not `--allowedTools`.

**Rationale:** `--allowedTools` requires enumerating every tool the agent might need, which is brittle and project-dependent. The current kickoff skill already maintains a complex allowedTools string that must be updated when Claude adds new tools or the project adds new conventions. `--dangerously-skip-permissions` with hooks is simpler: allow everything, block only what's dangerous. The hooks are the allowlist/denylist, not the Claude permission flag.

**Tradeoff:** The agent can use any Claude tool (including tools we haven't anticipated). Acceptable because: the container is the blast radius boundary, hooks gate dangerous operations, and the human reviews all output.

### D5: Credentials-only mount + `CLAUDE_CONFIG_DIR` redirect

**Decision:** Mount only `~/.claude/.credentials.json` read-only. Set `CLAUDE_CONFIG_DIR` to a writable directory inside the container. Copy credentials at startup.

**Rationale:** Claude CLI writes extensively to `~/.claude/` during sessions — `history.jsonl`, `session-env/`, `file-history/`, `statsig/`, `telemetry/`, `cache/`, and more. A read-only mount of the full directory causes Claude to crash immediately. Mounting only `.credentials.json` gives the agent auth (Max/Pro subscription) while letting it manage its own ephemeral state.

The copy-on-start pattern means:
- Host credentials stay untouched (read-only mount)
- Token refresh inside the container works (writes to the writable copy)
- Container destruction doesn't corrupt host auth
- No `ANTHROPIC_API_KEY` needed — uses the human's subscription

**Investigated alternative:** Mount all of `~/.claude` read-only with tmpfs overlays on writable subdirs. Too complex — Claude writes to 10+ directories and the list may change between versions.

### D6: One task container, not N agent containers

**Decision:** A single long-lived container hosts all agents for a kickoff task. Multiple `claude` processes run inside it, each in their own worktree.

**Rationale:** A Docker storm of one-container-per-agent adds management overhead (N containers to monitor, N sets of resource limits, N entrypoint runs) for no security benefit. The agents don't need isolation from each other — they're cooperating on the same task. They need isolation from the host. One container provides that boundary for the whole swarm.

Inside the container, crosslink's lock system prevents agents from conflicting, and each agent works in its own worktree. The lead agent can spawn sub-agents by creating worktrees and forking `claude` processes — no Docker-in-Docker needed.

**Tradeoff:** If one agent corrupts the container environment (e.g., installs a bad package), all agents are affected. Acceptable because: the worktree mounts preserve the code on the host, and `crosslink container kill` + restart is fast.

### D7: Container naming convention

**Decision:** Container names follow the pattern `crosslink-task-<slug>` where slug comes from the primary feature.

**Rationale:** The `task-` infix distinguishes task containers from any future per-agent containers. Makes `/check` and `crosslink container ps` output readable. Uniqueness guaranteed by the worktree slug.

### D8: Shared hub cache mount

**Decision:** Mount the host's `.crosslink/.hub-cache/` read-write into the container.

**Rationale:** The hub cache is already designed for concurrent access from multiple worktree agents. The event-sourced model guarantees each agent writes only to its own directory. Compaction uses a lease to prevent concurrent compactors. `git fetch`/`git push` on the hub branch are atomic. Giving the container its own hub cache would mean duplicate fetches, stale state, and a more complex setup — for no safety benefit since the shared model already handles concurrency.

**Investigated alternative:** Each container gets its own hub cache. Cleaner isolation but adds latency (separate git fetches), stale reads, and complexity in the entrypoint. The shared model is simpler and already battle-tested with multiple worktree agents.

### D9: Image staleness detection via binary hash

**Decision:** `crosslink container build` embeds a hash of the crosslink binary as a Docker label. `crosslink container start` compares the label against the running binary's hash and warns (but does not block) if stale.

**Rationale:** Staleness matters because the Dockerfile, entrypoint, and hooks are all embedded in the crosslink binary. An old image means old hooks, which could have policy gaps. But blocking on staleness would be annoying during rapid development — a warning is sufficient. The user can rebuild with `crosslink container build --force` when ready.

```bash
# During build:
docker build --label crosslink-binary-hash=$(sha256sum crosslink | cut -d' ' -f1) ...

# During start:
CURRENT_HASH=$(sha256sum $(which crosslink) | cut -d' ' -f1)
IMAGE_HASH=$(docker inspect --format '{{index .Config.Labels "crosslink-binary-hash"}}' crosslink-agent:latest)
if [ "$CURRENT_HASH" != "$IMAGE_HASH" ]; then
    echo "Warning: container image is stale. Run 'crosslink container build' to update."
fi
```

### D10: Sub-agent management via Claude CLI + polling

**Decision:** The lead agent inside the container spawns sub-agents by running `claude --dangerously-skip-permissions` as background processes. It monitors them by polling `.kickoff-status` sentinel files at regular intervals — the same pattern as the host `/check` skill.

**Rationale:** No process supervisor (s6, supervisord) is needed. The lead agent is a Claude process that can:
1. Create worktrees: `git worktree add` + `crosslink init --force` + `crosslink agent init`
2. Write a `KICKOFF.md` prompt for each sub-agent
3. Fork: `claude --dangerously-skip-permissions -- "$(cat KICKOFF.md)" &`
4. Poll: check `.kickoff-status` files periodically
5. Collect: review each sub-agent's diff when it writes `DONE`
6. Aggregate: combine results, run final integration tests, write its own `DONE`

This reuses the existing kickoff/check patterns. The lead agent is responsible for sub-agent lifecycle — if a sub-agent hangs, the lead agent can kill it and retry or report the failure.

**Inside-container kickoff flow:**
```
Lead agent (claude process 1)
├─ Creates worktree for subtask A
├─ crosslink agent init subtask-a
├─ Writes KICKOFF.md for subtask A
├─ Forks: claude --dangerously-skip-permissions -- "$(cat KICKOFF.md)" &
├─ Creates worktree for subtask B
├─ crosslink agent init subtask-b
├─ Writes KICKOFF.md for subtask B
├─ Forks: claude --dangerously-skip-permissions -- "$(cat KICKOFF.md)" &
├─ Polls .kickoff-status files every ~60s
├─ On completion: reviews diffs, runs integration tests
└─ Writes DONE to its own .kickoff-status
```

### D11: No remote credentials in container (defense in depth)

**Decision:** The container has zero credentials for pushing to `origin`. Only the human's host machine has remote push access.

**How it works:**
- `~/.claude/.credentials.json` (mounted read-only) is Claude API auth, not git remote auth
- The human's SSH key (`~/.ssh/id_*`) is NOT mounted into the container
- The human's git credential helper (`gh auth`) is NOT available inside the container
- Sub-agents get ED25519 signing keys from `crosslink agent init` — these are for signing hub branch commits only, not for remote authentication
- The hub cache worktree has its own local git config with credentials scoped to the `crosslink/hub` branch

**Three layers of push prevention:**

| Layer | Mechanism | What fails |
|-------|-----------|------------|
| 1. Hook | `work-check.py` tier 1 block | Tool call rejected before git runs |
| 2. Credentials | No SSH key or credential helper for origin | `git push` gets authentication error |
| 3. Human gatekeeper | Human pushes from host with their own key | The only path to remote |

Even if an agent somehow bypasses the hook (which requires replacing a file regenerated by `crosslink init`), it still can't push because there are no credentials. The human's key never enters the container.

### D12: Dynamic resource limits based on host capabilities

**Decision:** Detect host resources at container start and allocate generously. No artificial caps — let the machine's full power accelerate compilation and testing.

**Rationale:** Beefier machines should compile faster. Capping a 128GB/32-core workstation at 8GB/4-cores wastes the hardware the user paid for. The container is the only heavy workload running (the host head Claude is mostly idle while waiting), so it should get nearly everything.

**Detection and allocation:**

```bash
# In crosslink container start:
HOST_MEM_KB=$(grep MemTotal /proc/meminfo | awk '{print $2}')
HOST_MEM_GB=$((HOST_MEM_KB / 1024 / 1024))

# Reserve 2GB for host OS + head Claude, give the rest to the container
CONTAINER_MEM_GB=$((HOST_MEM_GB - 2))
if [ $CONTAINER_MEM_GB -lt 4 ]; then
    CONTAINER_MEM_GB=4  # Minimum viable for compilation
fi

# CPU: no limit (Docker default = all cores available)
# The OS scheduler handles fair sharing if the host needs cycles

docker run -d \
  --memory="${CONTAINER_MEM_GB}g" \
  ...
```

**No per-agent limits:** With multiple agents in one container, Docker can't limit individual processes. If one agent triggers a heavy compile, others slow down. This is acceptable — code agent work is bursty, and the slowdown is temporary. The alternative (cgroups v2 nesting) adds complexity for marginal benefit.

---

## 12. Testing Strategy

### Unit tests
- Dockerfile generation from embedded template
- Container name derivation from worktree path
- Mount path construction
- Hook-config merging for container mode
- Fallback detection logic (Docker → tmux → foreground)

### Integration tests
- Build the base image, start a task container, verify crosslink commands work inside
- Verify hooks fire inside container (attempt `git push`, confirm rejection)
- Verify `.kickoff-status` sentinel is visible from host
- Verify credentials mount is read-only, `CLAUDE_CONFIG_DIR` is writable
- Verify hub cache mount works (agent can `crosslink sync` inside container)
- Verify `.git` mount works (`git commit` succeeds, `git push` rejected by hook)
- Container stop/rm lifecycle (all agents inside stop cleanly)
- Multiple claude processes in one container (basic concurrency)

### End-to-end tests
- Full kickoff → container agent → completion → review → push → PR workflow
- Container agent creates crosslink issue, comments, commits (all gated correctly)
- Container agent attempts blocked operations, all rejected by hooks
- Lead agent spawns sub-agent inside container, both complete successfully
- Fallback: kickoff with Docker unavailable falls back to tmux
- Staleness warning: old image + new binary triggers warning on start

---

## 13. Resolved Questions

### Q1: Claude CLI read-only auth directory (RESOLVED → D5)

**Investigated:** Claude CLI writes to 10+ subdirectories in `~/.claude/` during every session (`history.jsonl`, `session-env/`, `file-history/`, `statsig/`, `telemetry/`, `cache/`, `debug/`, `shell-snapshots/`, `paste-cache/`, `plans/`, `tasks/`, `todos/`, etc.). A read-only mount of the full directory crashes Claude immediately.

**Resolution:** Mount only `.credentials.json` read-only. Use `CLAUDE_CONFIG_DIR` env var to redirect all runtime writes to a writable directory inside the container. Copy credentials at startup. See D5.

### Q2: Hub branch access from containers (RESOLVED → D8)

**Investigated:** The hub cache at `.crosslink/.hub-cache/` is already designed for concurrent access — the event-sourced model guarantees per-agent write isolation, and compaction uses a lease.

**Resolution:** Mount the hub cache read-write into the container. Same as host worktree agents sharing the cache today. See D8.

### Q3: Container image updates (RESOLVED → D9)

**Resolution:** Embed a binary hash as a Docker label during build. Compare on start, warn if stale, don't block. See D9.

### Q4: Multiple agents in one container (RESOLVED → D6)

**Resolution:** Yes — one task container hosts all agents for a kickoff. Multiple `claude` processes, each in its own worktree, coordinated by the hub branch. No Docker-in-Docker, no container storm. See D6.

---

## 14. Resolved Questions (continued)

### Q5: Sub-agent process management inside the container (RESOLVED → D10)

**Resolution:** The lead agent uses Claude CLI to spawn sub-agents and checks on them at regular intervals — the same pattern as the current `/check` skill but inside the container. No process supervisor needed. See D10.

### Q6: Git credential scope inside containers (RESOLVED → D11)

**Investigated:** Sub-agents get their own ED25519 signing keys (generated by `crosslink agent init`), used only for signing hub branch commits. The human's SSH key for `git push` to origin is NOT mounted into the container — only `.credentials.json` (Claude auth) is mounted.

**Resolution:** Three layers of defense prevent unauthorized pushes:
1. Hook blocks `git push` (tier 1, always, exit code 2)
2. No remote credentials in the container (human's SSH key stays on host)
3. Human must push from the host using their own key

See D11.

### Q7: Container resource limits for multi-agent (RESOLVED → D12)

**Resolution:** Detect host resources at container start and allocate generously. Beefier machines should use their full power to accelerate compile times. No artificial caps. See D12.

---

## 15. Appendix: Docker Run Command (Reference)

Full `docker run` invocation for a task container (generated by `crosslink container start`):

```bash
# Resource detection (done by crosslink container start)
HOST_MEM_KB=$(grep MemTotal /proc/meminfo | awk '{print $2}')
CONTAINER_MEM_GB=$(( (HOST_MEM_KB / 1024 / 1024) - 2 ))

docker run -d \
  --name crosslink-task-feat-implement-x \
  --label crosslink-agent=true \
  --label crosslink-task=feat-implement-x \
  --label crosslink-issue=42 \
  --memory="${CONTAINER_MEM_GB}g" \
  -v /home/user/project/.worktrees/feat-implement-x:/workspaces/feat-implement-x \
  -v /home/user/project/.git:/repo/.git:rw \
  -v /home/user/project/.crosslink/.hub-cache:/repo/.crosslink/.hub-cache:rw \
  -v /home/user/.claude/.credentials.json:/host-auth/.credentials.json:ro \
  -e AGENT_ID=driver--feat-implement-x \
  -e CLAUDE_CONFIG_DIR=/home/agent/.claude \
  -e CLAUDE_MODEL=opus \
  crosslink-agent:latest \
  claude --dangerously-skip-permissions \
    -- "$(cat /home/user/project/.worktrees/feat-implement-x/KICKOFF.md)"
```

Note: no `--cpus` flag — all host cores are available by default. No SSH keys or git credential helpers are mounted. The human's remote push credentials never enter the container.

Labels enable `crosslink container ps` to show meaningful output without maintaining a separate state file.

### Adding sub-agent worktrees to a running container

When the lead agent inside the container needs sub-agents, the host creates additional worktrees and mounts them into the running container. Alternatively, the worktrees can be pre-created and all mounted at container start:

```bash
# Pre-create multiple worktrees before starting the container:
git worktree add .worktrees/feat-subtask-a feature/subtask-a
git worktree add .worktrees/feat-subtask-b feature/subtask-b

# Mount all of them:
docker run -d \
  --name crosslink-task-feat-implement-x \
  -v .worktrees/feat-implement-x:/workspaces/feat-implement-x \
  -v .worktrees/feat-subtask-a:/workspaces/feat-subtask-a \
  -v .worktrees/feat-subtask-b:/workspaces/feat-subtask-b \
  -v .git:/repo/.git:rw \
  -v .crosslink/.hub-cache:/repo/.crosslink/.hub-cache:rw \
  -v ~/.claude/.credentials.json:/host-auth/.credentials.json:ro \
  ...
```

Or for dynamic worktree creation, mount the entire `.worktrees/` directory:

```bash
docker run -d \
  --name crosslink-task-feat-implement-x \
  -v .worktrees:/workspaces:rw \
  -v .git:/repo/.git:rw \
  -v .crosslink/.hub-cache:/repo/.crosslink/.hub-cache:rw \
  -v ~/.claude/.credentials.json:/host-auth/.credentials.json:ro \
  ...
```

This lets agents inside the container create new worktrees that appear both inside the container and on the host.
