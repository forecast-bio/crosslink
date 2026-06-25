---
name: feature
description: Use to create a `feature/<slug>` branch from a human-readable description and register a matching crosslink issue (priority medium, label `feature`). Slugifies the description, validates preconditions (no uncommitted changes, branch doesn't already exist), and checks out the new branch. Trigger when the user says "new feature branch for X", "make a feature branch", or "/feature …".
---

# Feature — create a feature branch + crosslink issue

The user will provide a human-readable description of the feature (e.g. "add batch retry logic"). Create a feature branch following the project's naming convention.

## 1. Derive the branch name

- Slugify the description: lowercase, strip non-alphanumeric characters (except hyphens), replace spaces with hyphens, collapse consecutive hyphens.
- The branch name is `feature/<slug>` (e.g. `feature/add-batch-retry-logic`).
- If the slug is empty or the branch already exists, ask the user for a different name.

## 2. Validate preconditions

- Confirm there are no uncommitted changes (other than `.crosslink/issues.db`). If there are, warn the user and ask whether to stash or abort.
- Identify the base branch. Default to the current branch. If the user provides a `--from <ref>` argument, use that instead.

## 3. Create the branch

- `git checkout -b feature/<slug>` (from the resolved base)
- Print the created branch name so the user can confirm.

## 4. Track in crosslink

- Create a crosslink issue for the feature work with the user's original description as the title.
- Set priority to `medium` (unless the user specifies otherwise).
- Use: `crosslink issue create "<description>" -p medium --label feature`

## Constraints

- Never force-push or delete branches.
- Do not push the branch to a remote — the user will do that when ready.
- Keep the slug concise. If the description is very long, truncate to the first 6-8 meaningful words.
