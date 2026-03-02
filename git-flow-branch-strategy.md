---
title: Git Flow Branch Strategy
tags: [git, workflow, ci-cd, branching]
sources:
  - url: https://github.com/forecast-bio/crosslink/issues/129
    title: 
    accessed_at: 2026-03-02
contributors: [maxine-at-forecast--noether--ci-cd-restructure]
created: 2026-03-02
updated: 2026-03-02
---

# Git Flow Branch Strategy

Crosslink uses a tiered git flow pattern to balance agent autonomy with quality gates.

## Branch Layout

```
main <---- release/v0.x.y <---- develop <---- feature/some-work
  |                                 ^
  +-- hotfix/critical-fix ----------+
```

## Branch Tiers

| Branch | Protection | CI | Purpose |
|--------|-----------|-----|---------|
| main | Strict (require PR, CI pass, no force push, no deletion) | Full suite + release publish | Production releases only |
| develop | Moderate (require PR, CI pass, no force push, no deletion) | Full test suite + clippy + fmt + cross-platform | Integration branch, all features merge here |
| feature/* | None | Fast checks only (build + unit tests) | Agent working branches, free to push |
| release/* | Moderate (CI pass, no force push, no deletion) | Full suite + dry-run publish | Release candidates |
| hotfix/* | Moderate (CI pass, no force push, no deletion) | Full suite | Emergency fixes branched from main |

## Default Branch

develop is the default branch. All PRs target develop unless explicitly targeting main (releases/hotfixes).

## Agent Workflow

1. /feature creates branches from develop
2. /kickoff launches agents that work on feature/* branches
3. Agents push freely to feature/* (no protection)
4. PRs from feature/* to develop require CI pass
5. /release creates release/* from develop, opens PR to main
6. Tag on main triggers crates.io publish + GitHub release

## CI Tiering

- Feature branches (ci-feature.yml): Build + unit tests only (~2 min)
- develop/release/hotfix (ci.yml): Full suite -- lint, security audit, tests on ubuntu + macos, proptests, fuzz, release builds
- main + tags (publish.yml): Full suite + crates.io publish + GitHub release with binary artifacts

## GitHub Rulesets (configured)

- main -- strict production protection: PR required, CI (Lint + Tests x2), no force push, no deletion
- develop -- integration branch protection: PR required, CI (Lint + Tests x2), no force push, no deletion
- release/* -- release candidate protection: CI (Lint + Tests x2), no force push, no deletion
- hotfix/* -- emergency fix protection: CI (Lint + Tests x2), no force push, no deletion
- feature/*: No rules (agents push freely)

## Reference

- GitHub Issue: https://github.com/forecast-bio/crosslink/issues/129
- Setup completed: 2026-03-02
