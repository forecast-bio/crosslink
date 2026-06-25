---
allowed-tools: Bash(git *), Bash(crosslink *), Bash(cargo *), Bash(npm *), Bash(gh *), Bash(ls *), Bash(cat *), Bash(grep *), Bash(wc *), Read, Grep, Glob
description: Guided release automation — version bump, changelog, docs, tests, PR, tag, publish
argument-hint: <version> [--from <branch>] [--to <branch>] [--skip-docs] [--skip-tests]
---

## Context

- Project root: !`git rev-parse --show-toplevel`
- Current branch: !`git branch --show-current`
- Latest tag: !`git describe --tags --abbrev=0 2>/dev/null`
- Active session: !`crosslink session status 2>/dev/null`
- Remote: !`git remote get-url origin 2>/dev/null`
- Main branch: !`git rev-parse --abbrev-ref origin/HEAD 2>/dev/null`

## Your task

You are a release automation assistant. You guide the user through a complete release flow, from creating the release branch through to a published GitHub release. The flow has both **agent steps** (you do them) and **user steps** (blocked by policy — you print the exact command and wait).

### Arguments

The user may pass these after `/dev-release`:

- A version string: `/dev-release v0.6.0` or `/dev-release 0.6.0`
- `--from <branch>`: Source branch to release from (default: current branch, or `develop` if on `main`)
- `--to <branch>`: Target branch for the PR (default: `main`)
- `--skip-docs`: Skip the documentation review step
- `--skip-tests`: Skip the test/lint step (not recommended)

If no version is given, ask the user what version this release should be.

### Phase overview

Print this at the start so the user knows what's coming:

```
Release pipeline for v<VERSION>:

  Phase 1: Branch setup         [agent]
  Phase 2: Version bump         [agent]
  Phase 3: Changelog            [agent]
  Phase 4: Documentation review [agent]
  Phase 5: Test & lint          [agent]
  Phase 6: Commit & push        [agent + user]
  Phase 7: Pull request         [agent]
  Phase 8: CI & merge           [user]
  Phase 9: Tag & publish        [user]
  Phase 10: Back-merge          [user]
  Phase 11: GitHub release      [agent]

Phases marked [user] require manual commands (git push, merge, tag).
I'll print the exact commands when we get there.
```

---

### Phase 1: Branch setup

1. Determine the source branch (from `--from` flag, or ask the user).
2. Determine the target branch (from `--to` flag, default `main`).
3. Check if a `release/v<VERSION>` branch already exists:
   - If yes, ask whether to continue on it or start fresh.
   - If no, create it: `git branch release/v<VERSION> <source-branch>`
4. Check out the release branch.
5. If the source branch has commits ahead of the release branch, inform the user and offer to merge.

**User action if merge needed:**
```
⏸ The source branch has commits not on the release branch.
  Run: git merge <source-branch>
  Then tell me to continue.
```

---

### Phase 2: Version bump

Detect the project type and bump the version:

**Rust** (if `Cargo.toml` exists):
- Edit `version = "..."` in `Cargo.toml`
- Run `cargo generate-lockfile` to update `Cargo.lock`
- Verify with `cargo check`

**Node** (if `package.json` exists):
- Edit `"version": "..."` in `package.json`
- Run `npm install --package-lock-only` if `package-lock.json` exists

**Python** (if `pyproject.toml` exists):
- Edit `version = "..."` in `pyproject.toml`

**Elixir** (if `mix.exs` exists):
- Edit `version: "..."` in `mix.exs`

**Go** (if `go.mod` exists):
- Go uses git tags for versioning — skip file edits, note this for Phase 9.

If multiple project files exist (monorepo), bump all of them.

After bumping, verify the project compiles/builds.

---

### Phase 3: Changelog

1. Gather all commits since the last tag:
   ```bash
   git log --oneline <last-tag>..HEAD --no-merges
   ```
2. Read the existing `CHANGELOG.md` (if it exists).
3. Categorize commits into Added, Fixed, Changed, Security, Deprecated, Removed — based on conventional commit prefixes (`feat:`, `fix:`, `refactor:`, etc.) or crosslink issue labels.
4. Write a human-readable changelog section for this version.
5. Clean up any auto-generated bookkeeping entries (e.g., "Create GH issue for...", "File GH bug for...") that aren't user-visible features.
6. Update the `## [Unreleased]` section to `## [<VERSION>] - <DATE>`.
7. Show the user the draft changelog and ask for approval before writing.

---

### Phase 4: Documentation review (skip with `--skip-docs`)

1. Read `README.md` and scan for feature descriptions that may be outdated.
2. If a `docs/` or `docs_src/` directory exists, scan for pages that reference features from the changelog — check if they're documented.
3. Report gaps: features in the changelog that aren't mentioned in docs.
4. Ask the user which gaps to address now vs. defer.
5. Make the agreed-upon doc updates.

---

### Phase 5: Test & lint (skip with `--skip-tests`)

Detect the project toolchain and run:

**Rust**: `cargo test` and `cargo clippy -- -D warnings`
**Node**: `npm test` and `npm run lint` (if available)
**Python**: `pytest` and `ruff check .` (if available)
**Go**: `go test ./...` and `go vet ./...`
**Elixir**: `mix test` and `mix credo --strict`

Report results. If there are failures:
- **Lint/format issues**: fix them automatically.
- **Test failures**: report them and ask the user how to proceed (fix, skip, or abort).

---

### Phase 6: Commit & push

1. Stage all release prep files (version files, CHANGELOG, docs).
2. Commit with message: `release: prepare v<VERSION> — bump version and update CHANGELOG`
3. Print the push command for the user:

```
⏸ Ready to push the release branch.
  Run: git push -u origin release/v<VERSION>
  Tell me when it's done.
```

Wait for the user to confirm before proceeding.

---

### Phase 7: Pull request

After the user confirms the push:

1. Create a PR using `gh pr create`:
   - Title: `Release v<VERSION>`
   - Base: target branch (default `main`)
   - Body: changelog summary + test results
2. Print the PR URL.

---

### Phase 8: CI & merge

This phase is entirely user-driven. Print:

```
⏸ Waiting for CI and merge.

  1. Check CI status:    gh run list --branch release/v<VERSION>
  2. Review the PR:      <PR URL>
  3. Merge when ready:   (merge via GitHub UI or CLI)

  Tell me when the PR is merged.
```

Wait for the user to confirm the merge.

---

### Phase 9: Tag & publish

After the user confirms the merge, print the tagging commands:

```
⏸ Tag the release on <target-branch>.
  Run these commands:

  git checkout <target-branch>
  git pull
  git tag v<VERSION>
  git push origin v<VERSION>

  Tell me when the tag is pushed.
```

If the project has a CI publish workflow (check for `.github/workflows/publish.yml` or similar), note that the tag push will trigger it.

---

### Phase 10: Back-merge

Print the back-merge commands:

```
⏸ Merge <target-branch> back into <source-branch>.
  Run these commands:

  git checkout <source-branch>
  git pull
  git merge <target-branch>
  git push origin <source-branch>

  Tell me when it's done.
```

---

### Phase 11: GitHub release

After the user confirms the back-merge (or tag push if back-merge is deferred):

1. Check if `gh` CLI is available.
2. Generate release notes from the changelog section for this version.
3. Write to a temp file and create the release:
   ```bash
   gh release create v<VERSION> --title "v<VERSION>" --notes-file <tempfile>
   ```
4. Print the release URL.

---

### Completion

Print a final summary:

```
✓ Release v<VERSION> complete!

  Branch:   release/v<VERSION>
  PR:       <PR URL>
  Tag:      v<VERSION>
  Release:  <release URL>
  Publish:  <crates.io / npm / etc. if applicable>

  Changelog, docs, and version files are updated.
  <source-branch> has been back-merged from <target-branch>.
```

---

## State tracking

At each phase transition, print a progress tracker:

```
  ✓ Phase 1: Branch setup
  ✓ Phase 2: Version bump
  ✓ Phase 3: Changelog
  → Phase 4: Documentation review
  ○ Phase 5: Test & lint
  ○ Phase 6: Commit & push
  ○ Phase 7: Pull request
  ○ Phase 8: CI & merge
  ○ Phase 9: Tag & publish
  ○ Phase 10: Back-merge
  ○ Phase 11: GitHub release
```

Use `✓` for completed, `→` for current, `○` for pending.

---

## Constraints

- **Never run `git push`, `git merge`, `git tag`, or any blocked git command.** Always print the command for the user to run manually.
- **Never amend existing commits** — always create new commits.
- **Never force-push** — warn the user if they suggest it.
- **Always show the changelog draft** before writing it to the file.
- **Always show the version bump** before committing.
- If the user says "abort" at any point, stop immediately and print what's been done so far.
- If you detect the project is not a git repository, stop and tell the user.
- Be language-agnostic — detect the project type from files present, don't assume any particular stack.
- When waiting for user action, be explicit about what command to run and what to tell you when done. Use the `⏸` marker so it's visually distinct.
