# Design: Web Dashboard

**Status:** Draft v1
**Last updated:** 2026-03-10

---

## 1. Problem Statement

Crosslink's power is locked behind a CLI. Monitoring agents means `tmux attach`, checking status means `crosslink kickoff status`, understanding the dependency graph means reading `crosslink tree` output in your head. For orchestrating multi-phase builds from design documents, you're juggling `crosslink swarm`, `kickoff`, `mission-control`, and manual merges.

A web dashboard unlocks:
- **At-a-glance monitoring** — see all agents, heartbeats, locks, and progress without attaching to terminals
- **Full CRUD** — every crosslink command available through forms, not memorized CLI syntax
- **Design doc orchestration** — upload a doc, review the decomposed plan, hit "Go", watch the DAG execute
- **Real-time streaming** — heartbeats and events push to the browser, no polling

### What exists today

| Capability | Status | Location |
|-----------|--------|----------|
| Agent heartbeats on hub branch | Working | `sync.rs:701` (`push_heartbeat`) |
| Heartbeat reading + staleness | Working | `sync.rs:763` (`read_heartbeats`) |
| Lock management (claim/release/stale) | Working | `sync.rs:1043` (`claim_lock`) |
| Issue CRUD + full organization | Working | `db.rs` (50+ public methods) |
| Session management | Working | `db.rs` sessions API |
| Milestone management | Working | `db.rs` milestones API |
| Knowledge pages | Working | `knowledge.rs` |
| Hub sync (push/pull/fetch) | Working | `sync.rs` |
| Export/import (JSON) | Working | `commands/export.rs`, `commands/import.rs` |
| Swarm plan/execute/resume | Working | `commands/swarm.rs` |
| Kickoff run/plan/status/report | Working | `commands/kickoff.rs` |
| TUI (ratatui terminal UI) | Working | `tui/` |
| Mission control (tmux dashboard) | Working | `commands/mission_control.rs` |
| Watchdog (idle agent nudging) | Working | `commands/kickoff.rs` watchdog sidecar |

### Design goals

1. **Full CLI parity** — every crosslink command has a GUI equivalent
2. **Real-time** — WebSocket push for heartbeats, events, agent status
3. **Orchestration** — LLM-assisted design doc decomposition → DAG execution
4. **Localhost-first** — no auth, no cloud, single-operator dashboard
5. **Parallel buildable** — each phase decomposes into 3–5 independent agent tasks

---

## 2. Architecture

```
┌─────────────────────────────────────────────┐
│               Browser (React)                │
│  ┌─────────┐ ┌──────────┐ ┌──────────────┐  │
│  │ Agent   │ │ Issues / │ │ Design Doc   │  │
│  │ Monitor │ │ Sessions │ │ Orchestrator │  │
│  └────┬────┘ └────┬─────┘ └──────┬───────┘  │
│       │           │              │           │
│       └───────────┼──────────────┘           │
│               WebSocket + REST               │
└───────────────────┬─────────────────────────┘
                    │
┌───────────────────┴─────────────────────────┐
│          crosslink serve (axum)              │
│  ┌──────────┐ ┌──────────┐ ┌─────────────┐  │
│  │ REST API │ │ WS Hub   │ │ Static File │  │
│  │ /api/*   │ │ /ws      │ │ Serving     │  │
│  └────┬─────┘ └────┬─────┘ └─────────────┘  │
│       │            │                         │
│  ┌────┴────────────┴──────────────────────┐  │
│  │         Crosslink Core (lib.rs)        │  │
│  │  Database · SyncManager · Knowledge    │  │
│  │  Identity · Kickoff · Swarm · Events   │  │
│  └────────────────────────────────────────┘  │
└──────────────────────────────────────────────┘
        │              │
   ┌────┴────┐    ┌────┴────┐
   │ SQLite  │    │ Hub Git │
   │issues.db│    │ Branch  │
   └─────────┘    └─────────┘
```

### 2.1 Backend: `crosslink serve`

New subcommand added to the existing `crosslink` binary.

```
crosslink serve [--port 3100] [--dashboard-dir ./dashboard/dist]
```

**Framework:** axum (already in the Rust ecosystem, async, lightweight)

**Key design decisions:**
- Direct Rust function calls into `db.rs`, `sync.rs`, `knowledge.rs` etc. — no shelling out
- `Database` and `SyncManager` wrapped in `Arc<Mutex<>>` for shared access across handlers
- WebSocket hub uses `tokio::sync::broadcast` — file watcher on `issues.db` and hub cache triggers events
- Static file serving from `dashboard/dist/` on disk (not embedded — dashboard is optional)
- All API responses are JSON, all mutations accept JSON bodies

**New Cargo dependencies:**
- `axum` — HTTP framework
- `tower-http` — CORS, static file serving, compression
- `tokio` — async runtime (may already be transitive)
- `tokio-tungstenite` or axum's built-in WS — WebSocket support
- `notify` — filesystem watcher for real-time event push

### 2.2 Frontend: `dashboard/`

Lives at repo root as a sibling to `crosslink/`. **Implemented as of Phase 1 Agent 1B.**

```
dashboard/
├── package.json
├── vite.config.ts           # Proxy /api and /ws to localhost:3100
├── tsconfig.json
├── components.json          # shadcn/ui config
├── src/
│   ├── main.tsx
│   ├── App.tsx              # Router + layout shell + WebSocket listener
│   ├── vite-env.d.ts
│   ├── index.css            # Tailwind v4 @theme inline dark palette
│   ├── api/
│   │   ├── client.ts        # Typed fetch wrapper for all endpoints
│   │   └── ws.ts            # WebSocket client with exponential-backoff reconnect
│   ├── stores/
│   │   ├── agents.ts        # zustand store
│   │   ├── issues.ts        # zustand store
│   │   └── orchestrator.ts  # zustand store
│   ├── pages/               # 13 page components
│   │   ├── Dashboard.tsx
│   │   ├── Agents.tsx
│   │   ├── AgentDetail.tsx
│   │   ├── Issues.tsx
│   │   ├── IssueDetail.tsx
│   │   ├── Sessions.tsx
│   │   ├── Milestones.tsx
│   │   ├── Knowledge.tsx
│   │   ├── KnowledgeDetail.tsx
│   │   ├── Sync.tsx
│   │   ├── Config.tsx
│   │   ├── Orchestrator.tsx
│   │   └── Execution.tsx
│   ├── components/
│   │   ├── Sidebar.tsx
│   │   └── ui/              # shadcn/ui: button, badge, card, input, table,
│   │                        #   dialog, separator, scroll-area, tooltip
│   └── lib/
│       ├── types.ts         # TypeScript types matching all Rust models
│       └── utils.ts
└── dist/                    # Built output, served by crosslink serve
```

**Stack:**
- React 19 + TypeScript
- Vite 7 (build + dev server with proxy to `crosslink serve`)
- shadcn/ui + Tailwind CSS v4
- zustand (state management)
- React Router v7 (navigation)
- @xyflow/react (DAG visualization — Phase 6)
- recharts (token usage graphs — Phase 5)

> **Tailwind v4 note:** Configuration lives entirely in `src/index.css` using `@theme inline`
> to map CSS variables to utility tokens. There is no `tailwind.config.ts`. The
> `@apply border-border` pattern from v3 does not work in v4.

### 2.3 REST API Surface

All endpoints prefixed with `/api/v1/`.

#### Issues
| Method | Path | Maps to |
|--------|------|---------|
| GET | `/issues` | `db.list_issues()` with query params for filters |
| POST | `/issues` | `db.create_issue()` |
| GET | `/issues/:id` | `db.get_issue()` + labels + comments + deps |
| PATCH | `/issues/:id` | `db.update_issue()` |
| DELETE | `/issues/:id` | `db.delete_issue()` |
| POST | `/issues/:id/close` | `db.close_issue()` |
| POST | `/issues/:id/reopen` | `db.reopen_issue()` |
| POST | `/issues/:id/subissue` | `db.create_subissue()` |
| GET | `/issues/:id/comments` | `db.get_comments()` |
| POST | `/issues/:id/comments` | `db.add_comment()` |
| POST | `/issues/:id/labels` | `db.add_label()` |
| DELETE | `/issues/:id/labels/:label` | `db.remove_label()` |
| POST | `/issues/:id/block` | `db.add_dependency()` |
| DELETE | `/issues/:id/block/:blocker` | `db.remove_dependency()` |
| GET | `/issues/:id/tree` | `db.get_subissues()` recursive |
| GET | `/issues/blocked` | `db.get_blocked_issues()` |
| GET | `/issues/ready` | `db.get_ready_issues()` |

#### Sessions
| Method | Path | Maps to |
|--------|------|---------|
| GET | `/sessions/current` | `db.get_current_session_for_agent()` |
| POST | `/sessions/start` | `db.start_session()` |
| POST | `/sessions/end` | `db.end_session()` |
| POST | `/sessions/work/:id` | `db.set_active_work()` |

#### Milestones
| Method | Path | Maps to |
|--------|------|---------|
| GET | `/milestones` | `db.list_milestones()` |
| POST | `/milestones` | `db.create_milestone()` |
| GET | `/milestones/:id` | `db.get_milestone()` |
| POST | `/milestones/:id/assign` | `db.assign_milestone()` |
| POST | `/milestones/:id/close` | `db.close_milestone()` |

#### Knowledge
| Method | Path | Maps to |
|--------|------|---------|
| GET | `/knowledge` | `knowledge::list_pages()` |
| GET | `/knowledge/:slug` | `knowledge::read_page()` |
| POST | `/knowledge` | `knowledge::create_page()` |
| GET | `/knowledge/search?q=` | `knowledge::search_content()` |

#### Agents & Monitoring
| Method | Path | Maps to |
|--------|------|---------|
| GET | `/agents` | `sync.read_heartbeats()` + worktree probe |
| GET | `/agents/:id` | Agent detail (heartbeat + locks + events) |
| GET | `/agents/:id/status` | `kickoff::status()` equivalent |
| GET | `/locks` | `sync.read_locks_auto()` |
| GET | `/locks/stale` | `sync.find_stale_locks_with_age()` |

#### Sync
| Method | Path | Maps to |
|--------|------|---------|
| GET | `/sync/status` | Hub init state, last fetch time |
| POST | `/sync/fetch` | `sync.fetch()` |
| POST | `/sync/push` | `sync.push()` |

#### Config
| Method | Path | Maps to |
|--------|------|---------|
| GET | `/config` | Read `hook-config.json` |
| PATCH | `/config` | Merge-update `hook-config.json` |

#### Orchestrator
| Method | Path | Maps to |
|--------|------|---------|
| POST | `/orchestrator/decompose` | LLM-assisted doc → phase/stage/task breakdown |
| GET | `/orchestrator/plan` | Current execution plan |
| POST | `/orchestrator/execute` | Start DAG execution |
| POST | `/orchestrator/pause` | Pause execution |
| GET | `/orchestrator/status` | Execution progress |

### 2.4 WebSocket Protocol

Single WebSocket endpoint: `/ws`

Messages are JSON with a `type` field:

```typescript
// Server → Client
{ type: "heartbeat", agent_id: string, timestamp: string, issue_id?: number }
{ type: "agent_status", agent_id: string, status: "running" | "idle" | "done" | "failed" }
{ type: "issue_updated", issue_id: number, field: string }
{ type: "lock_changed", issue_id: number, action: "claimed" | "released" }
{ type: "execution_progress", phase: string, stage: string, status: string }

// Client → Server
{ type: "subscribe", channels: ["agents", "issues", "execution"] }
```

Implementation: `notify` crate watches `issues.db` mtime and hub cache directory. On change, diff the state and broadcast relevant events through `tokio::sync::broadcast`.

---

## 3. Phase Breakdown

### Phase 1: Skeleton (3 agents, ~2 hours each)

**Merge gate:** `crosslink serve` boots, serves the React app at `http://localhost:3100`, health endpoint returns OK, frontend shows a layout shell with sidebar navigation.

#### Agent 1A: Rust axum server

**Files to create/modify:**
- `crosslink/Cargo.toml` — add axum, tower-http, tokio, serde_json deps
- `crosslink/src/server/mod.rs` — server module
- `crosslink/src/server/state.rs` — `AppState` struct wrapping `Arc<Database>`, `Arc<SyncManager>`, config
- `crosslink/src/server/routes.rs` — route definitions
- `crosslink/src/server/handlers/health.rs` — `GET /api/v1/health`
- `crosslink/src/main.rs` — add `Commands::Serve { port, dashboard_dir }` variant

**Deliverables:**
- `crosslink serve --port 3100 --dashboard-dir ./dashboard/dist` starts an axum server
- `GET /api/v1/health` returns `{"status": "ok", "version": "0.4.0"}`
- Static files served from the dashboard directory at `/`
- CORS configured for development (vite dev server on :5173)

#### Agent 1B: React + Vite scaffold ✅ DONE

**Status:** Implemented. See `dashboard/` at repo root.

**Delivered:**
- `cd dashboard && npm install && npm run dev` starts dev server on :5173
- Sidebar navigation with links for all 13 sections
- Dark theme (zinc palette, matches terminal aesthetic)
- Typed API client (`src/api/client.ts`) covering all endpoints
- WebSocket hook (`src/api/ws.ts`) with exponential-backoff reconnect
- shadcn/ui components: button, badge, card, input, table, dialog, separator, scroll-area, tooltip
- zustand stores for agents, issues, orchestrator
- 13 page components (functional with live data binding where applicable)
- TypeScript types matching all Rust models (`src/lib/types.ts`)

#### Agent 1C: API contract + shared types

**Files to create:**
- `dashboard/src/lib/types.ts` — complete TypeScript types (done by 1B — verify completeness)
- `crosslink/src/server/types.rs` — serde-serializable response/request types
- `docs/api.md` — API reference documenting every endpoint

**Deliverables:**
- Rust response structs with `#[derive(Serialize)]` matching the TS types
- Request structs with `#[derive(Deserialize)]` for mutations
- API reference document

---

### Phase 2: Agent Dashboard (4 agents, ~2 hours each)

**Merge gate:** Dashboard shows live agent cards that update in real-time via WebSocket.

**Depends on:** Phase 1

Agents: 2A (backend agent endpoints), 2B (backend WebSocket hub), 2C (frontend agent list), 2D (frontend agent detail)

---

### Phase 3: Issues & Sessions (4 agents, ~3 hours each)

**Merge gate:** Full issue CRUD through the web UI.

**Depends on:** Phase 1

Agents: 3A (backend issues CRUD), 3B (backend sessions + milestones), 3C (frontend issue list + detail), 3D (frontend session + organization UI)

---

### Phase 4: Remaining CLI Parity (4 agents, ~2 hours each)

**Merge gate:** Every crosslink CLI command has a web equivalent.

**Depends on:** Phase 3

Agents: 4A (backend knowledge + search), 4B (backend sync + config), 4C (frontend knowledge + milestones + search), 4D (frontend sync + config)

---

### Phase 5: Token Tracking (2 agents, ~3 hours each)

**Merge gate:** Per-agent token usage displayed, session cost estimates, usage graphs.

**Depends on:** Phase 2

Agents: 5A (backend token usage collection), 5B (frontend usage graphs)

---

### Phase 6: Design Document Orchestration (5 agents, ~4 hours each)

**Merge gate:** Upload a design doc, review LLM-decomposed plan, execute as a managed DAG, monitor progress in real-time.

**Depends on:** Phases 1–4

Agents: 6A (backend LLM decomposition), 6B (backend DAG execution engine), 6C (frontend document import + stage editor), 6D (frontend DAG/Gantt visualization), 6E (frontend execution control + live monitoring)

---

## 4. Dependency Graph

```
Phase 1 (Skeleton)
    ├──→ Phase 2 (Agent Dashboard)
    │        └──→ Phase 5 (Token Tracking)
    ├──→ Phase 3 (Issues & Sessions)
    │        └──→ Phase 4 (CLI Parity)
    └──────────────→ Phase 6 (Orchestrator) [depends on 1-4]
```

Phases 2 and 3 run **in parallel** after Phase 1.
Phase 4 depends on Phase 3. Phase 5 depends on Phase 2.
Phase 6 depends on Phases 1–4.

**Total: 22 agent sessions across 4 sequential rounds.**

---

## 5. Open Risks

| Risk | Mitigation |
|------|-----------|
| Agent merge conflicts (8 agents in phases 2+3) | Clear file ownership per agent. Backend agents never touch frontend. |
| WebSocket complexity | Start with polling fallback, upgrade to WS. axum has solid WS support. |
| LLM decomposition quality (phase 6) | Human review step before execution. Iterative refinement prompt. |
| SQLite concurrent access | Single writer via `Arc<Mutex<Database>>`. Reads use separate connections. |
| Large design docs overwhelming LLM context | Chunk by section, decompose phases independently, merge plans. |
| Dashboard build adding to CI time | Separate CI job. `crosslink serve` works without dashboard present. |
