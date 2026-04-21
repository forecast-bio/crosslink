# DESIGN-CROSSLINK-DASHBOARD.md

Multi-project SCADA-style control panel for crosslink.

- **Status**: Draft
- **Issue**: GH #429 (and internal #688)
- **Authors**: @dollspace-gay, @Claude
- **Reviewers**: @maxine-at-forecast (CTO sponsor)
- **Related**: `.design/crosslink-dashboard-*` for iterative refinement

---

## 1. Summary

`crosslink dashboard` is a per-machine control panel that gives its operator
real-time visibility and write authority over every crosslink-managed
repository they have GitHub access to. The target experience is an
industrial control board: a tile grid of projects, colour-coded status
indicators, a global alert stream, embedded terminal sessions for
spawning agents or authoring design docs, and a single pane of glass
for cross-project actions.

One binary (`crosslink dashboard`), one local service per user, no shared
infrastructure. Each user's view is scoped by their own GitHub PAT;
the underlying state is whatever the relevant repos' `crosslink/hub`
branches contain. Writes flow back as signed commits on those branches,
matching crosslink's existing coordination model.

## 2. Goals

- **G1** Single pane showing every crosslink-touched repo the user can read.
- **G2** Real-time (≤5s latency) status visibility across all tracked projects.
- **G3** Full write authority: every mutation available through the CLI must
  be reachable from the panel — issue lifecycle, labels, locks, milestones,
  relations, agent control, and invocation of interactive tools (design,
  kickoff) via an embedded terminal.
- **G4** Alert on any failure / friction signal: stale locks, silent agents,
  overdue issues, CI failures on the hub branch, and any future signals the
  rest of crosslink surfaces.
- **G5** Runs on each user's own machine. No centralised deploy, no SSO
  setup, no per-forecast-employee provisioning. External contributors can
  use it too.
- **G6** Distribution via the existing `cargo install crosslink` path. No
  new package to install.

## 3. Non-goals

- **Not** a centrally hosted multi-tenant service.
- **Not** a Tauri desktop app (deferred; could be layered later).
- **Not** a replacement for `crosslink tui` — the terminal UI stays for
  single-project workflows; the dashboard is the multi-project layer above it.
- **Not** a shipped-to-customers product. Primary users are forecast
  employees and approved collaborators with GitHub access to the relevant
  repos.
- **No** new concurrency model. The hub branch remains source of truth;
  this doc does not introduce a second coordination mechanism.

## 4. User stories

### US-1 — "CTO command post"
As CTO, I open `crosslink dashboard` on my second monitor at the start of the
day. I see every crosslink project at forecast-bio at a glance. A tile
flashes amber — a lock has been stale on repo X for 40 minutes. I click
in, see the stuck agent, force-release the lock and spawn a fresh agent
to retry. All without leaving the panel.

### US-2 — "On-call rotation"
As the on-call engineer, I have the panel open during business hours.
An alert fires (agent silent >10min on a critical issue). I click the
tile, view the agent's last heartbeat and recent commits, use the
embedded terminal to attach to the agent's worktree and inspect.

### US-3 — "Portfolio review"
As a product lead, I want a read-only view of all in-flight issues
across projects — progress toward milestones, overdue items, issues
blocked on external dependencies. I open the panel, filter to
"blocked" and "overdue," export as CSV.

### US-4 — "Design in context"
As a driver, I click "new design doc" on project X's tile. An
embedded terminal session opens running `crosslink design "..."` in
that project's repo. I iterate with Claude; when I `/exit` the
resulting `.design/*.md` appears in the project's recent-activity
feed back on the main panel.

### US-5 — "External contractor"
As an external contractor with access to three forecast repos, I run
`crosslink dashboard` locally. My GitHub PAT scopes the view to just those
three repos. I can take actions on them; I can't see the others.

## 5. Architecture overview

```
┌────────────────────────────────────────────────────────────────────┐
│  User's machine                                                           │
│                                                                           │
│  ┌─────────────────────────────────────────────┐     ┌─────────────────┐  │
│  │  crosslink dashboard (long-lived process)   │     │                 │  │
│  │                                             │◄────┤  Browser (SPA)  │  │
│  │  ┌─────────────────────────────────────┐    │     │                 │  │
│  │  │  Poll loop (every 5s)               │    │     │  React + shadcn │  │
│  │  │  ├─ GitHub org enumeration          │    │     │  + xterm.js     │  │
│  │  │  ├─ git fetch each hub              │    │     │                 │  │
│  │  │  └─ diff -> alerts + index          │    │     └─────────────────┘  │
│  │  └─────────────────────────────────────┘    │                          │
│  │                                             │                          │
│  │  ┌─────────────────────────────────────┐    │                          │
│  │  │  SQLite (~/.crosslink/dashboard.db) │    │                          │
│  │  │  projects, alerts, sessions,        │    │                          │
│  │  │  hub_shas, activity                 │    │                          │
│  │  └─────────────────────────────────────┘    │                          │
│  │                                             │                          │
│  │  ┌─────────────────────────────────────┐    │                          │
│  │  │  HTTP API + WebSocket               │    │                          │
│  │  │  + PTY broker (xterm.js)            │    │                          │
│  │  └─────────────────────────────────────┘    │                          │
│  │                                             │                          │
│  │  ┌─────────────────────────────────────┐    │                          │
│  │  │  Write queue: shell out to          │    │                          │
│  │  │  `crosslink -C <repo> ...`          │    │                          │
│  │  │  + git commit/push on hub           │    │                          │
│  │  └─────────────────────────────────────┘    │                          │
│  └─────────────────────────────────────────────┘                          │
│                  ▲                                                        │
└──────────────────┼────────────────────────────────────────────────────────┘
                   │ git fetch / push (https, user's creds)
                   │ GitHub API (PAT)
                   ▼
              ┌─────────────────────────────┐
              │  forecast-bio org + mirrors │
              │  (source of truth for every │
              │   tracked project's hub)    │
              └─────────────────────────────┘
```

Layers, inside-out:

1. **GitHub org enumeration** — at startup and on a slow cadence, the poll
   loop lists repos the user's PAT can see. A repo is *tracked* if its
   default branch has a `crosslink/hub` ref (or the user explicitly added
   it to the config).
2. **Per-repo sync** — for each tracked repo, maintain a shallow clone
   under `~/.crosslink/dashboard-cache/<owner>/<repo>/`. Every 5s, `git fetch
   origin crosslink/hub` in each clone. If the SHA advanced, diff and
   emit events.
3. **Aggregator index (SQLite)** — projects, their last known hub SHA,
   derived aggregate state (counts, statuses), open alerts, running
   terminal sessions, write-action audit log.
4. **API + WebSocket** — frontend pulls the current view via REST, subscribes
   to live changes via WS. PTY broker is a separate WebSocket endpoint
   (`/ws/pty/<session_id>`) for xterm.js terminal streams.
5. **Write path** — user clicks a control → frontend POSTs → aggregator
   shells out to `crosslink -C <repo_clone> <subcommand>` (or issues direct
   git commits on the hub branch for things that don't map to CLI) →
   pushes → updates index → broadcasts change.
6. **Frontend** — React SPA served by the same binary on the same port.
   React Router client-side routing for the multi-page feel.

## 6. Data model

SQLite schema in `~/.crosslink/dashboard.db`. Version-gated via `PRAGMA
user_version`.

```sql
-- Tracked repositories
CREATE TABLE projects (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    slug TEXT NOT NULL UNIQUE,          -- "forecast-bio/crosslink"
    clone_path TEXT NOT NULL,           -- local clone location
    default_branch TEXT NOT NULL,       -- usually "main"
    hub_sha TEXT,                       -- last fetched crosslink/hub tip
    hub_fetched_at TEXT,
    status TEXT NOT NULL DEFAULT 'active',  -- active, paused, error
    added_at TEXT NOT NULL,
    last_activity_at TEXT,
    pinned INTEGER NOT NULL DEFAULT 0   -- user-pinned to top of grid
);

-- Materialised aggregate state per project (for fast tile rendering)
CREATE TABLE project_state (
    project_id INTEGER PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    open_issues INTEGER NOT NULL DEFAULT 0,
    overdue_issues INTEGER NOT NULL DEFAULT 0,
    due_soon_issues INTEGER NOT NULL DEFAULT 0,
    blocked_issues INTEGER NOT NULL DEFAULT 0,
    active_agents INTEGER NOT NULL DEFAULT 0,
    stale_locks INTEGER NOT NULL DEFAULT 0,
    ci_status TEXT,                     -- passing, failing, unknown
    updated_at TEXT NOT NULL
);

-- Active alerts (not per-user — per-project, derived from state)
CREATE TABLE alerts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,                 -- stale_lock, silent_agent, overdue_issue, ci_failure
    severity TEXT NOT NULL,             -- info, warning, critical
    subject_ref TEXT,                   -- "issue#12", "agent:jus4", "lock:3", etc.
    detail TEXT,                        -- human-readable
    opened_at TEXT NOT NULL,
    resolved_at TEXT,
    acknowledged_at TEXT,               -- local dismissal only
    acknowledged_by TEXT
);

-- Running terminal sessions for xterm.js
CREATE TABLE pty_sessions (
    id TEXT PRIMARY KEY,                -- uuid
    project_id INTEGER REFERENCES projects(id) ON DELETE SET NULL,
    command TEXT NOT NULL,              -- "crosslink design ..."
    started_at TEXT NOT NULL,
    ended_at TEXT,
    exit_code INTEGER,
    pid INTEGER                         -- nullable after process exits
);

-- Write-action audit log (what the dashboard did, on behalf of whom)
CREATE TABLE actions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id INTEGER REFERENCES projects(id),
    actor TEXT NOT NULL,                -- driver fingerprint
    verb TEXT NOT NULL,                 -- close_issue, claim_lock, kill_agent, ...
    subject TEXT,
    payload_json TEXT,
    requested_at TEXT NOT NULL,
    completed_at TEXT,
    outcome TEXT,                       -- success, failed, partial
    error TEXT
);

-- Activity stream (compacted events from hub branches, for drill-down feed)
CREATE TABLE activity (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    at TEXT NOT NULL,
    author TEXT,                        -- agent_id or driver fingerprint
    kind TEXT NOT NULL,                 -- issue_opened, comment_added, lock_claimed, ...
    subject_ref TEXT,
    summary TEXT
);

-- Config values persisted across restarts
CREATE TABLE config (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE INDEX idx_alerts_project ON alerts(project_id) WHERE resolved_at IS NULL;
CREATE INDEX idx_activity_project_at ON activity(project_id, at DESC);
CREATE INDEX idx_actions_project ON actions(project_id, requested_at DESC);
```

## 7. API surface

All endpoints bearer-token auth'd (same pattern as `crosslink serve`).
Default bind: `127.0.0.1:4000`.

### REST

```
GET    /api/v1/projects                → list of tracked projects + summary state
GET    /api/v1/projects/{slug}          → single project detail (issues, agents, locks)
POST   /api/v1/projects                 → add a repo to tracking
DELETE /api/v1/projects/{slug}          → stop tracking
PATCH  /api/v1/projects/{slug}          → { pinned, status } updates

GET    /api/v1/alerts?open=true         → all active alerts across projects
POST   /api/v1/alerts/{id}/ack          → locally dismiss an alert

GET    /api/v1/activity?limit=N         → global recent activity feed
GET    /api/v1/projects/{slug}/activity → project-scoped feed

# Control surfaces — each mirrors a CLI command
POST   /api/v1/projects/{slug}/issues                    → create
PATCH  /api/v1/projects/{slug}/issues/{id}               → update
POST   /api/v1/projects/{slug}/issues/{id}/close
POST   /api/v1/projects/{slug}/issues/{id}/reopen
POST   /api/v1/projects/{slug}/issues/{id}/comment
POST   /api/v1/projects/{slug}/issues/{id}/relate
POST   /api/v1/projects/{slug}/issues/{id}/block
POST   /api/v1/projects/{slug}/labels/attach
POST   /api/v1/projects/{slug}/locks/{id}/claim
POST   /api/v1/projects/{slug}/locks/{id}/release
POST   /api/v1/projects/{slug}/locks/{id}/steal
POST   /api/v1/projects/{slug}/agents/{agent_id}/request  → git-native kill/pause/resume
POST   /api/v1/projects/{slug}/milestones
...

# Terminal sessions (for design/kickoff workflows)
POST   /api/v1/pty                       → spawn { project_slug, command } → session_id
GET    /api/v1/pty                       → list active sessions
DELETE /api/v1/pty/{session_id}          → kill

# Config
GET    /api/v1/config
PATCH  /api/v1/config                    → { org, poll_interval_secs, alert_thresholds, ... }

# Discovery
POST   /api/v1/discovery/scan            → immediate org enumeration pass
GET    /api/v1/discovery/candidates      → crosslink-touched repos not yet tracked
```

### WebSocket

```
/ws                   → live updates: project state changes, new alerts,
                        new activity. Frontend subscribes once at page load.
/ws/pty/{session_id}  → bidirectional byte stream for xterm.js. See §9.
```

WS messages over `/ws` are thin notifications — the frontend refetches the
affected REST resource on receipt. Avoids maintaining a parallel push data
channel; keeps the server simple.

## 8. Frontend information architecture

Keep the React + Vite + shadcn + recharts stack. Redesign the IA
completely around multi-project; don't try to adapt what's in `dashboard/`
today.

### Top-level layout

```
┌─ header (forecast logo, global status line, user avatar, settings) ─┐
│                                                                     │
├─ alert banner (if any active critical alerts) ──────────────────────┤
│                                                                     │
├─ sidebar ──────────┬─ main area ──────────────────────────────────── │
│                    │                                                │
│  Projects          │  ┌──── project tile grid (default view) ─────┐ │
│  - [pinned]        │  │                                           │ │
│  - [active]        │  │  ┌───────┐  ┌───────┐  ┌───────┐          │ │
│  - [archived]      │  │  │ proj1 │  │ proj2 │  │ proj3 │          │ │
│                    │  │  │ ●●○○○ │  │ ●●●●● │  │ ●●●●○ │          │ │
│  Alerts            │  │  │ 3 ovr │  │ OK    │  │ 2 stk │          │ │
│  (N active)        │  │  └───────┘  └───────┘  └───────┘          │ │
│                    │  │                                           │ │
│  Activity          │  │  ...                                      │ │
│                    │  └───────────────────────────────────────────┘ │
│  Terminals         │                                                │
│  (N running)       │  OR drill-down detail panel for selected proj  │
│                    │                                                │
│                    │  OR alerts page, activity page, etc.           │
└────────────────────┴────────────────────────────────────────────────┘
```

### Pages / routes

- `/` — **Project grid** (home). Tile per project, live-updating.
- `/project/:slug` — **Project detail**. Issues, agents, locks, activity.
- `/project/:slug/issue/:id` — issue-level drill-down.
- `/alerts` — Global alert stream; ack / resolve from here.
- `/activity` — Cross-project event stream (infinite scroll).
- `/terminals` — **Terminal session list**. Click any running session to attach.
- `/terminals/:id` — **Terminal attach view**. Full-screen xterm.js.
- `/settings` — Config: poll interval, alert thresholds, PAT management,
  tracked repo list editor, discovery scope.

### Tile anatomy

Each project tile is a compact status summary:

- Top strip: colour (green/amber/red) + project slug + pin toggle.
- Middle: key counts — open issues, overdue, blocked, active agents.
- Bottom strip: most recent activity line + timestamp.
- Hover: quick actions (view, create issue, open terminal).

Sort options: by most-recent-activity (default), by alert severity, by slug.

### SCADA conventions

- **Colour semantics**: green = nominal; amber = warning (one or more
  non-critical alerts); red = critical alert open; grey = paused/unreachable.
- **Dense tiles, not cards**: prefer at-a-glance density over whitespace.
- **Always-visible alert badge** in header with count.
- **Audible alert** (optional, config-gated): short beep on critical open.

## 9. Git-native agent control protocol

Crosslink already coordinates agents via the hub branch. This design
extends that pattern with *agent request files*. Dashboard writes a request,
target agent reads and acts on it.

### On-disk shape (on `crosslink/hub`)

```
agents/
  <agent_id>/
    heartbeat.json       # (existing, V2) — last-seen timestamp, status
    requests/
      <request_id>.json  # pending request
      <request_id>.ack.json  # written by agent on completion
```

### Request schema

```json
{
    "request_id": "01HXYZ...",         // ulid, lexicographic order
    "kind": "kill",                    // kill | pause | resume | reprioritise
    "subject": { "issue_id": 42 },     // optional, kind-specific
    "requested_by": "SHA256:driver...",
    "requested_at": "2026-04-20T18:30:00Z",
    "reason": "stuck >40min on #42"
}
```

Commit is signed by the driver key (same as any other dashboard write).

### Ack schema

```json
{
    "request_id": "01HXYZ...",
    "ack_at": "2026-04-20T18:30:05Z",
    "acted": true,
    "result": "killed",
    "notes": "agent terminated cleanly"
}
```

### Agent-side handling

Agents already poll their own `heartbeat.json` to emit liveness. This
design adds: on every sync tick, scan `agents/<self>/requests/` for
unacknowledged requests (no matching `.ack.json`). For each:

1. Validate the request signature is by a known driver fingerprint.
2. Execute the action (kill → exit after current tool use; pause →
   write a pause flag; etc.).
3. Write the ack file, commit, push.

Implementation ships as a library function (`agent::poll_requests`) the
existing agent loop calls once per tick. No new long-running process.

### Dashboard-side rendering

The dashboard displays request status by diffing `requests/` vs
`requests/*.ack.json` — pending requests render as "awaiting agent
ack (5s ago)", acknowledged ones as their result.

### Non-goals for the protocol

- Not an RPC system. One-shot declarative requests only.
- Not a push channel — agents poll on the same cadence they already do.
- No authentication beyond the existing allowed-signers store. Requests
  from untrusted signers are ignored.

## 10. Embedded terminal (xterm.js + PTY broker)

`crosslink dashboard` hosts Claude Code sessions and other interactive crosslink
commands inside an xterm.js terminal embedded in the panel.

### Lifecycle

1. User clicks "new design doc" on project X's tile.
2. Frontend POSTs `/api/v1/pty { project_slug: "...", command: "crosslink design" }`.
3. Server spawns a PTY running that command via `portable-pty`:
   - cwd = project's clone path
   - env includes `CROSSLINK_DASHBOARD=1` so child processes can detect this context
   - Returns `{ session_id: "..." }`.
4. Frontend opens `ws://<host>/ws/pty/<session_id>`.
5. Bidirectional byte stream:
   - Client → server: `stdin` frames (keystrokes, pastes), `resize` frames
     (rows/cols on window resize).
   - Server → client: `stdout` frames (raw PTY bytes, terminal ANSI
     sequences intact).
6. User closes tab → WS disconnects. Server holds the PTY open for a
   configurable grace period (default 30 min), letting the user reconnect
   from /terminals.
7. User runs `/exit` or the command finishes. Server records exit code,
   closes the PTY, keeps the session row for audit with its final state.

### Wire format

JSON frames over WS for control, binary frames for data. Simpler than a
custom multiplexing protocol; xterm.js handles the raw bytes directly.

```
// Client -> server
{ "type": "stdin", "data": "..." }           // base64-encoded bytes
{ "type": "resize", "rows": 40, "cols": 140 }

// Server -> client
{ "type": "stdout", "data": "..." }          // base64-encoded bytes
{ "type": "exit", "code": 0 }
```

### Concurrency & resource limits

- Max concurrent PTY sessions per user: config, default 8.
- Each session is a child process — same user privileges as the aggregator.
- No isolation beyond the normal OS process model. This is the user's
  own machine running their own code; no sandboxing is required.

### Security

- Same bearer-token auth as REST. WS upgrade requires a valid token.
- Server binds to `127.0.0.1` by default. For remote access users must
  SSH-tunnel — matches `crosslink serve`'s current posture.
- A flag (`--bind 0.0.0.0`) exists for advanced users, with a startup
  warning that it exposes the PTY broker and all controls to the network.

## 11. Alerts

Alert types (MVP):

| Kind | Trigger | Default severity |
|---|---|---|
| `stale_lock` | Lock held > N minutes without a heartbeat | warning |
| `silent_agent` | Agent heartbeat >M minutes stale while holding a lock | critical |
| `overdue_issue` | Open issue with `due_at < now` | warning |
| `ci_failure` | Most recent commit on `crosslink/hub` shows CI failing | warning |
| `unreachable_project` | git fetch failing for > K cycles | warning |
| `signature_invalid` | Invalid signature on recent hub commit | critical |

Thresholds (`N`, `M`, `K`) are user-configurable via `/settings`, with
sensible defaults.

### Delivery

- In-dashboard: badge + banner + sound (sound config-gated).
- Desktop notifications: OS-native via web `Notification` API
  (permission-prompted once on first critical alert).
- Webhook integrations (Slack / Discord / email): deferred to phase 2.

### Per-user vs per-project

Alerts live in the local SQLite. Different users tracking the same repo
each have their own local alert history and ACK state. This is a
deliberate simplicity choice — shared ACK state would require a
central server.

## 12. Distribution & deployment

- **Subcommand**: `crosslink dashboard`. No new crate, no separate binary.
- **Frontend assets**: bundled into the binary at build time via
  `rust-embed` or `include_dir!`. Build pipeline:
  1. `npm --prefix dashboard ci && npm --prefix dashboard run build`
  2. `cargo build` picks up `dashboard/dist/` via the embed macro.
- **CI gate**: the crosslink CI workflow runs `dashboard/` typecheck +
  tests before proceeding to `cargo build`. A broken frontend fails
  the same pipeline as a broken backend — prevents the type-drift
  class of failure that caused #429 in the first place.
- **Release coordination**: single crate, single artifact. Every
  `cargo install crosslink` ships the dashboard. No need for users to
  build anything themselves. This is the direct resolution for #429 —
  there is no longer a world in which `cargo install` users face an
  empty 404 page, because the bundled assets ship with the binary.
- **First-run**: on first `crosslink dashboard` invocation, if no GitHub PAT
  is configured, the server prints a URL showing how to set one via
  `/settings`. The dashboard is usable in read-only "what repos do I have?"
  mode without a PAT, but discovery is best-effort.

## 13. Open questions

### Q1 — Should tracked-repo clones live next to existing repos, or in a dedicated cache?

Option A: reuse user's existing clones (user passes a local path).
Option B: maintain our own shallow clones in `~/.crosslink/dashboard-cache/`.

Pro-A: no duplicate disk usage, picks up the user's existing
   work-in-progress, AND — the decisive factor — the write surface
   can shell out to the real `crosslink` CLI in the user's
   already-initialised workspace (agent identity, driver signing key,
   hub-cache worktree all set up by their normal `crosslink init`
   flow). No second copy of that machinery to maintain.
Pro-B: isolated state, never conflicts with active user work, always
   guaranteed fetchable state of `crosslink/hub`.

**Resolution**: **A** (reuse existing clones). B was the original
proposal, but it forced us into one of three architecturally ugly
positions for the write surface: re-mint each cache clone as a
crosslink workspace on track, duplicate the CLI's write logic in the
dashboard, or teach the CLI a new `--hub-dir` flag. A cuts through
all three.

Mechanically: `crosslink dashboard track <path>` takes a path to the
user's existing local working copy of a crosslink-managed repository.
The slug is derived from `git remote get-url origin` (override with
`--slug owner/repo`). `untrack` removes the DB row only — the user's
working copy is never touched. Poll-loop `git fetch` runs in that
same workspace; write operations (P1.8+) shell out to `crosslink` via
`Command::new("crosslink").current_dir(clone_path)`.

### Q2 — How does the write path handle conflicts?

Aggregator shells out to `crosslink -C <clone> issue close 42`. If the
hub branch has advanced remotely since the last fetch, the subsequent
push will be rejected. `crosslink` today handles this internally by
rebasing and retrying. Should the aggregator mirror that or surface the
conflict to the user?

**Proposal**: mirror the CLI behaviour (automatic retry within
`write_commit_push`'s existing retry budget, expose the final outcome
in the audit log).

### Q3 — What happens when the GitHub PAT rate limit is hit during discovery?

5000 req/hr for classic PATs, 1000/hr for fine-grained. With 50+ repos
and a 5-minute org-enumeration cadence, we're fine. With the per-repo
5-second fetch, we're doing git-over-https, not API — no rate limit
concerns there.

**Proposal**: no special handling needed in MVP. Expose rate-limit
status in `/settings` for operator awareness.

### Q4 — Should the aggregator persist terminal scrollback?

Currently only a live PTY stream. On reconnect after detach, the user
sees only bytes emitted after reconnection. For long design sessions
this loses context.

**Proposal**: ring buffer in memory (configurable size, default 5000
lines). Replayed to the xterm.js client on reattach. Not written to
disk — ephemeral to aggregator process lifetime.

### Q5 — How are failed PTY exits surfaced?

Command exits non-zero → session row records exit code → UI shows red
dot on /terminals. Do we auto-spawn an alert for these?

**Proposal**: no. PTY exits are user-initiated; failures are their
responsibility to notice. An alert here would be noisy.

### Q6 — Does the dashboard's own state (dashboard.db) need to be backed up / portable?

It's recoverable — every piece of durable state re-derives from the
hub branches on next sync. Activity history and audit log, however, are
purely local.

**Proposal**: no backup logic in MVP. Document that `dashboard.db` is
ephemeral per-machine state; users wanting a long audit trail should
rely on the hub branch's git history.

## 14. Phased rollout

> **Note on PR shape**: the project owner elected to ship Phases 1–3
> (and as much of 4–5 as falls out naturally) on a *single* PR rather
> than stacking one PR per phase. The "phased" framing below describes
> the logical milestones inside that one PR, not separate reviews.
> Commit boundaries within the branch (`git log feat/429-crosslink-
> dashboard`) preserve the ordering for reviewers who want to walk it
> progressively.


### Phase 1 — Read-only MVP (2-3 PRs)

- Subcommand scaffolding, bound to 127.0.0.1, bearer-token auth
- Discovery via manual config (no GitHub enumeration)
- Poll loop, SQLite index, 5-second fetch cadence
- Frontend: project grid (read-only), per-project detail page
- Alerts (`stale_lock`, `overdue_issue`, `ci_failure`)
- WebSocket live updates
- Embed existing `dashboard/` stack with the new IA

### Phase 2 — Write surface

- Close/reopen issues, labels, milestones, relations ✓ (P1.8/P1.9)
- Lock claim/release/steal ✓ (P1.10)
- Audit log ✓ (`actions` table; written by `run_cli` primitive)
- Git-native agent control protocol (`agents/.../requests/`) ✓ (P1.11)
  - Driver-side write: `crosslink agent request`, dashboard REST
    endpoint, React UI with per-agent request drawer
  - Agent-side poll: `crate::agent_flags` (pause/kill/reprioritise
    local flags), `crate::agent_requests::poll::process_pending`,
    `crosslink agent poll-requests` CLI, auto-integrated into
    `crosslink sync` so every sync tick processes requests
- Desktop notifications **(deferred to Phase 3)**

### Phase 3 — Interactive terminal

- PTY broker with xterm.js
- Session persistence across tab closes
- Design-doc / kickoff launch flows
- Terminal list + attach page

### Phase 4 — Discovery

- GitHub org enumeration
- Auto-track crosslink-touched repos
- PAT management UI

### Phase 5 — Polish

- Webhook alerting (Slack/Discord/email)
- Theme / audible alerts / custom dashboards
- Export (CSV, JSON, screenshots for status reports)

## 15. Success criteria

- **SC-1** A new `crosslink dashboard` user, starting from `cargo install
  crosslink`, gets a working panel showing at least one tracked repo
  within 2 minutes.
- **SC-2** Time from a lock going stale to an alert appearing on the
  panel: ≤10 seconds.
- **SC-3** Every mutation currently possible through `crosslink` CLI is
  reachable from the panel (full CLI parity for writes).
- **SC-4** A design-doc session launched from the panel produces a
  `.design/*.md` on the target repo's working tree, identical to
  launching `crosslink design` from a terminal.
- **SC-5** The panel can track 50+ projects with poll loop CPU overhead
  under 5% sustained and ≤50 MB resident memory.

## 16. Appendix: Why not Tauri / separate repo / scratch frontend?

- **Tauri**: Primary objection is distribution overhead (per-OS signing,
  notarization, auto-update). A web dashboard served by the existing
  `crosslink` binary reuses the already-working install path. If
  cross-monitor native-window polish becomes a real requirement, Tauri
  can wrap the same React frontend later — no rework needed.
- **Separate repo**: Worst of every axis — drift (frontend types vs Rust
  types), release coordination, user friction. This is explicitly the
  thing we're trying to *avoid* because it's what bit #429 in the first
  place.
- **Start from scratch (`npm create vite@latest`)**: We'd redo a week of
  setup work — Vite config, TypeScript baseline, shadcn registry, auth
  bootstrap flow, theme tokens — for no upside. The existing `dashboard/`
  directory has all that already working; what's broken is only the
  single-project IA built on top of it.

## 16a. What we keep vs rewrite from the existing `dashboard/`

The current `dashboard/` is ~60% reusable scaffolding, ~40% dead weight
from the single-project IA. The scaffolding PR in Phase 1 (P1.1)
prunes the dead weight in place rather than starting a new directory.

**Keep (untouched or lightly edited)**
- `package.json`, `vite.config.ts`, `tsconfig.*`, `components.json`
- `src/components/ui/*` — shadcn primitives (Button, Dialog,
  DropdownMenu, Tooltip, etc.). Generic, reused directly.
- `src/lib/*` — utility functions (className merging, date formatting).
- `public/` — static assets.
- Bearer-token auth bootstrap (the `?token=X` → sessionStorage flow).

**Delete**
- `src/pages/*` — every existing page. The new routes (grid, project
  detail, alerts, terminals, settings) are different enough to rebuild.
- `src/stores/*` — existing zustand stores are single-project-scoped;
  multi-project needs a different shape.
- `src/components/UsageChart.tsx` — recharts type drift; not
  required by the CTO use case for MVP.
- `src/pages/Agents.tsx` — `AgentSummary.id` drift; agents live inside
  project drill-downs in the new IA.

**Add**
- `ts-rs` (or similar) integration — Rust crate emits `.d.ts` at build
  time; frontend consumes it. Prevents future API-contract drift.
- `xterm.js` dependency and a `src/components/Terminal.tsx` wrapper.
- WebSocket client with multi-project subscription logic.
- New multi-project state architecture (see §8).

**Net effect**: `npm --prefix dashboard run build` compiles cleanly
after P1.1 with a minimal bundle containing shadcn primitives and
nothing else. Every subsequent PR adds pages and components on that
clean foundation.

## 17. Deprecation of `crosslink serve`

The existing `crosslink serve` subcommand is the surface exposed by
#429: it runs an HTTP server and accepts `--dashboard-dir <path>`,
but ships with no dashboard for that flag to point at — hence the 404
that filed the bug. With `crosslink dashboard` as the blessed entry
point, `serve` is redundant.

### Deprecation plan

1. **First release that ships `crosslink dashboard`**
   - `crosslink serve` is retained, but prints a warning to stderr on
     every invocation:
     ```
     warning: `crosslink serve` is deprecated. Use `crosslink dashboard`
     instead. See https://github.com/forecast-bio/crosslink/issues/429.
     This alias will be removed in crosslink 0.7.
     ```
   - `crosslink serve` behaviour: dispatch through to the same handler
     as `crosslink dashboard`, honouring the existing `--dashboard-dir`
     flag (which still works — the new bundled-at-build dashboard is
     just the default when no flag is given).
   - `--dashboard-dir` itself is also deprecated: with
     `rust-embed`-bundled assets it's no longer necessary.
2. **Second release**
   - `crosslink serve` disappears entirely. Shell users calling it
     see `error: unrecognised subcommand`.
   - Release notes link to #429 as the canonical migration target.
3. **Documentation**
   - `guides/web-dashboard.html` becomes the `crosslink dashboard`
     page; `crosslink serve` mentions are replaced or deleted.
   - Closes #429 as resolved when the first release lands.

### Implementation in P1.1 (scaffolding)

The serve → dashboard rename is a single-commit change: rename the
handler, add the deprecation-warning wrapper that keeps `serve` working
for one more release cycle. Both clap subcommands dispatch to the same
`run_dashboard_server(opts)` function under the hood.
