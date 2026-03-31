---
title: "Knowledge Index"
tags: ["index"]
sources: []
contributors: ["maxine--basel"]
created: 2026-03-17
updated: 2026-03-31
---


# Knowledge Index

Shared knowledge repository for crosslink. Search with \`crosslink knowledge search <topic>\` or browse by tag.

## Onboarding (start here)

- **what-is-crosslink** — What crosslink is, why it exists, first/last actions in any session
- **agent-workflow-patterns** — Standard session, design-first, multi-agent, investigation, review patterns
- **data-model-overview** — Entity relationships, issue lifecycle, comment kinds, priority levels
- **command-taxonomy** — When to use which command, grouped by purpose
- **coordination-practical-guide** — Hub branch, sync, locking, conflict resolution in practice

## Conventions (how we build)

- **error-handling-conventions** — Three categories of \`let _ =\`, transaction boundaries, unsafe defaults
- **module-size-conventions** — 1200-line limit, decomposition strategy, god file inventory
- **testing-strategy** — Five test tiers, coverage gaps, conventions for new tests
- **migration-conventions** — Two-era migration system (v1-v15 legacy, v16+ proper runner)
- **observability-conventions** — tracing stack, log levels, no-eprintln rule

## Architecture

- **event-sourced-coordination** — Deep CRDT design for multi-agent state sharing
- **dual-state-architecture-gotchas** — Hub/SQLite consistency pitfalls and rules of thumb
- **signing-trust-design** — SSH signing, agent key isolation, trust model
- **container-agents** — Container-based agent execution design
- **web-dashboard** — Web dashboard architecture and API surface

## Process

- **git-flow-branch-strategy** — Branch tiers, CI tiering, agent workflow
- **policy-review** — How behavioral policies are structured and reviewed
- **forecast-visual-design-system** — UI/UX design tokens and component patterns

## Design Documents

## Design Documents

- **adversarial-review-v1** — Correctness, structure, and test hardening punch list (GH #364)
- **dashboard-extraction** — Standalone multi-repo dashboard architecture
- **refactor-subcommand-structure** — CLI surface area restructuring
- **v050-release** / **v050-release-gap-analysis** — v0.5.0 release planning
- **v070-release** — v0.7.0 QA audit, hub-write-lock bug, smoke test regression patterns
- **shared-issues-migration** — Migration to shared issue coordination
- **swarm-introspection** — Swarm phase introspection and budget design
- **adversarial-review-adr** — Earlier adversarial review findings (Feb 2026)
