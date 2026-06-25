---
name: featree
description: Use to create a `feature/<slug>` branch AND move it into a new git worktree under `<repo-root>/.worktrees/<slug>`, then initialize crosslink hooks and an agent identity in the worktree so a child agent can work there. Trigger when the user says "make a worktree for <feature>", "/featree …", or asks to spin up an isolated branch+worktree for a feature.
---

# Featree — feature branch + worktree

The user provides a human-readable feature description (e.g. "add batch retry logic"). First create a feature branch using the `feature` skill, then move it into a new git worktree.

## 1. Create the feature branch

- Invoke the `feature` skill with the user's description as the argument.
- This creates the `feature/<slug>` branch.
- Note the branch name that was created.

## 2. Generate worktree path

- The worktree directory is `<repo-root>/.worktrees/<slug>` (inside the repo, gitignored).
- Extract the slug from the branch name by stripping the `feature/` prefix.
- Create the `.worktrees` directory if it doesn't exist: `mkdir -p <repo-root>/.worktrees`
- Ensure `.worktrees/` is gitignored: check if it's already in `.gitignore`, and if not, append it.

## 3. Create the worktree

- Switch back to the previous branch (the one we were on before `feature` created the new branch): `git checkout -`
- Create the worktree pointing at the feature branch: `git worktree add <worktree-path> feature/<slug>`

## 4. Initialize crosslink in the worktree

After creating the worktree, initialize crosslink so the child agent has proper hooks, skills, and access to shared state:

```bash
# In the worktree directory:
cd <worktree-path>

# Set up crosslink hooks and skills in the worktree
crosslink init --force

# Initialize agent identity for this worktree
# Format: <parent-agent>--<feature-slug>
crosslink agent init <parent-agent>--<feature-slug>

# Sync latest issues from the coordination branch
crosslink sync
```

The agent ID should be derived from the parent agent name and the feature slug. For example, if the parent agent is `m1` and the feature slug is `add-retry`, the agent ID would be `m1--add-retry`.

To get the parent agent name, check `crosslink agent status --json` in the parent repo, or default to the machine hostname.

## 5. Report to user

Print a summary:

```
Worktree: <path>
Branch:   feature/<slug>

To start working:
  cd <worktree-path>
```

## Constraints

- Never force-push or delete branches.
- Do not push the branch to a remote — the user will do that when ready.
- Worktrees MUST be placed inside `<repo-root>/.worktrees/` to inherit the project's Claude Code trust scope and settings hierarchy.
