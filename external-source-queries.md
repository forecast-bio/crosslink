---
title: "Allow crosslink knowledge and issue commands to query external sources"
tags: [design-doc]
sources: []
contributors: [maxine--basel]
created: 2026-03-17
updated: 2026-03-17
---

# Feature: Allow crosslink knowledge and issue commands to query external sources

## Summary

Enable `crosslink knowledge` and `crosslink issue` read commands to query data from other repositories — either by fetching a remote repo's `crosslink/knowledge` and `crosslink/hub` branches, or by reading from another local repo's `.crosslink` data. This enables agent-to-agent knowledge transfer: an agent in repo A can query the crosslink trail from repo B to understand how and why code was built.

## Requirements

- REQ-1: `crosslink knowledge search/show/list` must accept a flag to specify an external repository, resolving it to a fetchable `crosslink/knowledge` branch and reading pages from the fetched tree.
- REQ-2: `crosslink issue search/show/list` must accept the same flag, resolving to a fetchable `crosslink/hub` branch and reading `IssueFile` JSON from the `issues/` directory.
- REQ-3: External queries must be strictly read-only — no writes, no pushes, no modifications to the external repo's data.
- REQ-4: The CLI flag must not collide with the existing `--source` flag on `knowledge search` (which filters by source URL domain via `KnowledgeManager::search_sources()`). A new flag name is required.
- REQ-5: Remote data must be cached locally with configurable TTLs (5-minute default for data, 24-hour default for resolved URLs) to avoid repeated fetches. Cache location must be under `.crosslink/` and isolated per external source. A `--refresh` flag must bypass the TTL for on-demand re-fetching.
- REQ-6: External data must be visually distinguished from local data in CLI output — via a labeled header/footer for human output, a prefix for individual results, and a `source` field in `--json` output.
- REQ-7: Named aliases for frequently-used external sources must be configurable, so users can write `--repo @upstream` instead of a full URL.
- REQ-8: Authentication must leverage existing git credentials — no new credential management.
- REQ-9: The MCP knowledge server (`.claude/mcp/knowledge-server.py`) must gain a `source` parameter on its `search_knowledge` tool, passing through to `crosslink knowledge search --repo <value>`.
- REQ-10: The REST API (`server/handlers/`) must NOT gain external source support — it serves only local repository data.

## Acceptance Criteria

- [ ] AC-1: `crosslink knowledge search "auth" --repo github.com/org/other-repo` fetches the remote's `crosslink/knowledge` branch, caches it locally, and returns matching pages with a `--- Results from github.com/org/other-repo ---` banner (validates REQ-1, REQ-6).
- [ ] AC-2: `crosslink knowledge show page-name --repo /path/to/local/repo` reads from the local repo's knowledge cache and displays the page (validates REQ-1).
- [ ] AC-3: `crosslink issue search "migration" --repo github.com/org/other-repo` fetches the remote's `crosslink/hub` branch, deserializes `issues/*.json` using `read_all_issue_files()`, and returns matching issues (validates REQ-2).
- [ ] AC-4: `crosslink issue show 42 --repo github.com/org/other-repo` displays the issue with display_id 42 from the external hub data (validates REQ-2).
- [ ] AC-5: `crosslink issue list -s closed --repo /path/to/local/repo` lists closed issues from the external source (validates REQ-2).
- [ ] AC-6: Running `crosslink knowledge add "page" --repo github.com/org/other-repo` produces a clear error: "External sources are read-only" (validates REQ-3).
- [ ] AC-7: `crosslink knowledge search "auth" --source rust-lang.org --repo github.com/org/other-repo` correctly combines both flags: searches external pages filtered by source URL domain (validates REQ-4).
- [ ] AC-8: A second invocation of `--repo github.com/org/other-repo` within the 5-minute data TTL window does not trigger a `git fetch`; adding `--refresh` forces a fetch regardless of TTL (validates REQ-5).
- [ ] AC-13: A shorthand `--repo github.com/org/repo` probes HTTPS then SSH via `git ls-remote`, caches the resolved URL in `meta.json`, and subsequent fetches within the 24-hour URL TTL reuse the cached URL without re-probing (validates REQ-8).
- [ ] AC-9: `crosslink config set repo-alias.upstream github.com/org/other-repo` followed by `crosslink knowledge search "auth" --repo @upstream` works identically to using the full URL (validates REQ-7).
- [ ] AC-10: `--json` output includes `"source": "github.com/org/other-repo"` on each result object; `--quiet` output omits the banner but preserves the data (validates REQ-6).
- [ ] AC-11: The MCP tool `search_knowledge` accepts an optional `source` parameter and returns results from the specified external repo (validates REQ-9).
- [ ] AC-12: REST API endpoints (`GET /api/v1/knowledge`, `GET /api/v1/issues`) do not accept a source/repo parameter and continue to serve only local data (validates REQ-10).

## Architecture

### Flag naming: `--repo`

The existing `--source` flag on `knowledge search` (defined at `main.rs:1167`, dispatched to `KnowledgeManager::search_sources()` at `knowledge.rs:612`) means "filter by source URL domain." To avoid collision, the external source flag is named `--repo`. This is:
- Unambiguous: it clearly refers to a repository, not a metadata filter
- Composable: `--source` and `--repo` can be used together (AC-7)
- Consistent: the flag refers to the same concept across both `knowledge` and `issue` commands

The `--repo` flag is added to: `KnowledgeCommands::Search`, `KnowledgeCommands::Show`, `KnowledgeCommands::List`, `IssueCommands::Search`, `IssueCommands::Show`, `IssueCommands::List` in `main.rs`.

### Source resolution

A `--repo` value is resolved in this order:

1. **Named alias**: If the value starts with `@`, look up `repo-alias.<name>` in `crosslink config` (stored in `.crosslink/config.toml`). Example: `@upstream` → `github.com/org/other-repo`.
2. **Local path**: If the value is a path that exists on disk and contains `.crosslink/` or `.git/`, treat it as a local repository.
3. **Git URL**: Otherwise, treat it as a git remote URL. For shorthand like `github.com/org/repo`, resolve to a fetchable URL using the HTTPS-first-then-SSH protocol probe (see below).

This resolution logic lives in a new module `src/external.rs`.

### URL shorthand resolution

When a `--repo` value looks like a shorthand (e.g., `github.com/org/repo` — no scheme, no `git@` prefix), resolve it to a fetchable URL using a lightweight protocol probe, consistent with how git itself treats ambiguous URLs:

1. Probe `git ls-remote https://github.com/org/repo` (fast — only checks refs, no data transfer)
2. If HTTPS probe fails, probe `git ls-remote git@github.com:org/repo.git`
3. Whichever succeeds first, use that URL for the actual `git fetch`
4. Cache the resolved URL in `meta.json` under `resolved_url` so subsequent fetches skip the probe entirely

Values that are already fully qualified (`https://...`, `git@...:...`) skip the probe and are used directly.

The probe uses `git ls-remote` with a short timeout (5 seconds) to avoid blocking on unreachable hosts. If both probes fail, the error message includes both attempted URLs so the user can diagnose credential or network issues.

### Named aliases

Aliases are managed via `crosslink config`:

```bash
crosslink config set repo-alias.upstream github.com/forecast-bio/other-repo
crosslink config set repo-alias.ml-core /Users/maxine/code/forecast/ml-core
crosslink config list repo-alias    # show all aliases
crosslink config unset repo-alias.upstream
```

This uses the existing `crosslink config` infrastructure (`commands/config.rs`) which stores key-value pairs in `.crosslink/config.toml`. The `repo-alias` namespace is a convention — no schema changes needed. This is preferable to `hook-config.json` because:
- It's discoverable via `crosslink config list`
- It uses the same set/get/unset verbs users already know
- `hook-config.json` is for hook behavior, not user preferences

### External data access: knowledge

For knowledge queries against an external source:

1. **Resolve source** → git URL or local path
2. **Fetch/update cache**: For remote URLs, `git fetch <url> crosslink/knowledge` into a bare ref under `.crosslink/.external-cache/<hash>/knowledge/`. For local repos, read directly from their `.crosslink/.knowledge-cache/` (or set up a worktree from their branch).
3. **Construct a `KnowledgeManager`-like reader**: Rather than modifying `KnowledgeManager::new()` (which is tightly coupled to the local repo root at `knowledge.rs:136-155`), introduce an `ExternalKnowledgeReader` that takes a cache directory path and exposes `list_pages()`, `search_content()`, `search_sources()`, and `show_page()`. These can largely reuse the existing parsing functions (`parse_frontmatter()` at `knowledge.rs:708`, `search_content()` at `knowledge.rs:523`) by extracting them into standalone functions that operate on a directory path.
4. **Return results** with source annotation.

### External data access: issues

For issue queries against an external source:

1. **Resolve source** → git URL or local path
2. **Fetch/update cache**: `git fetch <url> crosslink/hub` into `.crosslink/.external-cache/<hash>/hub/`.
3. **Read issue files directly**: Use `read_all_issue_files()` from `issue_file.rs` against the cached hub tree's `issues/` directory. This returns `Vec<IssueFile>` — the same struct used for local hydration (defined at `issue_file.rs:13-45`).
4. **Search/filter in memory**: All search and filtering is done in-memory over `Vec<IssueFile>` — no temporary SQLite hydration. This is the permanent approach, not a stepping stone. Per-repo issue trackers rarely exceed a few hundred issues, making in-memory operations effectively instant.
   - `search`: case-insensitive substring match on title, description, and comment content (mirroring `db.rs:1087-1108` semantics with `str::to_lowercase()` + `str::contains()`)
   - `list`: filter by status, label, priority (mirroring `db.rs:489-539` field checks)
   - `show`: find by display_id; always display inline comments (consistent with local `crosslink issue show` behavior — the decision trail in comments is the primary reason to query external issues)
5. **Return results** with source annotation.

### Cache management

External source data is cached under `.crosslink/.external-cache/`:

```
.crosslink/.external-cache/
  <sha256-of-repo-url>/
    knowledge/     # bare checkout of crosslink/knowledge branch
    hub/           # bare checkout of crosslink/hub branch
    meta.json      # { "url": "...", "resolved_url": "...", "last_fetched": "...", "ttl_seconds": 300 }
```

**TTL strategy** — two separate TTLs with sensible defaults:

| Cache type | Default TTL | Config key | Rationale |
|---|---|---|---|
| Data (knowledge pages, issue JSON) | 5 minutes | `external-cache-ttl` | Balance freshness with fetch cost. Agents querying mid-session get reasonably current data without hammering remotes. |
| URL resolution (HTTPS vs SSH probe result) | 24 hours | `external-url-ttl` | Protocol availability rarely changes. The probe is the slowest part of a cold fetch — caching aggressively here eliminates the most noticeable latency. |

Both are configurable via `crosslink config set <key> <seconds>`.

- `meta.json` tracks `last_fetched` (data TTL) and `resolved_url` (URL TTL) independently
- When data TTL expires, re-fetch using the cached `resolved_url` (no re-probe)
- When URL TTL expires, re-probe on the next fetch
- `--repo` with `--refresh` flag forces a fetch regardless of TTL (useful for debugging stale data)
- For local paths, no caching — read directly from the source repo's existing worktree/cache
- Cache can be inspected with `crosslink config list external-cache` and cleared by deleting `.crosslink/.external-cache/`

### Output formatting

**Human output (default)**:
```
--- Results from github.com/org/other-repo ---

  auth-middleware (line 12):
    11 | The auth middleware validates JWT tokens...
    12 | ...using the RS256 algorithm with key rotation.
    13 |

--- End external results ---
```

**JSON output (`--json`)**:
Each result object gains a `"source"` field:
```json
{
  "slug": "auth-middleware",
  "line_number": 12,
  "context_lines": ["..."],
  "source": "github.com/org/other-repo"
}
```

**Quiet output (`--quiet`)**: No banner, just data. Source info only in `--json`.

### New module: `src/external.rs`

This module contains:
- `resolve_repo(value: &str, config: &Config) -> Result<RepoSource>` — alias/path/URL resolution
- `enum RepoSource { Local(PathBuf), Remote(String) }`
- `ExternalKnowledgeReader` — reads knowledge pages from an arbitrary directory
- `ExternalIssueReader` — reads and filters `IssueFile` structs from an arbitrary `issues/` directory
- `ExternalCache` — manages fetch, dual-TTL (data + URL resolution), `--refresh` bypass, and cache directory lifecycle
- `fn probe_url(shorthand: &str) -> Result<String>` — HTTPS-first, SSH-fallback protocol probe via `git ls-remote`
- Cache hashing: `sha256(canonical_url)` truncated to 16 hex chars for directory name

### MCP integration

Update `.claude/mcp/knowledge-server.py`:
- Add `source` parameter to the `search_knowledge` tool definition
- When `source` is provided, pass `--repo <value>` to the `crosslink knowledge search` CLI call
- Add a new tool `search_external_issues` with parameters `query` (required) and `source` (required)

### Commands that reject `--repo`

Write commands (`knowledge add/edit/remove/sync/import`, all `issue` mutation commands) reject `--repo` with error: `"External sources are read-only. The --repo flag is only supported on read commands."` This is enforced at the clap argument level using `conflicts_with` on the write-specific args, or at dispatch time in the command handler.

## Resolved Questions

### Q1: Should `crosslink issue show` display comments from external issues?

**Decision: Always show comments.** Consistent with local `crosslink issue show` behavior. The decision trail in comments (`--kind decision`, `--kind plan`, etc.) is the primary reason agents query external issues. Output length is not a concern for programmatic callers, and human users can filter with `--json | jq`.

### Q2: Should external issue search hydrate into a temporary SQLite for complex queries?

**Decision: In-memory only, permanently.** Per-repo issue trackers rarely exceed a few hundred issues. In-memory `to_lowercase()` + `contains()` matching over `Vec<IssueFile>` is effectively instant at this scale. Avoids coupling to `hydrate()` from `hydration.rs`, which expects to own schema lifecycle (migrations, counters) and would require significant adaptation for throwaway temp databases.

### Q3: What `github.com/org/repo` shorthand expansion should be used?

**Decision: HTTPS-first with SSH fallback, consistent with git's own transport behavior.** Use `git ls-remote` as a lightweight probe to determine which protocol works, then cache the resolved URL in `meta.json` for 24 hours. Fully-qualified URLs (`https://...`, `git@...:...`) skip the probe. See "URL shorthand resolution" section in Architecture.

## Out of Scope

- Writing to external sources (creating issues, adding knowledge pages remotely)
- REST API support for external sources (the web dashboard serves local data only)
- Bidirectional sync or replication between repositories
- External source support for non-read commands (timer, session, milestone, etc.)
- Automatic discovery of related repositories (must be explicitly specified)
- External source support for hub branch mutations (locks, agent state, events)
