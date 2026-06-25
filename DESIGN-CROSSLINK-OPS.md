# DESIGN-CROSSLINK-OPS.md

Operator's runbook for `crosslink dashboard`.

- **Status**: Initial (companion to [DESIGN-CROSSLINK-DASHBOARD.md](DESIGN-CROSSLINK-DASHBOARD.md))
- **Issue**: GH #429 (followup — internal #688)
- **Audience**: anyone running `crosslink dashboard` on their own machine
  (Forecast employees, approved collaborators, self-hosted end users)
- **Scope**: how to install, configure, operate, troubleshoot, and
  upgrade the dashboard. The companion design doc says **why** things
  work the way they do; this doc says **how to operate** the deployed
  thing.

---

## 1. Overview for operators

One crosslink binary. One per-user local service. No centralised
deployment. Each user's panel is scoped by their own GitHub PAT.

```
┌──────────────────────────────────────────────────────────────┐
│ user machine                                                 │
│                                                              │
│   ~/.crosslink/dashboard.db    ← per-user SQLite index       │
│         ▲                                                    │
│         │ 5 s poll            ┌──────────────────────────┐   │
│   ┌─────┴─────┐   git fetch   │ tracked workspaces       │   │
│   │ crosslink │ ─────────────▶│   ~/work/foo, ~/code/bar │   │
│   │ dashboard │               │   (user's real clones,   │   │
│   │  serve    │◀──────────────│   not private copies)    │   │
│   └─────┬─────┘  HubSnapshot  └──────────────────────────┘   │
│         │                                                    │
│         │ http://127.0.0.1:3100  (loopback-only)             │
│         ▼                                                    │
│   ┌───────────┐                                              │
│   │ browser   │  ?token=…  → sessionStorage → Bearer header  │
│   └───────────┘                                              │
│                                                              │
│   outbound: git fetch (ssh / https), optional GitHub REST    │
│             via stored PAT, optional alert webhooks          │
└──────────────────────────────────────────────────────────────┘
```

Key facts:

- **Binding**: `127.0.0.1:3100` by default. Loopback-only. Not
  routable off the host.
- **Auth**: a random 32-char hex token is generated at startup and
  printed once on stdout. Every request on `/api/*` (except
  `/api/v1/health`) and `/ws` must carry it.
- **State**: all state lives in one SQLite file at
  `~/.crosslink/dashboard.db`. Deleting the file resets everything
  except your tracked git clones, which the dashboard doesn't own.
- **Writes**: shell out through the real `crosslink` CLI in each
  tracked workspace. The dashboard never does its own git mutations.

---

## 2. Installation

```bash
cargo install crosslink
```

That's it. The dashboard frontend is embedded in the binary via
`rust-embed`; there is no separate `npm install` for operators.

Requirements:

- Rust toolchain to build from crates.io (`cargo install`) on first
  setup — a release binary isn't shipped on every target today.
- `git` on `$PATH` — the poll loop shells out to it for `git fetch`
  and signature verification.
- A modern browser with WebAudio support if you plan to enable
  audible alerts (Phase 5.3). Chromium-family and recent Firefox are
  fine; Safari ≥14.

Verifying the install:

```bash
crosslink --version                   # print version, exit 0
crosslink dashboard --help            # shows Serve / Track / Untrack / List
```

---

## 3. First-run setup

### 3.1 Start the server

```bash
crosslink dashboard serve
# crosslink dashboard: listening on http://127.0.0.1:3100
#   Dashboard: http://127.0.0.1:3100/?token=91ab…d7cf
#   API:       http://127.0.0.1:3100/api/v1/health
#   WebSocket: ws://127.0.0.1:3100/ws
#   Auth:      Bearer 91ab…d7cf
```

Open the printed `Dashboard: …?token=` URL. The frontend strips the
token out of the URL after first load and persists it to
`sessionStorage` for that tab — reloads in the same tab reuse the
stored token without re-pasting.

**Token lifetime**: regenerated every time the server starts. If you
restart `crosslink dashboard serve`, all currently-open tabs will
401; use the freshly-printed `?token=…` URL to re-auth.

### 3.2 Track your first project

Easiest path — track a repo you've already cloned:

```bash
crosslink dashboard track /home/you/work/my-org/my-repo
```

The dashboard reads the repo's `origin` remote to derive the slug
(`my-org/my-repo`); override with `--slug` if you need something
custom. The tile appears in the grid within one poll tick (≤5 s).

### 3.3 (Optional) Wire up GitHub discovery

Navigate to **Settings → GitHub**.

1. Paste a GitHub PAT with `repo` scope. Fine-grained PATs work too —
   they need Contents: read and Metadata: read on each org you plan
   to enumerate.
2. Set your default org (e.g. `forecast-bio`).
3. Click **Browse `<org>`**. The dashboard walks the org via the
   GitHub REST API and returns every repo that already has a
   `crosslink/hub` branch.
4. Click **Track all** to clone + track the lot in one shot. Each
   repo lands at `~/<repo>` — flat, next to your manual clones like
   `~/crosslink` and `~/ferrotorch`. The filesystem-discover walker
   picks them up on subsequent runs. Pass a clone-root override if
   you want them grouped under a subdirectory
   (e.g. `~/code/<repo>`).

The token is stored AES-256-GCM encrypted in
`~/.crosslink/dashboard.db`, keyed to the machine — see §7 Security.

### 3.4 (Optional) Turn on outbound alerting

**Settings → Webhooks.** Paste one or more URLs; payload shape is
auto-detected from the host:

| URL pattern                          | Payload shape                     |
|--------------------------------------|-----------------------------------|
| `hooks.slack.com/…`                  | Slack Block Kit                   |
| `discord.com/api/webhooks/…`         | Discord native `{content, embed}` |
| anything else                        | generic JSON                      |

URLs must be `https://…` (`http://127.0.0.1` is permitted for local
bridges). A single bad URL rejects the whole batch with no partial
write — validate one at a time if you want granular feedback.

### 3.5 (Optional) Tune personal UI

**Settings → Preferences.** Theme (System / Dark / Light), audible
alert toggle, per-severity filter. Preferences persist in
`localStorage` — per-browser, per-tab-origin, never synced.

---

## 4. Daily operations

### 4.1 What the grid shows

One tile per tracked repo, sorted pinned-first then alphabetical.
Each tile surfaces:

- `open_issues`, `overdue_issues`, `due_soon_issues`, `blocked_issues`
- `active_agents` (last heartbeat within 10 min)
- `stale_locks` (held longer than 60 min)
- CI status (derived from `meta/ci-status.json` on the hub branch)
- last hub activity timestamp

Tiles refresh within one poll tick of the underlying state. A
WebSocket push arrives immediately when the 5-second poll cycle
completes a project — no wait for the next full tick.

### 4.2 Reading alerts

Global severity-sorted view at `/alerts`. Alerts reconcile every
tick: a derived alert that disappears from the snapshot gets
`resolved_at` set; one that appears opens a new row.

Supported alert kinds (design doc §11 + the implementation):

| Kind                   | Severity | Trigger                                       |
|------------------------|----------|-----------------------------------------------|
| `stale_lock`           | warning  | Lock held > 60 min                            |
| `silent_agent`         | critical | Agent holding a lock + heartbeat silent 10 min|
| `overdue_issue`        | warning  | Open issue with `due_at < now`                |
| `orphan_subissue`      | info     | Closed parent with open subissues             |
| `unreachable_project`  | warning  | `project.status == "error"` (fetch failures)  |
| `ci_failure`           | warning  | `meta/ci-status.json.state == "failing"`      |
| `signature_invalid`    | critical | Invalid signature on recent hub commit        |

Thresholds are currently compile-time constants
(`STALE_LOCK_MINUTES`, `SILENT_AGENT_MINUTES`). Making them per-
project overrideable is future work — the infrastructure is a DB
column away, but the UI isn't built yet.

### 4.3 Responding to a lock alert

1. Click into the project tile.
2. Open the **Locks** drawer.
3. Verify the lock is really stale (click into the linked issue and
   read the trail).
4. Choose **Release** (the claimant cooperates and yields) or
   **Steal** (force-reassign to you / another agent).

Steal writes a signed commit on the hub branch; the previous
claimant will see the change next time they sync and may need to
reconcile any in-progress work.

### 4.4 Controlling a running agent

Project detail → **Agents** tab. Each agent row surfaces the git-
native control protocol (design doc §9):

- **Pause** — writes a `pause` request under `agents/<id>/requests/`.
  Agent picks it up on its next `crosslink sync`, sets a local flag,
  and reads-but-doesn't-write until **Resume** clears the flag.
- **Kill** — cooperative shutdown request. Agent is expected to
  checkpoint work, drop its lock, and exit cleanly.
- **Reprioritise** — tell an agent to drop its current issue and
  switch to a different one (e.g. escalate a critical bug).

**All control actions are cooperative.** This is by design —
forceful termination is outside the tool's purpose.

### 4.5 Opening a terminal

Project detail → **Terminals**. Spawns an xterm.js session attached
to a PTY rooted in the tracked workspace. Use cases:

- Launching a design session (`crosslink design "…"`)
- Kicking off a new agent (`crosslink kickoff run <issue>`)
- Anything the CLI can do

Sessions survive tab closes up to a configurable idle limit. See the
Terminals page for active PTY inventory.

---

## 5. Configuration reference

### 5.1 Command-line flags

```
crosslink dashboard serve [OPTIONS]

Options:
      --port <PORT>                   Port to listen on (default: 3100)
      --dashboard-dir <PATH>          Override bundled assets with a
                                      local Vite build output
                                      (development only — bundled
                                      assets are preferred otherwise)

crosslink dashboard track <PATH> [--slug <SLUG>]
crosslink dashboard untrack <SLUG>
crosslink dashboard list
```

The server always binds `127.0.0.1`. A flag to change the interface
is **not** provided — exposing the panel beyond loopback is out of
scope (see §7).

### 5.2 Persistent config (`dashboard.db` `config` table)

| Key                  | Type   | Written by        | Purpose                                    |
|----------------------|--------|-------------------|--------------------------------------------|
| `github.token`       | string | `/settings/github` | Encrypted GitHub PAT (AES-256-GCM)        |
| `github.default_org` | string | `/settings/github` | Org used by the "Browse org" shortcut     |
| `webhook.urls`       | JSON   | `/settings/webhooks` | Array of outbound webhook URLs          |

Rows are inserted/updated via the REST endpoints; direct SQL editing
works but is unsupported — the shape may change without migration
for unreleased keys.

### 5.3 Browser-local preferences (`localStorage`)

Stored under `crosslink_dashboard_prefs`:

```json
{
  "theme": "system" | "dark" | "light",
  "audibleEnabled": false,
  "audibleSeverities": ["critical"]
}
```

Shape is sanitised on read — unknown severities are dropped, bad
JSON falls back to defaults. Clear the key (DevTools → Application →
Local Storage) to reset to factory defaults.

### 5.4 Environment variables

The dashboard itself doesn't read any — today all runtime knobs are
flags or DB-stored config. The main crosslink binary honours the
usual environment (`HOME`, `USER`, `GIT_*` for subprocess git calls);
those apply transitively.

---

## 6. Backup & recovery

### 6.1 What's in `~/.crosslink/dashboard.db`

Seven tables (design doc §6):

| Table           | Purpose                                       | Backup? |
|-----------------|-----------------------------------------------|---------|
| `projects`      | Tracked repo list + clone path + pinning      | Yes     |
| `project_state` | Counters (open/overdue/etc.) — rehydratable   | No      |
| `alerts`        | Alert lifecycle + ACK state                   | Optional (rehydrates in one tick) |
| `pty_sessions`  | Terminal session metadata                     | No      |
| `actions`       | Audit log of dashboard-originated writes      | Yes (forensics) |
| `activity`      | Hub-observed activity timeline                | No (derived) |
| `config`        | GitHub token, default org, webhook URLs       | Yes     |

For a minimal personal backup:

```bash
sqlite3 ~/.crosslink/dashboard.db ".backup ~/dashboard-backup.db"
```

Full restore is a file copy back:

```bash
pkill -f "crosslink dashboard"        # stop the server
cp ~/dashboard-backup.db ~/.crosslink/dashboard.db
crosslink dashboard serve             # start again
```

### 6.2 Reset scenarios

| I want to…                       | Action                                                        |
|----------------------------------|---------------------------------------------------------------|
| Forget one project               | `crosslink dashboard untrack <slug>`                          |
| Forget my PAT                    | Settings → GitHub → **Remove** (or `crosslink dashboard serve` + delete the `github.token` key) |
| Clear all webhooks               | Settings → Webhooks → remove each → Save; or PUT `{"urls": []}` |
| Start from scratch (nuke all state)| `pkill -f "crosslink dashboard" && rm ~/.crosslink/dashboard.db && crosslink dashboard serve` |

Your tracked git workspaces are **never** touched by any of the
above. They're yours; the dashboard only remembers paths to them.

### 6.3 Schema migrations

Schema version is stored in `PRAGMA user_version`. `DashboardDb::open`
is idempotent — re-running it on an existing DB at the current
version is a no-op. When the schema bumps, migrations run
version-gated inside `open`, so upgrading the binary is enough to
migrate. No external migration tool to invoke.

Downgrading the binary below the current schema version is not
supported. If you need to run an older release, restore a backup
taken before the schema bump.

---

## 7. Security

The dashboard is **not** a hostile-network-facing service. Its
security posture is: trusted operator, trusted workstation. This
section documents the actual mitigations and the known gaps.

### 7.1 Network exposure

- **Bind address**: `127.0.0.1:3100`, hard-coded. No flag exposes a
  way to change it. The listener socket itself refuses non-loopback
  connections at the kernel level.
- **No TLS**: on loopback it's unnecessary and would only add
  cert-management pain for the operator.
- **CORS**: allows `http://localhost:5173` and `http://127.0.0.1:5173`
  for the Vite dev server. All other origins are rejected.

If you need a dashboard accessible from another machine, run it
locally *there* rather than exposing this one over the network. Any
sharing-over-LAN story needs a front-end proxy with proper auth —
not shipping yet.

### 7.2 API auth

- Random 32-char hex bearer token, regenerated every process start.
- Printed once to stdout at startup; the `?token=` URL is the only
  place it's echoed. Re-generated, not persisted.
- Required on every `/api/*` route except `/api/v1/health` (liveness
  probe) and `/ws` (subscribed separately — but WS rejects messages
  without a valid token query param).

### 7.3 GitHub PAT storage

- AES-256-GCM encrypted, stored in `dashboard.db` `config` table
  under `github.token`.
- Key is SHA-256 of: `/etc/machine-id` (or
  `/var/lib/dbus/machine-id` fallback) + `$USER` +
  `"crosslink-dashboard-pat-v1"` + a per-install random file
  (`~/.crosslink/.dashboard-key`, created if missing).
- Threat model: obfuscation against **casual disk read** by someone
  else poking around on the box — not defence against an attacker
  with full shell access. The real protection is `chmod 600` on the
  DB file and standard OS multi-user isolation.

PAT rotation: Settings → GitHub → paste new token → Save. The old
ciphertext is overwritten in place; no `shred`.

### 7.4 Webhook URL secrets

Slack / Discord webhook URLs embed secrets in their path component.
Handling:

- **At rest**: stored plaintext in `dashboard.db`. Encryption would
  only obscure local reads on the same box that already has the
  `.dashboard-key`; no real benefit.
- **In transit**: HTTPS enforced by URL validation (`https://…`
  only, except loopback).
- **In logs**: when a dispatch fails, only the scheme + host is
  logged (`mask_url` drops the path). Rotate a URL by revoking it
  upstream (Slack → Incoming Webhooks → Deactivate) and pasting the
  new one in Settings.

### 7.5 Write path

Every mutation shells out to the real `crosslink` CLI in the tracked
workspace. Commits are signed with the workspace's git identity.
This means:

- The dashboard can never write on behalf of a different identity
  than whichever key `git commit` would use in that workspace.
- Stealing a lock or closing an issue from the dashboard produces a
  hub commit that shows up in the audit trail under the user's real
  key — not an anonymous "dashboard" service account.
- The `actions` table records every `run_cli` invocation with actor
  + verb + outcome for local forensics.

### 7.6 Multi-user machines

One `~/.crosslink/dashboard.db` per user. Users on the same machine
do **not** share dashboard state — separate PATs, separate alert
histories, separate webhooks. The underlying git workspaces can
still be shared if both users clone into the same paths, but the
dashboard treats each user's view as private.

---

## 8. Observability

### 8.1 Logs

All logs go to stdout/stderr in the format `tracing_subscriber`
produces by default. Run the server foreground or redirect.

Useful knobs (standard `tracing` env):

```bash
RUST_LOG=info crosslink dashboard serve
RUST_LOG=crosslink::dashboard=debug crosslink dashboard serve
RUST_LOG=warn,crosslink::dashboard::webhook=debug crosslink dashboard serve
```

Common log lines and what they mean:

| Log                                           | Meaning                                  |
|-----------------------------------------------|------------------------------------------|
| `dashboard poll loop starting (tick = 5s, …)` | Server booted, poll loop live            |
| `poll failed for <slug>: …`                   | Single project failed this tick. Non-fatal; tick continues |
| `webhook dispatch failed for <host>: …`       | One webhook URL returned non-2xx / timed out |
| `no tracker_remote configured in …`           | Benign config warning                    |

### 8.2 Health endpoint

`GET /api/v1/health` returns `200 OK` with an empty body. No auth.
Use for liveness probes if you wrap the process with a supervisor.

### 8.3 WebSocket events you can subscribe to

Operators typically don't interact with the WS directly, but useful
for debugging:

- `dashboard_project_updated` — one project's state row just changed
- `dashboard_alerts_changed` — alerts opened/resolved for a project

---

## 9. Troubleshooting runbook

### 9.1 "404 on the root URL" (the #429 bug from history)

If `/` returns 404 after a `cargo install`, you're running a build
from before GH #429 was fixed. Update:

```bash
cargo install crosslink --force
```

The bundled frontend ships with the binary now; there is no
configuration step where a user can produce an empty-404 state
accidentally.

### 9.2 "401 Unauthorized" on every request

Either (a) the token in `sessionStorage` is stale (you restarted the
server) or (b) you opened a new tab without a fresh `?token=…` URL.

Fix: open the URL from the **most recent** `Dashboard: http://…?token=…`
line printed by `crosslink dashboard serve`. That round-trips a
fresh token into `sessionStorage` and strips it from the URL.

### 9.3 "No projects tracked" after a restart

Likely: you deleted `dashboard.db` or copied a backup from another
machine where the projects table was empty.

Fix: re-run `crosslink dashboard track <path>` for each workspace.
Or restore a backup that contains them.

### 9.4 A project stays at `unreachable_project` for more than a tick

Means `git fetch` is failing for the tracked workspace. Inspect:

```bash
cd /path/to/tracked/workspace
git fetch --all --verbose
```

Typical causes: SSH agent not forwarding, expired HTTPS credentials,
firewall outage, repo renamed/moved. Once `git fetch` works again,
the next poll tick clears the alert automatically.

### 9.5 Webhook alerts stopped firing

Check, in order:

1. Settings → Webhooks — is the URL still listed?
2. Settings → Preferences — is audible / webhook-wise routing still
   enabled? (Preferences is audible-only; webhooks are always on.)
3. Server logs with `RUST_LOG=crosslink::dashboard::webhook=debug` —
   look for `webhook dispatch failed for <host>: …`.
4. The upstream receiver — Slack, Discord, etc. Has the webhook been
   rotated / deactivated upstream?

### 9.6 Audio alerts don't play

Most likely: the browser suspended the AudioContext pre-gesture.
Click anywhere in the dashboard tab, then wait for the next alert —
it'll play. Chrome / Safari / Firefox all enforce this; no dashboard
workaround exists.

Also check Settings → Preferences → audible toggle is on and the
incoming alert's severity is in your enabled list.

### 9.7 The poll loop falls behind

Symptom: tile timestamps are many ticks stale. Cause: the poll loop
runs projects serially within each 5-second tick, so if a single
`git fetch` takes 30 seconds, the whole fleet stalls until it
returns.

Mitigations:

- Untrack any repos you don't actively care about.
- Check `git fetch` latency on the slowest-updating tracked
  workspace (often a repo with network hiccups or a huge object
  graph).
- Pin a large mirror to a faster remote if possible.

Parallelising the fetches is future work; it was deliberately kept
serial for the MVP to avoid hammering the network / a shared git
forge.

### 9.8 PAT rate-limited

GitHub's primary limit is 5000 req/hour for classic PATs. The
"Browse org" call is one REST request per enumerated repo. If
you're bouncing against the limit:

- Wait for the window to reset.
- Prefer tracking one-repo-at-a-time via `crosslink dashboard track`
  rather than bulk enumeration.
- Consider a GitHub App install instead of a PAT for higher limits
  (out of scope for now; not implemented).

### 9.9 Something else is wrong

`RUST_LOG=debug crosslink dashboard serve 2>&1 | tee dashboard.log`,
reproduce the issue, and open a crosslink issue with the log
attached. Include your OS, `crosslink --version`, and rough count of
tracked projects.

---

## 10. Upgrades

### 10.1 In-place upgrade

```bash
pkill -f "crosslink dashboard"        # graceful: SIGTERM, poll loop drains
cargo install crosslink --force       # replace the binary
crosslink dashboard serve             # schema migrations run automatically
```

No separate dashboard rebuild needed — the frontend is bundled into
the binary.

### 10.2 Pre-upgrade checklist

- Backup `~/.crosslink/dashboard.db` (§6.1).
- Note your current version: `crosslink --version`.
- Skim the [CHANGELOG](CHANGELOG.md) `[Unreleased]` → whatever's
  next for breaking changes.

### 10.3 Downgrade

Supported only between schema-compatible versions. If the newer
binary bumped `PRAGMA user_version`, the older one will refuse to
open the DB. Restore a pre-upgrade backup in that case.

---

## 11. Known sharp edges

Documented so operators don't waste debugging time on them:

- **First audible alert after page load may be silent** — browser
  AudioContext gesture policy. Not fixable without a modal "click
  anywhere to enable alerts" prompt, which we've opted to skip.
- **Poll loop is serial** — a single slow repo stalls the tick for
  all others. See §9.7.
- **PAT encryption is obfuscation, not defence-in-depth** — see
  §7.3.
- **Webhooks fire on *open*, not resolve** — resolution events are
  visible only in the UI. Adding "resolved" webhooks is a trivial
  extension if demand arrives.
- **No per-project alert threshold overrides in UI** — the knobs
  exist as compile-time constants; UI wiring is future work.
- **Agent control requests are cooperative** — a wedged agent that
  isn't polling won't respond to pause/kill. Out-of-band recovery
  (kill the tmux pane, free the lock via Settings → Locks → Steal)
  is the fallback.
- **No HA / no clustering** — it's a single-user local service. If
  your machine goes away, so does your dashboard. Backups (§6)
  protect your state; nothing protects uptime.

---

## 12. FAQ

**Q: Can I run this on a shared server for my team?**
No. One user per installation; bind is loopback-only. For a team
view, each member runs their own.

**Q: Is there a docker image?**
Not yet. There's no technical blocker — a single-static-binary
image would be small — but it hasn't shipped.

**Q: Does the dashboard modify my git workspaces?**
Only through the `crosslink` CLI in that workspace (writes, commits).
It never `git reset`s or checks out branches behind your back. The
poll loop does `git fetch` of the hub branch only.

**Q: Where do I file operational issues vs. design feedback?**
Operational (doesn't work, confused by behaviour, need better
logs): GH issues on the crosslink repo. Design (think this should
work differently): crosslink issues on the `crosslink/hub` branch,
or propose a revision to [DESIGN-CROSSLINK-DASHBOARD.md](DESIGN-CROSSLINK-DASHBOARD.md).

**Q: Can I run the dashboard and `crosslink serve` simultaneously?**
They bind the same default port. Pick one. `crosslink serve` is
deprecated (see design doc §17); migrate when you can.

**Q: The `?token=…` URL is scary — can I disable auth?**
No. Even on loopback, other processes on the same machine can reach
`127.0.0.1`, so some form of auth is necessary. The token mechanism
is the lightest thing that gives you that without cert management.

---

## 13. Related docs

- [DESIGN-CROSSLINK-DASHBOARD.md](DESIGN-CROSSLINK-DASHBOARD.md) —
  architecture, data model, wire formats, phased rollout
- [CHANGELOG.md](CHANGELOG.md) — what shipped in which release
- [README.md](README.md) — crosslink top-level intro + install

---

## 14. Revision log

| Date       | Notes                                              |
|------------|----------------------------------------------------|
| 2026-04-21 | Initial version. Covers through Phase 5.3.         |
