---
allowed-tools: Bash(crosslink *), Bash(git *), Bash(cargo *), Bash(npm *), Bash(npx *), Bash(uv *), Bash(ruff *), Bash(go *), Bash(ls *), Read, Grep
description: Codebase maintenance — dependency audit, lint health, dead code, test gaps, issue hygiene
---

## Context

- Project root: !`git rev-parse --show-toplevel`
- Current branch: !`git branch --show-current`
- Active session: !`crosslink session status`
- Open issues: !`crosslink list -s open --json 2>/dev/null | grep -c '"id"' || crosslink list -s open 2>/dev/null | wc -l`

## Your task

Run a structured codebase maintenance pass. This is a periodic health check — not a pre-commit review. Check each section, report findings, and fix what you can.

### 1. Dependency health

Detect the project's toolchain and audit dependencies:

**Rust** (if `Cargo.toml` exists):
```bash
cargo update --dry-run 2>&1 | head -30
```
Check for outdated or yanked crates. If `cargo-audit` is available:
```bash
cargo audit 2>/dev/null || echo "cargo-audit not installed — skip"
```

**Node** (if `package.json` exists):
```bash
npm outdated 2>/dev/null || echo "npm outdated not available"
npm audit --audit-level=moderate 2>/dev/null || echo "npm audit not available"
```

**Python** (if `pyproject.toml` or `requirements.txt` exists):
```bash
uv pip list --outdated 2>/dev/null || pip list --outdated 2>/dev/null || echo "skip"
```

Report: list any dependencies with known vulnerabilities or major version bumps available.

### 2. Lint and format check

Run the full lint suite without fixing — report-only mode:

**Rust**:
```bash
cargo clippy -- -D warnings 2>&1
cargo fmt --check 2>&1
```

**Node/TypeScript**:
```bash
npx eslint . 2>/dev/null || npm run lint 2>/dev/null
```

**Python**:
```bash
ruff check . 2>/dev/null || uv run ruff check . 2>/dev/null
```

**Go**:
```bash
go vet ./... 2>/dev/null
gofmt -l . 2>/dev/null
```

Count warnings and errors. If any are found, fix them.

### 3. Test suite health

Run the full test suite and assess health:

**Rust**: `cargo test 2>&1`
**Node**: `npm test 2>&1`
**Python**: `uv run pytest 2>/dev/null || pytest 2>/dev/null`
**Go**: `go test ./... 2>/dev/null`

Report:
- Total tests, passed, failed, skipped
- Any flaky tests (if visible from output)
- Tests that are unusually slow

### 4. Dead code and stale patterns

Search the codebase for patterns that indicate maintenance debt:

```
TODO, FIXME, HACK, XXX, DEPRECATED
```

Also search for:
- `#[allow(dead_code)]` or `#[allow(unused)]` in Rust
- `// eslint-disable` or `// @ts-ignore` in TypeScript/JavaScript
- Unused imports (from lint output in step 2)
- Empty `catch` blocks or swallowed errors

For each finding, decide:
- **Fix now** if it's a quick cleanup (remove unused import, delete dead code)
- **File issue** if it requires more work: `crosslink create "<description>" -p low --label maintenance`

### 5. Documentation freshness

Check that key documentation files exist and aren't stale:

- `README.md` — exists?
- `CHANGELOG.md` — has entries for recent work?
- `CLAUDE.md` — exists and reflects current project structure?

Read the first 20 lines of each to assess whether they're current. Flag any that reference features or structures that no longer exist.

### 6. Crosslink issue hygiene

Audit the issue tracker:

```bash
crosslink list -s open
```

Check for:
- **Stale issues**: open issues that haven't been updated recently and may be obsolete
- **Duplicate issues**: multiple issues describing the same work
- **Missing labels**: open issues without category labels
- **Orphaned subissues**: subissues whose parent is already closed

For each finding, suggest an action (close, merge, label, etc.) but do not close issues without user confirmation.

### 7. Build artifact cleanup

Check for build artifacts or temp files that shouldn't be tracked:

```bash
git status --ignored --short 2>/dev/null | head -20
```

Verify `.gitignore` covers common patterns for the detected languages.

### 8. Print maintenance report

Print a summary using this format:

```
Maintenance Report
==================

Dependencies:     [OK | N outdated | N vulnerable]
Lint:             [OK | N warnings | N errors]
Format:           [OK | N files need formatting]
Tests:            [N passed, N failed, N skipped]
Dead code/TODOs:  [OK | N items found]
Documentation:    [OK | N files stale]
Issue hygiene:    [OK | N issues need attention]
Build artifacts:  [OK | N items to clean]

Actions taken:
  - Fixed N lint warnings
  - Removed N dead code items
  - Created N maintenance issues

Recommended follow-ups:
  - <list of items that need human attention>
```

## Constraints

- Do not make breaking changes. Maintenance is conservative — fix warnings, remove dead code, update docs.
- Do not update dependency versions without checking for breaking changes first. Report outdated deps; only update patch versions automatically.
- Do not close crosslink issues without user confirmation — only suggest closures.
- Do not modify test behavior — only fix test infrastructure issues (imports, configs).
- If a fix would touch more than 10 lines, create a crosslink issue for it instead of fixing inline.
