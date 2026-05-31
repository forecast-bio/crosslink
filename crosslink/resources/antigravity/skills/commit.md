---
allowed-tools: Bash(git *), Bash(crosslink *)
description: Commit changes and auto-document the result on the active crosslink issue
---

## Context

- Working tree status: !`git status --short`
- Current branch: !`git branch --show-current`
- Active session: !`crosslink session status 2>/dev/null`

## Your task

The user wants to commit their current changes. You will create a well-formed git commit AND automatically record a result comment on the active crosslink issue.

### 1. Review changes

Run `git diff --cached --stat` and `git diff --stat` to see staged and unstaged changes. If nothing is staged, stage the relevant files (ask the user if unclear which files to include). Never use `git add -A` blindly — stage specific files.

### 2. Write the commit message

- Summarize what changed and why (1-2 sentences)
- Follow conventional commit style if the project uses it
- Include the crosslink issue reference if an active issue exists (e.g. `[CL-5]`)

### 3. Create the commit

```bash
git commit -m "<message>"
```

### 4. Auto-document the result on the active crosslink issue

After a successful commit, check if there's an active crosslink session with an active issue:

```bash
crosslink session status
```

If an active issue exists, record the commit as a result comment:

```bash
crosslink issue comment <issue-id> "Committed: <first line of commit message> | Files: <shortstat summary>" --kind result
```

For example:
```bash
crosslink issue comment 5 "Committed: Add typed comment support to schema | Files: 14 files changed, 312 insertions(+), 48 deletions(-)" --kind result
```

If no active session or issue, skip the comment silently.

### 5. Show summary

Display:
- The commit hash and message
- Files changed summary
- Whether the result was recorded on a crosslink issue

## Constraints

- Never force-push or amend commits without explicit user request.
- Never use `git add -A` or `git add .` without confirming with the user.
- Always record the result comment after a successful commit when an active issue exists.
- If the commit fails (e.g. pre-commit hook), fix the issue and retry — do NOT record a result comment for failed commits.
