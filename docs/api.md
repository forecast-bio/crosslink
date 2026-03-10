# Crosslink Web Dashboard — API Reference

> **Status:** Phase 1 contract. All endpoints are prefixed with `/api/v1/`.
> **Server:** `crosslink serve [--port 3100]`
> **Auth:** None (localhost-only dashboard)
> **Content-Type:** `application/json` for all request and response bodies.

---

## Table of Contents

1. [Health](#health)
2. [Issues](#issues)
3. [Comments](#comments)
4. [Labels](#labels)
5. [Dependencies](#dependencies)
6. [Sessions](#sessions)
7. [Milestones](#milestones)
8. [Knowledge Pages](#knowledge-pages)
9. [Agents & Monitoring](#agents--monitoring)
10. [Locks](#locks)
11. [Sync](#sync)
12. [Config](#config)
13. [Orchestrator](#orchestrator)
14. [WebSocket Protocol](#websocket-protocol)
15. [Error Responses](#error-responses)

---

## Health

### `GET /api/v1/health`

Returns server status and version. Use this to verify the server is up.

**Response `200`**
```json
{
  "status": "ok",
  "version": "0.4.0"
}
```

**Rust type:** `HealthResponse`
**TS type:** `HealthResponse`

---

## Issues

### `GET /api/v1/issues`

List issues with optional filtering.

**Query Parameters**

| Parameter  | Type     | Description                                     |
|------------|----------|-------------------------------------------------|
| `status`   | string   | `open`, `closed`, `archived`, or `all`          |
| `label`    | string   | Filter by exact label name                      |
| `priority` | string   | `low`, `medium`, `high`, or `critical`          |
| `search`   | string   | Full-text search across title, description, comments |
| `parent_id`| number   | Return only subissues of this parent            |

**Response `200`**
```json
{
  "items": [
    {
      "id": 42,
      "title": "Fix auth timeout",
      "description": null,
      "status": "open",
      "priority": "high",
      "parent_id": null,
      "created_at": "2026-03-01T12:00:00Z",
      "updated_at": "2026-03-01T12:00:00Z",
      "closed_at": null,
      "labels": ["bug"],
      "blocker_count": 0
    }
  ],
  "total": 1
}
```

**Rust type:** `IssueListResponse`
**TS type:** `IssueListResponse`

---

### `POST /api/v1/issues`

Create a new issue.

**Request body**
```json
{
  "title": "Add dark mode",
  "description": "Users want a dark theme option.",
  "priority": "medium",
  "parent_id": null
}
```

| Field        | Type   | Required | Default    |
|--------------|--------|----------|------------|
| `title`      | string | Yes      | —          |
| `description`| string | No       | `null`     |
| `priority`   | string | No       | `"medium"` |
| `parent_id`  | number | No       | `null`     |

**Response `201`**
```json
{ "id": 43 }
```

**Rust type:** `CreateIssueRequest`
**TS type:** `CreateIssueRequest`

---

### `GET /api/v1/issues/:id`

Get a fully hydrated issue with labels, comments, and dependency info.

**Response `200`**
```json
{
  "id": 42,
  "title": "Fix auth timeout",
  "description": "Connection drops after 30s idle.",
  "status": "open",
  "priority": "high",
  "parent_id": null,
  "created_at": "2026-03-01T12:00:00Z",
  "updated_at": "2026-03-02T09:00:00Z",
  "closed_at": null,
  "labels": ["bug", "backend"],
  "comments": [
    {
      "id": 1,
      "issue_id": 42,
      "content": "Root cause found in connection pool.",
      "created_at": "2026-03-02T09:00:00Z",
      "kind": "observation",
      "trigger_type": null,
      "intervention_context": null,
      "driver_key_fingerprint": null
    }
  ],
  "blockers": [],
  "blocking": [44],
  "subissues": [],
  "milestone": {
    "id": 1,
    "name": "v1.0",
    "status": "open"
  }
}
```

**Response `404`** — issue not found.

**Rust type:** `IssueDetail`
**TS type:** `IssueDetail`

---

### `PATCH /api/v1/issues/:id`

Update issue fields. All fields are optional — only provided fields change.

**Request body**
```json
{
  "title": "Fix auth timeout on slow connections",
  "priority": "critical"
}
```

**Response `200`**
```json
{ "ok": true }
```

**Rust type:** `UpdateIssueRequest`
**TS type:** `UpdateIssueRequest`

---

### `DELETE /api/v1/issues/:id`

Permanently delete an issue and all its comments, labels, and dependencies.

**Response `200`**
```json
{ "ok": true }
```

**Response `404`** — issue not found.

---

### `POST /api/v1/issues/:id/close`

Close an issue.

**Response `200`**
```json
{ "ok": true }
```

---

### `POST /api/v1/issues/:id/reopen`

Reopen a closed issue.

**Response `200`**
```json
{ "ok": true }
```

---

### `POST /api/v1/issues/:id/subissue`

Create a subissue (child) of an existing issue.

**Request body**
```json
{
  "title": "Research connection pool options",
  "priority": "medium"
}
```

**Response `201`**
```json
{ "id": 45 }
```

**Rust type:** `CreateSubissueRequest`
**TS type:** `CreateSubissueRequest`

---

### `GET /api/v1/issues/:id/tree`

Get all subissues recursively.

**Response `200`**
```json
{
  "items": [ /* Issue[] */ ],
  "total": 3
}
```

---

### `GET /api/v1/issues/blocked`

List all open issues that are blocked by other open issues.

**Response `200`** — `IssueListResponse`

---

### `GET /api/v1/issues/ready`

List all open issues with no open blockers.

**Response `200`** — `IssueListResponse`

---

## Comments

### `GET /api/v1/issues/:id/comments`

Get all comments on an issue.

**Response `200`**
```json
[
  {
    "id": 1,
    "issue_id": 42,
    "content": "Found the root cause.",
    "created_at": "2026-03-02T09:00:00Z",
    "kind": "observation",
    "trigger_type": null,
    "intervention_context": null,
    "driver_key_fingerprint": null
  }
]
```

**TS type:** `Comment[]`

---

### `POST /api/v1/issues/:id/comments`

Add a comment to an issue.

**Request body**
```json
{
  "content": "Resolved by bumping connection pool timeout to 120s.",
  "kind": "resolution"
}
```

| Field                  | Type   | Required | Default    |
|------------------------|--------|----------|------------|
| `content`              | string | Yes      | —          |
| `kind`                 | string | No       | `"note"`   |
| `trigger_type`         | string | No       | `null`     |
| `intervention_context` | string | No       | `null`     |

Valid `kind` values: `note`, `plan`, `decision`, `observation`, `blocker`, `resolution`, `result`, `intervention`.

**Response `201`**
```json
{ "id": 2 }
```

**Rust type:** `CreateCommentRequest`
**TS type:** `CreateCommentRequest`

---

## Labels

### `GET /api/v1/issues/:id/labels`

Get labels on an issue.

**Response `200`**
```json
["bug", "backend"]
```

---

### `POST /api/v1/issues/:id/labels`

Add a label to an issue.

**Request body**
```json
{ "label": "backend" }
```

**Response `200`**
```json
{ "ok": true }
```

**Rust type:** `AddLabelRequest`
**TS type:** `AddLabelRequest`

---

### `DELETE /api/v1/issues/:id/labels/:label`

Remove a label from an issue. `:label` is the label string (URL-encoded if needed).

**Response `200`**
```json
{ "ok": true }
```

---

## Dependencies

### `POST /api/v1/issues/:id/block`

Mark an issue as blocked by another issue.

**Request body**
```json
{ "blocker_id": 41 }
```

This means "issue `:id` is blocked by issue `41`."

**Response `200`**
```json
{ "ok": true }
```

**Response `400`** — would create a cycle or self-block.

**Rust type:** `AddBlockerRequest`
**TS type:** `AddBlockerRequest`

---

### `DELETE /api/v1/issues/:id/block/:blocker_id`

Remove a blocker dependency.

**Response `200`**
```json
{ "ok": true }
```

---

## Sessions

### `GET /api/v1/sessions/current`

Get the current active session. Returns `null` if no session is active.

**Query Parameters**

| Parameter  | Type   | Description                             |
|------------|--------|-----------------------------------------|
| `agent_id` | string | Scope to a specific agent (optional)    |

**Response `200`**
```json
{
  "id": 1,
  "started_at": "2026-03-10T03:17:00Z",
  "ended_at": null,
  "active_issue_id": 42,
  "handoff_notes": null,
  "last_action": "Implementing auth fix",
  "agent_id": "worker-1"
}
```

**Response `200` (no active session)**
```json
null
```

**Rust type:** `Session`
**TS type:** `Session | null`

---

### `POST /api/v1/sessions/start`

Start a new session.

**Request body**
```json
{ "agent_id": "worker-1" }
```

**Response `201`**
```json
{
  "id": 2,
  "started_at": "2026-03-10T04:00:00Z",
  "ended_at": null,
  "active_issue_id": null,
  "handoff_notes": null,
  "last_action": null,
  "agent_id": "worker-1"
}
```

**Rust type:** `StartSessionRequest`
**TS type:** `StartSessionRequest`

---

### `POST /api/v1/sessions/end`

End the current active session.

**Request body**
```json
{ "notes": "Completed auth fix. Tests passing." }
```

**Response `200`**
```json
{ "ok": true }
```

**Rust type:** `EndSessionRequest`
**TS type:** `EndSessionRequest`

---

### `POST /api/v1/sessions/work/:id`

Set the active issue for the current session.

**Response `200`**
```json
{ "ok": true }
```

---

## Milestones

### `GET /api/v1/milestones`

List milestones with progress statistics.

**Query Parameters**

| Parameter | Type   | Description                         |
|-----------|--------|-------------------------------------|
| `status`  | string | `open`, `closed`, or `all`          |

**Response `200`**
```json
{
  "items": [
    {
      "id": 1,
      "name": "v1.0",
      "description": "First stable release",
      "status": "open",
      "created_at": "2026-03-01T00:00:00Z",
      "closed_at": null,
      "issue_count": 12,
      "completed_count": 8,
      "progress_percent": 66.7
    }
  ],
  "total": 1
}
```

**Rust type:** `MilestoneListResponse`
**TS type:** `MilestoneListResponse`

---

### `POST /api/v1/milestones`

Create a milestone.

**Request body**
```json
{
  "name": "v1.1",
  "description": "Performance improvements"
}
```

**Response `201`**
```json
{ "id": 2 }
```

**Rust type:** `CreateMilestoneRequest`
**TS type:** `CreateMilestoneRequest`

---

### `GET /api/v1/milestones/:id`

Get a single milestone with progress.

**Response `200`** — `MilestoneDetail`
**Response `404`** — not found.

---

### `POST /api/v1/milestones/:id/assign`

Assign an issue to a milestone.

**Request body**
```json
{ "issue_id": 42 }
```

**Response `200`**
```json
{ "ok": true }
```

**Rust type:** `AssignMilestoneRequest`
**TS type:** `AssignMilestoneRequest`

---

### `POST /api/v1/milestones/:id/close`

Close a milestone.

**Response `200`**
```json
{ "ok": true }
```

---

## Knowledge Pages

### `GET /api/v1/knowledge`

List all knowledge pages (summaries only, no content).

**Response `200`**
```json
[
  {
    "slug": "axum-routing",
    "title": "Axum routing patterns",
    "tags": ["rust", "web"],
    "updated": "2026-03-05"
  }
]
```

**TS type:** `KnowledgePageSummary[]`

---

### `GET /api/v1/knowledge/:slug`

Get a single knowledge page with full content.

**Response `200`**
```json
{
  "slug": "axum-routing",
  "title": "Axum routing patterns",
  "tags": ["rust", "web"],
  "sources": [
    {
      "url": "https://docs.rs/axum",
      "title": "Axum docs",
      "accessed_at": "2026-03-01"
    }
  ],
  "contributors": ["worker-1"],
  "created": "2026-03-01",
  "updated": "2026-03-05",
  "content": "# Axum routing\n\n..."
}
```

**Response `404`** — page not found.

**Rust type:** `KnowledgePage`
**TS type:** `KnowledgePage`

---

### `POST /api/v1/knowledge`

Create a new knowledge page.

**Request body**
```json
{
  "slug": "serde-tips",
  "title": "Serde serialization tips",
  "content": "# Tips\n\nUse `skip_serializing_if`...",
  "tags": ["rust", "serde"],
  "sources": []
}
```

**Response `201`**
```json
{ "slug": "serde-tips" }
```

**Rust type:** `CreateKnowledgePageRequest`
**TS type:** `CreateKnowledgePageRequest`

---

### `GET /api/v1/knowledge/search`

Search knowledge pages by content.

**Query Parameters**

| Parameter | Type   | Required | Description |
|-----------|--------|----------|-------------|
| `q`       | string | Yes      | Search query |

**Response `200`**
```json
[
  {
    "slug": "axum-routing",
    "title": "Axum routing patterns",
    "line_number": 12,
    "context_lines": [[11, "## Nested routes"], [12, "Use `Router::merge`..."], [13, ""]]
  }
]
```

**Rust type:** `KnowledgeSearchMatch[]`
**TS type:** `KnowledgeSearchMatch[]`

---

## Agents & Monitoring

### `GET /api/v1/agents`

List all agents with current heartbeat status.

**Response `200`**
```json
[
  {
    "agent_id": "worker-1",
    "machine_id": "my-host",
    "description": "Feature agent",
    "status": "active",
    "last_heartbeat": "2026-03-10T03:15:00Z",
    "active_issue_id": 42,
    "branch": "feature/auth-fix",
    "worktree_path": "/home/user/repo/.worktrees/auth-fix",
    "locks": [42]
  }
]
```

**Agent status values:**
- `active` — heartbeat < 5 minutes ago
- `idle` — heartbeat 5–30 minutes ago
- `stale` — heartbeat > 30 minutes ago
- `unknown` — no heartbeat file found

**Rust type:** `AgentSummary[]`
**TS type:** `AgentSummary[]`

---

### `GET /api/v1/agents/:id`

Get detailed info for a single agent.

**Response `200`**
```json
{
  "agent_id": "worker-1",
  "machine_id": "my-host",
  "description": "Feature agent",
  "status": "active",
  "last_heartbeat": "2026-03-10T03:15:00Z",
  "active_issue_id": 42,
  "branch": "feature/auth-fix",
  "worktree_path": "/home/user/repo/.worktrees/auth-fix",
  "locks": [42],
  "heartbeat_history": [
    "2026-03-10T03:15:00Z",
    "2026-03-10T03:10:00Z",
    "2026-03-10T03:05:00Z"
  ],
  "kickoff_status": "In progress: implementing JWT middleware (step 3/5)"
}
```

**Response `404`** — agent not found.

**Rust type:** `AgentDetail`
**TS type:** `AgentDetail`

---

### `GET /api/v1/agents/:id/status`

Get the kickoff status string for an agent (content of `.kickoff-status` file).

**Response `200`**
```json
{ "status": "In progress: implementing JWT middleware (step 3/5)" }
```

**Response `404`** — agent or status file not found.

---

## Locks

### `GET /api/v1/locks`

List all current issue locks.

**Response `200`**
```json
[
  {
    "issue_id": 42,
    "agent_id": "worker-1",
    "branch": "feature/auth-fix",
    "claimed_at": "2026-03-10T02:00:00Z",
    "signed_by": "SHA256:abc...",
    "age_seconds": 4500,
    "is_stale": false
  }
]
```

**Rust type:** `LockEntry[]`
**TS type:** `LockEntry[]`

---

### `GET /api/v1/locks/stale`

List locks that have exceeded the stale timeout.

**Response `200`** — `LockEntry[]` (only stale locks)

---

## Sync

### `GET /api/v1/sync/status`

Get the current hub sync state.

**Response `200`**
```json
{
  "hub_initialized": true,
  "hub_branch": "crosslink/hub",
  "remote": "origin",
  "last_fetch_at": "2026-03-10T03:00:00Z",
  "active_lock_count": 2,
  "stale_lock_count": 1
}
```

**Rust type:** `SyncStatusResponse`
**TS type:** `SyncStatusResponse`

---

### `POST /api/v1/sync/fetch`

Pull the latest state from the hub branch.

**Response `200`**
```json
{
  "success": true,
  "message": "Fetched 3 new heartbeats, 1 lock update."
}
```

**Rust type:** `SyncActionResponse`
**TS type:** `SyncActionResponse`

---

### `POST /api/v1/sync/push`

Push local changes to the hub branch.

**Response `200`** — `SyncActionResponse`

---

## Config

### `GET /api/v1/config`

Get the current hook configuration.

**Response `200`**
```json
{
  "tracking_mode": "strict",
  "stale_lock_timeout_minutes": 60,
  "remote": "origin",
  "signing_enforcement": "audit",
  "intervention_tracking": true,
  "auto_steal_stale_locks": false
}
```

**Rust type:** `ConfigResponse`
**TS type:** `ConfigResponse`

---

### `PATCH /api/v1/config`

Update config fields. All fields optional — only provided fields are changed.

**Request body**
```json
{
  "tracking_mode": "normal",
  "stale_lock_timeout_minutes": 120
}
```

**Response `200`** — updated `ConfigResponse`

**Rust type:** `UpdateConfigRequest`
**TS type:** `UpdateConfigRequest`

---

## Orchestrator

### `POST /api/v1/orchestrator/decompose`

Submit a design document for LLM-assisted decomposition into phases and stages.

**Request body**
```json
{
  "document": "# My Feature\n\n## Phase 1\n\n...",
  "slug": "my-feature"
}
```

**Response `200`**
```json
{
  "id": "plan-abc123",
  "document_slug": "my-feature",
  "phases": [
    {
      "id": "phase-1",
      "title": "Skeleton",
      "description": "Scaffold the basic structure",
      "stages": [
        {
          "id": "stage-1a",
          "title": "Rust axum server",
          "description": "Set up the axum HTTP server",
          "tasks": [],
          "depends_on": [],
          "agent_count": 1,
          "complexity_hours": 2.0
        }
      ],
      "gate_criteria": ["Server boots", "Health endpoint returns OK"]
    }
  ],
  "created_at": "2026-03-10T03:00:00Z",
  "total_stages": 3,
  "estimated_hours": 6.0
}
```

**Rust type:** `DecomposeRequest` / `OrchestratorPlan`
**TS type:** `DecomposeRequest` / `OrchestratorPlan`

---

### `GET /api/v1/orchestrator/plan`

Get the current saved execution plan.

**Response `200`** — `OrchestratorPlan`
**Response `404`** — no plan exists.

---

### `POST /api/v1/orchestrator/execute`

Start executing the current plan.

**Response `200`**
```json
{ "ok": true }
```

---

### `POST /api/v1/orchestrator/pause`

Pause execution (lets running stages finish, stops launching new ones).

**Response `200`**
```json
{ "ok": true }
```

---

### `GET /api/v1/orchestrator/status`

Get real-time execution progress.

**Response `200`**
```json
{
  "plan_id": "plan-abc123",
  "state": "running",
  "current_phase_id": "phase-1",
  "progress_percent": 33.3,
  "started_at": "2026-03-10T03:05:00Z",
  "completed_at": null,
  "stage_statuses": {
    "stage-1a": "done",
    "stage-1b": "running",
    "stage-1c": "pending"
  },
  "stage_agents": {
    "stage-1b": "worker-2"
  }
}
```

**Stage status values:** `pending`, `running`, `done`, `failed`, `skipped`, `blocked`
**Execution state values:** `idle`, `running`, `paused`, `done`, `failed`

**Rust type:** `ExecutionStatus`
**TS type:** `ExecutionStatus`

---

## WebSocket Protocol

**Endpoint:** `ws://localhost:3100/ws`

All messages are JSON with a `type` field. After connecting, send a `subscribe` message to filter channels, or receive all events by default.

### Subscribe (Client → Server)

```json
{
  "type": "subscribe",
  "channels": ["agents", "issues", "locks", "execution"]
}
```

Valid channels:
- `"agents"` — heartbeat and agent status events
- `"issues"` — issue created/updated/closed events
- `"locks"` — lock claimed/released events
- `"execution"` — orchestrator stage progress events

**TS type:** `WsSubscribeMessage`

---

### Heartbeat (Server → Client)

Fired when an agent publishes a heartbeat to the hub branch.

```json
{
  "type": "heartbeat",
  "agent_id": "worker-1",
  "timestamp": "2026-03-10T03:20:00Z",
  "active_issue_id": 42
}
```

**TS type:** `WsHeartbeatEvent`

---

### Agent Status (Server → Client)

Fired when an agent's derived status changes (e.g., becomes stale).

```json
{
  "type": "agent_status",
  "agent_id": "worker-1",
  "status": "stale"
}
```

**TS type:** `WsAgentStatusEvent`

---

### Issue Updated (Server → Client)

Fired when an issue is mutated (create, update, close, reopen, label, comment).

```json
{
  "type": "issue_updated",
  "issue_id": 42,
  "field": "status"
}
```

After receiving this, fetch `GET /api/v1/issues/42` for the latest state.

**TS type:** `WsIssueUpdatedEvent`

---

### Lock Changed (Server → Client)

Fired when an issue lock is claimed or released.

```json
{
  "type": "lock_changed",
  "issue_id": 42,
  "action": "claimed",
  "agent_id": "worker-1"
}
```

**TS type:** `WsLockChangedEvent`

---

### Execution Progress (Server → Client)

Fired when an orchestration stage changes status.

```json
{
  "type": "execution_progress",
  "plan_id": "plan-abc123",
  "phase_id": "phase-1",
  "stage_id": "stage-1b",
  "status": "done",
  "agent_id": "worker-2"
}
```

**TS type:** `WsExecutionProgressEvent`

---

## Error Responses

All error responses use the following shape:

```json
{
  "error": "not found",
  "detail": "Issue #999 does not exist"
}
```

| Status | Meaning                                  |
|--------|------------------------------------------|
| `400`  | Bad request (validation error, cycle, etc.) |
| `404`  | Resource not found                       |
| `409`  | Conflict (e.g., duplicate label)         |
| `500`  | Internal server error                    |

**Rust type:** `ApiError`
**TS type:** `ApiError`

---

## Type Cross-Reference

| TypeScript type         | Rust type                  | Source module             |
|-------------------------|----------------------------|---------------------------|
| `Issue`                 | `Issue`                    | `models.rs`               |
| `Comment`               | `Comment`                  | `models.rs`               |
| `Session`               | `Session`                  | `models.rs`               |
| `Milestone`             | `Milestone`                | `models.rs`               |
| `Lock`                  | `Lock`                     | `locks.rs`                |
| `Heartbeat`             | `Heartbeat`                | `locks.rs`                |
| `HealthResponse`        | `HealthResponse`           | `server/types.rs`         |
| `CreateIssueRequest`    | `CreateIssueRequest`       | `server/types.rs`         |
| `UpdateIssueRequest`    | `UpdateIssueRequest`       | `server/types.rs`         |
| `IssueDetail`           | `IssueDetail`              | `server/types.rs`         |
| `IssueSummary`          | `IssueSummary`             | `server/types.rs`         |
| `IssueListResponse`     | `IssueListResponse`        | `server/types.rs`         |
| `CreateCommentRequest`  | `CreateCommentRequest`     | `server/types.rs`         |
| `AddLabelRequest`       | `AddLabelRequest`          | `server/types.rs`         |
| `AddBlockerRequest`     | `AddBlockerRequest`        | `server/types.rs`         |
| `StartSessionRequest`   | `StartSessionRequest`      | `server/types.rs`         |
| `EndSessionRequest`     | `EndSessionRequest`        | `server/types.rs`         |
| `MilestoneSummary`      | `MilestoneSummary`         | `server/types.rs`         |
| `MilestoneDetail`       | `MilestoneDetail`          | `server/types.rs`         |
| `MilestoneListResponse` | `MilestoneListResponse`    | `server/types.rs`         |
| `CreateMilestoneRequest`| `CreateMilestoneRequest`   | `server/types.rs`         |
| `KnowledgePage`         | `KnowledgePage`            | `server/types.rs`         |
| `KnowledgePageSummary`  | `KnowledgePageSummary`     | `server/types.rs`         |
| `KnowledgeSource`       | `KnowledgeSource`          | `server/types.rs`         |
| `AgentSummary`          | `AgentSummary`             | `server/types.rs`         |
| `AgentDetail`           | `AgentDetail`              | `server/types.rs`         |
| `AgentStatus`           | `AgentStatus`              | `server/types.rs`         |
| `LockEntry`             | `LockEntry`                | `server/types.rs`         |
| `SyncStatusResponse`    | `SyncStatusResponse`       | `server/types.rs`         |
| `SyncActionResponse`    | `SyncActionResponse`       | `server/types.rs`         |
| `ConfigResponse`        | `ConfigResponse`           | `server/types.rs`         |
| `UpdateConfigRequest`   | `UpdateConfigRequest`      | `server/types.rs`         |
| `OrchestratorPlan`      | `OrchestratorPlan`         | `server/types.rs`         |
| `OrchestratorPhase`     | `OrchestratorPhase`        | `server/types.rs`         |
| `OrchestratorStage`     | `OrchestratorStage`        | `server/types.rs`         |
| `OrchestratorTask`      | `OrchestratorTask`         | `server/types.rs`         |
| `DecomposeRequest`      | `DecomposeRequest`         | `server/types.rs`         |
| `ExecutionStatus`       | `ExecutionStatus`          | `server/types.rs`         |
| `StageStatus`           | `StageStatus`              | `server/types.rs`         |
| `ExecutionState`        | `ExecutionState`           | `server/types.rs`         |
| `WsMessage`             | (discriminated union)      | `server/types.rs`         |
| `WsHeartbeatEvent`      | `WsHeartbeatEvent`         | `server/types.rs`         |
| `WsAgentStatusEvent`    | `WsAgentStatusEvent`       | `server/types.rs`         |
| `WsIssueUpdatedEvent`   | `WsIssueUpdatedEvent`      | `server/types.rs`         |
| `WsLockChangedEvent`    | `WsLockChangedEvent`       | `server/types.rs`         |
| `WsExecutionProgressEvent` | `WsExecutionProgressEvent` | `server/types.rs`      |
| `WsSubscribeMessage`    | `WsSubscribeMessage`       | `server/types.rs`         |
| `ApiError`              | `ApiError`                 | `server/types.rs`         |
| `OkResponse`            | `OkResponse`               | `server/types.rs`         |
