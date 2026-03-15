---
title: "Dashboard Extraction — Standalone Multi-Repo Web Dashboard"
tags: [design-doc]
sources: []
contributors: [maxine--basel]
created: 2026-03-15
updated: 2026-03-15
---


## Design Specification

### Summary

Extract the crosslink web dashboard from the monorepo into a standalone repository, re-architecting it as a multi-repo-aware application that can connect to multiple crosslink backends simultaneously. This enables organizations to host a single dashboard instance that monitors all their crosslink-instrumented repositories, with proper authentication, a typed API client, and a test suite built from day one.

### Requirements

- REQ-1: Extract `dashboard/` into its own repository with independent CI, versioning, and release cycle, preserving git history for the dashboard directory
- REQ-2: Introduce a multi-repo context system where the dashboard can connect to N crosslink backends, each identified by a user-configured name and URL
- REQ-3: Replace the hardcoded `/api/v1` base path with a repo-scoped API client that routes all requests through the correct backend URL per repo
- REQ-4: Add a repo switcher UI in the sidebar and a cross-repo aggregation dashboard (total issues, agents across all repos, unified search)
- REQ-5: Implement an authentication layer supporting API keys (for headless/CI) and OAuth (for interactive users), enforced on both REST and WebSocket connections
- REQ-6: Build a typed API client generated from the Rust server's handler types, replacing the current raw `fetch` calls with compile-time type safety
- REQ-7: Establish a test suite covering: Vitest unit tests for stores and utilities, React Testing Library component tests, Playwright E2E tests against a mock API
- REQ-8: Add a backend gateway mode to the Rust `crosslink serve` command that can proxy requests to multiple repo backends, enabling single-deployment scenarios
- REQ-9: Implement WebSocket multiplexing so a single dashboard connection can receive real-time events from multiple repo backends
- REQ-10: Add offline resilience: event queue for client-to-server actions during outage, reconnection state recovery, and visual offline indicator

### Acceptance Criteria

- [ ] AC-1: Dashboard repository builds independently with `npm run build`; CI runs lint, type-check, unit tests, and E2E tests; `npm run dev` connects to a configurable backend URL via environment variable (validates REQ-1)
- [ ] AC-2: `useRepoContext()` hook provides current repo ID; repo list is persisted in localStorage; adding/removing repos does not require page reload (validates REQ-2)
- [ ] AC-3: Zero hardcoded `/api/v1` strings remain in `src/`; all API calls route through `createApiClient(repoConfig)` which prefixes the correct base URL; switching repos updates all active store subscriptions (validates REQ-3)
- [ ] AC-4: Sidebar shows repo selector dropdown with colored indicators (connected/disconnected); "All Repos" view shows aggregated issue counts, agent statuses, and a unified search that queries all connected backends (validates REQ-4)
- [ ] AC-5: Unauthenticated API requests return 401; dashboard login flow stores JWT in httpOnly cookie; API key auth supported via `X-Crosslink-Token` header; WebSocket upgrade requires valid auth (validates REQ-5)
- [ ] AC-6: Typed API client in `src/api/client.gen.ts` is generated from `crosslink/src/server/types.rs` via a build script; type errors in API calls are caught at compile time; no `any` types in API layer (validates REQ-6)
- [ ] AC-7: `npm test` runs 50+ unit tests (stores, utilities, API client); `npm run test:e2e` runs 20+ Playwright tests against MSW mock server; CI runs both on every PR (validates REQ-7)
- [ ] AC-8: `crosslink serve --gateway` accepts a config file mapping repo names to backend URLs; proxies `/repos/{name}/api/v1/*` to the correct backend; health endpoint aggregates all backend statuses (validates REQ-8)
- [ ] AC-9: Single WebSocket connection to gateway receives events tagged with `repo_id`; `WsClient` dispatches events to the correct repo's store; connection count is 1 (gateway mode) or N (direct mode) (validates REQ-9)
- [ ] AC-10: Actions performed while offline are queued in IndexedDB; queue drains automatically on reconnect; visual indicator shows "offline (N actions pending)"; queued actions are idempotent (validates REQ-10)

### Architecture

### Current State

The dashboard lives in `dashboard/` with:
- 15 page components in `src/pages/`
- 28 domain components in `src/components/`
- 5 Zustand stores in `src/stores/` (agents, issues, orchestrator, usage, theme)
- Raw fetch API client in `src/api/client.ts` with 34 endpoints hardcoded to `/api/v1`
- WebSocket client in `src/api/ws.ts` connecting to `/ws`
- Vite build with dev proxy to `localhost:3100`
- Zero tests, no auth, single-repo assumption throughout

### Target Architecture

```
crosslink-dashboard/                  (standalone repo)
├── src/
│   ├── api/
│   │   ├── client.gen.ts             # Generated typed API client
│   │   ├── gateway.ts                # Gateway WebSocket multiplexer
│   │   ├── ws.ts                     # Per-repo WebSocket client (refactored)
│   │   └── types.ts                  # Shared API types (generated)
│   ├── auth/
│   │   ├── AuthProvider.tsx           # Auth context + login flow
│   │   ├── ProtectedRoute.tsx         # Route guard
│   │   └── useAuth.ts                # Auth state hook
│   ├── repos/
│   │   ├── RepoProvider.tsx           # Multi-repo context
│   │   ├── RepoSwitcher.tsx           # Sidebar repo selector
│   │   ├── useRepoContext.ts          # Current repo hook
│   │   └── repoStore.ts              # Repo list persistence
│   ├── stores/                        # Existing stores, scoped per repo
│   │   ├── agents.ts                  # agents[repoId] state
│   │   ├── issues.ts                  # issues[repoId] state
│   │   ├── orchestrator.ts            # orchestrator[repoId] state
│   │   ├── usage.ts                   # usage[repoId] state
│   │   └── theme.ts                   # Global (not repo-scoped)
│   ├── pages/                         # Existing pages, receive repoId from context
│   ├── components/                    # Existing components, unchanged
│   └── __tests__/                     # Test files co-located
├── e2e/                               # Playwright E2E tests
├── mocks/                             # MSW mock handlers
└── scripts/
    └── generate-types.ts              # Type generation from Rust types
```

### Multi-Repo Data Flow

**Direct mode** (dashboard connects to N backends):
```
Dashboard
  ├── RepoProvider (manages repo list from localStorage)
  │   ├── Repo A: WsClient("wss://backend-a.example.com/ws")
  │   ├── Repo B: WsClient("wss://backend-b.example.com/ws")
  │   └── Repo C: WsClient("ws://localhost:3100/ws")
  └── Stores
      ├── agents: { "repo-a": [...], "repo-b": [...], "repo-c": [...] }
      ├── issues: { "repo-a": [...], "repo-b": [...], "repo-c": [...] }
      └── ...
```

**Gateway mode** (dashboard connects to 1 gateway that proxies N backends):
```
Dashboard ──ws──> Gateway (crosslink serve --gateway)
                    ├── Backend A (crosslink serve --port 3101)
                    ├── Backend B (crosslink serve --port 3102)
                    └── Backend C (crosslink serve --port 3103)

Gateway config (gateway.toml):
  [[repos]]
  name = "frontend"
  url = "http://localhost:3101"

  [[repos]]
  name = "backend"
  url = "http://localhost:3102"
```

### Store Scoping Strategy

Current stores hold flat state (`issues: Issue[]`). Refactored stores hold per-repo state:

```typescript
// Current (single-repo)
interface IssuesState {
  issues: Issue[];
  fetch: (params?) => Promise<void>;
}

// Target (multi-repo)
interface IssuesState {
  byRepo: Record<string, Issue[]>;
  fetch: (repoId: string, params?) => Promise<void>;
  allIssues: () => Issue[];  // Flattens all repos for aggregation
}
```

The `useRepoContext()` hook provides the current `repoId` so page components can remain largely unchanged — a thin wrapper reads from context:

```typescript
// Convenience hook used by pages
function useCurrentIssues() {
  const { repoId } = useRepoContext();
  const { byRepo, fetch } = useIssuesStore();
  return {
    issues: byRepo[repoId] ?? [],
    fetch: (params?) => fetch(repoId, params),
  };
}
```

### Typed API Client Generation

The Rust server defines response types in `crosslink/src/server/types.rs`. A build script generates TypeScript types:

1. **Rust side**: Add `#[typeshare]` attribute to all API response types (via `typeshare` crate)
2. **Build step**: `typeshare --lang=typescript --output-file=dashboard/src/api/types.gen.ts crosslink/src/`
3. **Dashboard side**: `scripts/generate-client.ts` reads generated types and produces `client.gen.ts` with typed methods per endpoint
4. **CI check**: If Rust types change without regenerating TS types, CI fails

### Authentication Architecture

Two auth modes, selected by deployment:

1. **API Key** (headless/CI): `X-Crosslink-Token: <key>` header on every request. Keys stored in `crosslink config set auth.api_keys`. Simple, stateless.

2. **OAuth/OIDC** (interactive): Dashboard redirects to identity provider. Server validates JWT. Token stored in httpOnly cookie. Refresh token rotates automatically.

The Rust server gains an auth middleware layer:
- `crosslink serve --auth none` (default, current behavior for local use)
- `crosslink serve --auth api-key --api-key-file keys.txt`
- `crosslink serve --auth oidc --oidc-issuer https://...`

### WebSocket Multiplexing

**Gateway mode**: Single WebSocket connection carries events from all repos, tagged:
```json
{"repo": "frontend", "channel": "issues", "event": "issue_updated", "data": {...}}
```

**Direct mode**: One WebSocket per repo. `WsMultiplexer` class manages connections:
```typescript
class WsMultiplexer {
  private clients: Map<string, WsClient>;
  connect(repoId: string, url: string): void;
  disconnect(repoId: string): void;
  on(handler: (repoId: string, event: WsEvent) => void): void;
}
```

### Test Architecture

**Unit tests (Vitest)**: Store tests (state transitions, multi-repo scoping, aggregation), API client tests (URL construction, error handling), auth tests (token storage, refresh, expiry).

**Component tests (React Testing Library)**: Render each page with mock store state, verify user interactions, test repo switcher, test offline indicator.

**E2E tests (Playwright)**: MSW provides API stubs. Login flow, repo selection, issue creation, multi-repo switching, offline simulation.

### Out of Scope

- Mobile/responsive layout redesign (the current desktop-first layout is adequate)
- Real-time collaborative editing (Google Docs-style concurrent dashboard users)
- Dashboard plugin system or extension API
- Custom dashboard themes beyond the existing HSL color picker
- Server-side rendering (SSR) — the SPA model is fine for this use case
- Migrating away from React (the current stack is modern and well-supported)
- Crosslink CLI changes beyond the gateway mode addition
- VS Code extension dashboard integration

