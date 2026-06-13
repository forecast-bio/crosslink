# Feature: Autonomous Maintenance Daemon (`crosslink sentinel`)

## Summary

A persistent command that monitors external sources (GitHub labels, internal issue hygiene, CI failures) and autonomously dispatches scoped maintenance tasks via the existing kickoff infrastructure. The human filter is the `agent-todo:*` label convention — sentinel automates the *response* to signals humans have already blessed, not the triage itself. Starts with a single flow (replicate bugs from labeled GitHub issues) and grows toward a multi-source, policy-driven system. Fix agents auto-dispatch with `VerifyLevel::Ci`, pushing a branch and opening a draft PR for human review via normal GitHub flow.

## Requirements

- REQ-1: New module at `crosslink/src/commands/sentinel/` with subcommand group: `sentinel run` (one-shot sweep), `sentinel watch` (persistent daemon), `sentinel status` (live state), `sentinel history` (past runs and outcomes).
- REQ-2: Add `sentinel_runs` and `sentinel_dispatches` tables to the existing SQLite database (`db/core.rs`, SCHEMA_VERSION 15 -> 16). `sentinel_runs` tracks sweep metadata; `sentinel_dispatches` tracks per-signal disposition, agent ID, and outcome.
- REQ-3: Define a `Source` trait with `fn name(&self) -> &str` and `fn poll(&mut self, seen: &SeenSet) -> Result<Vec<Signal>>`. V0 ships one concrete source: `GitHubLabelSource` polling `agent-todo: replicate` issues via `gh issue list`.
- REQ-4: Multi-layer deduplication to prevent sentinel from acting on items already filed or already looked at. See "Deduplication Architecture" section for full design. Core invariant: a given GitHub issue number is dispatched at most once per label, unless the previous dispatch reached a terminal failure state AND a configurable cooldown has elapsed.
- REQ-5: Dispatch via `kickoff::run::run()` with a scoped reproduction prompt for `agent-todo: replicate`. The prompt constrains the agent to: read the issue, write a failing test demonstrating the bug, run the test to confirm it fails, record findings via crosslink comments. Agent may only modify files under `tests/` and project test directories. Uses `VerifyLevel::Local`.
- REQ-6: Result collection by polling `.kickoff-status` sentinel file in the agent's worktree. On completion (sentinel file contains `DONE` or `CI_FAILED`), read agent's crosslink comments, post a structured markdown summary to the originating GitHub issue via `gh issue comment`, and update `sentinel_dispatches.outcome`.
- REQ-7: Two-tier human filter model:
  - **Tier 1 (auto)**: Read-only operations — polling sources, dedup checks, creating crosslink issues for triage. No human involvement.
  - **Tier 2 (auto-dispatch + draft PR)**: Agent spawning in response to a human-applied `agent-todo:*` label. For `replicate`: `VerifyLevel::Local`, tests only. For `fix`: `VerifyLevel::Ci` — agent pushes branch and opens a draft PR. Human reviews via normal GitHub PR flow. The `agent-todo:` label IS the human approval — sentinel never invents new autonomy.
- REQ-8: `sentinel watch` daemon mode as a **separate process** from the hydration daemon, with configurable interval (default 10 minutes), PID file at `.crosslink/sentinel.pid`, signal handling (SIGTERM/SIGINT), and stdin-closure zombie prevention — following the existing `daemon.rs` pattern. Independent lifecycle, independent failure domain.
- REQ-9: Concurrent agent limit (default 3). Sentinel must track in-flight agents and refuse to dispatch new ones when at capacity. In-flight count derived from `sentinel_dispatches WHERE outcome = 'pending'` cross-referenced with worktree existence.
- REQ-10: Configuration via new `"sentinel"` key in `.crosslink/hook-config.json`. Keys: `enabled`, `interval_minutes`, `max_concurrent_agents`, `sources.*`, `default_agent.*`, `escalation.*`, `retry.*`. Register all keys in `config_registry.rs`.
- REQ-11: `sentinel run` (one-shot) must work without a running daemon. It performs exactly one poll-triage-dispatch cycle, collects results from any previously-dispatched agents that have completed, and exits.
- REQ-12: `sentinel history` shows past runs with signal counts, dispatch counts, outcomes, and timestamps. Supports `--json` for machine-readable output.
- REQ-13: Automatic model escalation: first attempt uses Sonnet. If the dispatch fails (agent couldn't reproduce, test doesn't compile, fix doesn't pass tests), the signal becomes eligible for retry with Opus after a configurable cooldown. Maximum 2 attempts per signal (1 Sonnet + 1 Opus). Escalation history tracked in `sentinel_dispatches` via `attempt_number` and `model_used` columns.
- REQ-14: Structured result template for GitHub comments. Fixed sections: Status, Agent, Duration, Model, Test File (for replicate) or PR Link (for fix), Findings, Test Output (truncated), Next Steps. Template is per-dispatch-type, compiled at post time from agent crosslink comments and worktree state.
- REQ-15: `agent-todo: fix` dispatch rule (V1). Agent spawns with `VerifyLevel::Ci`, allowed to modify `src/` and `tests/`, pushes branch, opens draft PR linking the original GH issue. PR title format: `fix: <GH issue title> (sentinel #<dispatch-id>)`. Agent needs `gh auth` credentials propagated to the worktree.

## Acceptance Criteria

- [ ] AC-1: `crosslink sentinel run` polls GitHub for issues labeled `agent-todo: replicate`, creates a crosslink issue for each new signal, dispatches a kickoff agent scoped to reproduction, and prints a summary. (REQ-1, REQ-3, REQ-5)
- [ ] AC-2: Running `crosslink sentinel run` twice against the same GH issue does NOT dispatch a second agent. The second run prints "1 skipped (already dispatched)". (REQ-4)
- [ ] AC-3: Running `crosslink sentinel run` after the dispatched agent has completed posts a structured result template to the GH issue via `gh issue comment` and marks the dispatch outcome as `"success"` or `"failure"`. (REQ-6, REQ-14)
- [ ] AC-4: `crosslink sentinel history` shows the run with correct signal/dispatch/skip counts. `--json` produces valid JSON. (REQ-12)
- [ ] AC-5: `crosslink sentinel watch` starts a persistent daemon that writes `.crosslink/sentinel.pid` (separate from `daemon.pid`), runs sentinel cycles at the configured interval, and exits cleanly on SIGTERM. (REQ-8)
- [ ] AC-6: `crosslink sentinel status` reports whether the daemon is running, how many agents are in-flight, and the last poll time. (REQ-1)
- [ ] AC-7: With `max_concurrent_agents: 2` and 2 agents already in-flight, a third signal is logged as "deferred (at capacity)" and retried on the next cycle. (REQ-9)
- [ ] AC-8: Schema migration v15->v16 adds `sentinel_runs` and `sentinel_dispatches` tables. Existing data is unaffected. (REQ-2)
- [ ] AC-9: `agent-todo: replicate` agents are spawned with `VerifyLevel::Local` and a scoped prompt that only allows test file modifications. (REQ-5, REQ-7)
- [ ] AC-10: `.crosslink/hook-config.json` with `"sentinel": { "enabled": false }` causes `sentinel run` to print "sentinel is disabled" and exit 0. (REQ-10)
- [ ] AC-11: `crosslink sentinel run` with no `agent-todo: replicate` issues prints "0 signals found" and exits 0. (REQ-3)
- [ ] AC-12: A sentinel dispatch whose agent worktree has been cleaned up (via `kickoff cleanup`) is marked `outcome: "orphaned"` and its GH issue is NOT commented on. (REQ-6, REQ-9)
- [ ] AC-13: `crosslink sentinel run --dry-run` prints what it *would* dispatch without creating issues or spawning agents. (REQ-1)
- [ ] AC-14: Sentinel-spawned agents use `init_worktree_agent` for identity and signing — same trust path as kickoff. (REQ-5)
- [ ] AC-15: A failed Sonnet dispatch (outcome `"failure"`) becomes eligible for Opus retry after `escalation.cooldown_minutes` (default 30). The retry dispatch has `attempt_number: 2` and `model_used: "claude-opus-4-6"`. (REQ-13)
- [ ] AC-16: A signal that failed on both Sonnet and Opus is marked `outcome: "exhausted"` and is never retried. The GH comment notes both attempts. (REQ-13)
- [ ] AC-17: GH issue comment from a completed `replicate` dispatch follows the structured template: Status, Agent, Duration, Model, Test File, Findings, Test Output, Next Steps. (REQ-14)
- [ ] AC-18: `agent-todo: fix` dispatch spawns an agent with `VerifyLevel::Ci` that pushes a branch and opens a draft PR. The PR title contains the GH issue title and sentinel dispatch ID. (REQ-15, REQ-7)
- [ ] AC-19: A GH issue that already has a successful `replicate` dispatch and is then labeled `agent-todo: fix` triggers a new `fix` dispatch (different label = different signal_ref). (REQ-4, REQ-15)
- [ ] AC-20: Dedup correctly distinguishes signals by `(gh_issue_number, label)` tuple — the same issue can have one `replicate` and one `fix` dispatch in flight simultaneously. (REQ-4)
- [ ] AC-21: An issue whose `agent-todo: replicate` label was removed before sentinel polls it is NOT dispatched. Sentinel only acts on labels present at poll time. (REQ-4)

## Architecture

### Module Structure

New module at `crosslink/src/commands/sentinel/` following the `kickoff/` pattern:

```
crosslink/src/commands/sentinel/
+-- mod.rs          # CLI dispatch + SentinelCommands enum
+-- engine.rs       # Core sentinel loop (one-shot and watch modes)
+-- sources/
|   +-- mod.rs      # Source trait + SourceKind enum + SeenSet
|   +-- github.rs   # GitHubLabelSource (polls `gh issue list`)
+-- dispatch.rs     # Triage rules + kickoff integration
+-- collect.rs      # Result collection from completed agents
+-- history.rs      # Query + display sentinel_runs/dispatches
+-- config.rs       # Sentinel-specific config loading from hook-config.json
```

### Core Abstractions

**Signal** — a maintenance event detected by a source adapter:

```rust
pub struct Signal {
    pub source: SourceKind,
    pub kind: SignalKind,
    pub reference: String,       // "GH#499", "CL#42", "CI:run/12345"
    pub title: String,
    pub body: String,
    pub metadata: serde_json::Value,
    pub detected_at: DateTime<Utc>,
}

pub enum SourceKind { GitHub, Internal, CI }
pub enum SignalKind { LabelAdded, StaleIssue, CIFailure }
```

**Disposition** — the triage engine's decision for a signal:

```rust
pub enum Disposition {
    Dispatch {
        description: String,
        scope: AgentScope,
    },
    Triage {
        priority: String,
        labels: Vec<String>,
    },
    Skip {
        reason: String,
    },
    Defer {
        reason: String,   // "at capacity", "cooldown"
    },
}
```

**AgentScope** — constrains what a dispatched agent can do:

```rust
pub struct AgentScope {
    pub allowed_paths: Vec<String>,
    pub verify: VerifyLevel,
    pub timeout: Duration,
    pub model: String,
}
```

**Source** trait:

```rust
pub trait Source {
    fn name(&self) -> &str;
    fn poll(&mut self, seen: &SeenSet) -> Result<Vec<Signal>>;
}
```

### Database Schema (Layer 1)

Increment `SCHEMA_VERSION` from 15 to 16 in `crosslink/src/db/core.rs:5`. Add migration:

```sql
CREATE TABLE IF NOT EXISTS sentinel_runs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id TEXT NOT NULL UNIQUE,
    started_at TEXT NOT NULL,
    completed_at TEXT,
    mode TEXT NOT NULL,               -- "oneshot" | "watch"
    signals_found INTEGER DEFAULT 0,
    dispatched INTEGER DEFAULT 0,
    collected INTEGER DEFAULT 0,
    triaged INTEGER DEFAULT 0,
    skipped INTEGER DEFAULT 0,
    deferred INTEGER DEFAULT 0
);

CREATE TABLE IF NOT EXISTS sentinel_dispatches (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id TEXT NOT NULL,
    signal_ref TEXT NOT NULL,          -- "GH#499:replicate", "GH#499:fix"
    signal_title TEXT NOT NULL,
    source TEXT NOT NULL,
    disposition TEXT NOT NULL,         -- "dispatch" | "triage" | "skip" | "defer"
    agent_id TEXT,                     -- kickoff compact name (if dispatched)
    crosslink_issue_id INTEGER,
    gh_issue_number INTEGER,          -- originating GH issue (if from GitHub source)
    label TEXT NOT NULL,              -- "agent-todo: replicate", "agent-todo: fix"
    attempt_number INTEGER DEFAULT 1, -- 1 = first attempt (Sonnet), 2 = escalation (Opus)
    model_used TEXT,                  -- "claude-sonnet-4-6", "claude-opus-4-6"
    outcome TEXT DEFAULT 'pending',   -- "pending" | "success" | "failure" | "timeout" | "orphaned" | "exhausted"
    outcome_detail TEXT,
    created_at TEXT NOT NULL,
    completed_at TEXT,
    FOREIGN KEY (crosslink_issue_id) REFERENCES issues(id)
);

CREATE INDEX idx_sentinel_dispatches_signal_ref ON sentinel_dispatches(signal_ref);
CREATE INDEX idx_sentinel_dispatches_outcome ON sentinel_dispatches(outcome);
CREATE INDEX idx_sentinel_dispatches_run_id ON sentinel_dispatches(run_id);
CREATE INDEX idx_sentinel_dispatches_gh_label ON sentinel_dispatches(gh_issue_number, label);
```

### Deduplication Architecture

Sentinel polls on a fixed interval. The same GitHub issue with the same label will appear on every poll until the label is removed. Without robust dedup, sentinel would create duplicate crosslink issues, spawn duplicate agents, and post duplicate GH comments. This section defines the multi-layer dedup system.

#### Signal Identity

Every signal has a composite reference that uniquely identifies it:

```
signal_ref = "GH#<issue_number>:<label_suffix>"
```

Examples:
- `GH#499:replicate` — issue #499 with `agent-todo: replicate` label
- `GH#499:fix` — issue #499 with `agent-todo: fix` label
- `GH#502:replicate` — different issue, same label

This means the same issue CAN have both a `replicate` and a `fix` dispatch simultaneously — they're different signals with different scopes.

#### SeenSet (In-Memory Dedup Cache)

Loaded once per sentinel cycle from `sentinel_dispatches`. Determines whether a signal should be skipped, retried, or escalated.

```rust
pub struct SeenRecord {
    pub disposition: String,
    pub outcome: String,
    pub attempt_number: i32,
    pub model_used: String,
    pub completed_at: Option<DateTime<Utc>>,
}

pub struct SeenSet {
    /// signal_ref -> most recent dispatch record
    seen: HashMap<String, SeenRecord>,
}

impl SeenSet {
    pub fn load(db: &Database) -> Result<Self> {
        // SELECT signal_ref, disposition, outcome, attempt_number, model_used, completed_at
        // FROM sentinel_dispatches
        // ORDER BY created_at DESC
        // (take the most recent record per signal_ref)
    }

    pub fn evaluate(&self, signal_ref: &str, config: &SentinelConfig) -> SignalDecision {
        let Some(record) = self.seen.get(signal_ref) else {
            return SignalDecision::New; // never seen — dispatch
        };

        match record.outcome.as_str() {
            "pending" => SignalDecision::Skip("agent in-flight"),

            "success" => SignalDecision::Skip("already resolved"),

            "exhausted" => SignalDecision::Skip("both attempts failed"),

            "failure" | "timeout" => {
                // Check if eligible for escalation
                if record.attempt_number >= 2 {
                    return SignalDecision::Skip("max attempts reached");
                }
                // Check cooldown
                if let Some(completed) = &record.completed_at {
                    let elapsed = Utc::now().signed_duration_since(*completed);
                    if elapsed.num_minutes() < config.escalation.cooldown_minutes as i64 {
                        return SignalDecision::Skip("cooldown not elapsed");
                    }
                }
                SignalDecision::Escalate // retry with Opus
            }

            "orphaned" => {
                // Worktree was cleaned up before completion — treat like failure
                if record.attempt_number >= 2 {
                    return SignalDecision::Skip("max attempts reached");
                }
                SignalDecision::Escalate
            }

            _ => SignalDecision::Skip("unknown state"),
        }
    }
}

pub enum SignalDecision {
    /// Never seen before — dispatch with Sonnet (attempt 1)
    New,
    /// Previous attempt failed — dispatch with Opus (attempt 2)
    Escalate,
    /// Already handled or ineligible — do not dispatch
    Skip(&'static str),
}
```

#### Layer 1: Source-Level Filtering

`GitHubLabelSource::poll()` only returns issues that currently have the `agent-todo:*` label at poll time. If a human removes the label before sentinel polls, the issue is never seen. This is the first dedup layer — it's passive and relies on GitHub's state.

#### Layer 2: SeenSet Lookup

Before creating a crosslink issue or dispatching an agent, sentinel checks the SeenSet. This prevents:
- Re-dispatching while an agent is in-flight (`"pending"`)
- Re-dispatching for already-resolved signals (`"success"`)
- Re-dispatching exhausted signals (`"exhausted"` — both Sonnet and Opus failed)
- Re-dispatching too soon after a failure (cooldown not elapsed)

#### Layer 3: Database Constraint

The `idx_sentinel_dispatches_gh_label` index on `(gh_issue_number, label)` enables fast lookups. Before inserting a new dispatch, sentinel queries:

```sql
SELECT id, outcome, attempt_number FROM sentinel_dispatches
WHERE gh_issue_number = ? AND label = ?
ORDER BY created_at DESC LIMIT 1
```

This is the authoritative dedup check — even if the in-memory SeenSet is stale (e.g., sentinel restarted mid-cycle), the database prevents duplicates.

#### Layer 4: GH Comment Dedup

Before posting a result comment to a GH issue, sentinel checks whether it already posted for this dispatch ID:

```rust
fn already_commented(gh_issue: u64, dispatch_id: i64) -> Result<bool> {
    // gh issue view <N> --json comments --jq '.comments[].body'
    // Check for "sentinel #<dispatch-id>" marker string
}
```

This prevents duplicate GH comments if sentinel crashes between posting and updating the dispatch outcome.

#### Escalation Flow

```
Signal arrives (GH#499:replicate)
  |
  v
SeenSet.evaluate() -> New
  |
  v
Dispatch attempt 1: Sonnet, 30min timeout
  |
  +---> Success -> outcome="success", DONE
  |
  +---> Failure -> outcome="failure"
           |
           v
        (cooldown elapses, next sentinel cycle)
           |
           v
        SeenSet.evaluate() -> Escalate
           |
           v
        Dispatch attempt 2: Opus, 45min timeout
           |
           +---> Success -> outcome="success", DONE
           |
           +---> Failure -> outcome="exhausted", DONE (give up)
```

### Dispatch Flow (V0: `agent-todo: replicate`)

```
                    +------+
GH Issue            |  gh  |
+ agent-todo:  ---->| CLI  |----> Signal { ref: "GH#499", kind: LabelAdded }
  replicate         | poll |
                    +--+---+
                       |
                       v
                 +-----+------+
                 |  SeenSet   |--- already dispatched? ---> Skip
                 |  (dedup)   |
                 +-----+------+
                       |  new signal
                       v
                 +-----+------+
                 |  Triage    |--- match label -> Disposition::Dispatch
                 |  Engine    |
                 +-----+------+
                       |
                       v
                 +-----+------+     +------------------+
                 |  kickoff   |---->| Agent (tmux)     |
                 |  ::run()   |     | - read GH issue  |
                 +-----+------+     | - write test     |
                       |            | - run test       |
                       |            | - comment results|
                       |            +--------+---------+
                       v                     |
                 +-----+------+              |
                 |  sentinel_   |<-------------+ .kickoff-status
                 |  dispatches|   (collect phase)
                 +-----+------+
                       |
                       v
                 +-----+------+
                 |  gh issue  |
                 |  comment   |--- post results to GH#499
                 +------------+
```

**Step-by-step:**

1. **Poll**: `gh issue list --repo <repo> --label "agent-todo: replicate" --json number,title,body,labels,createdAt --state open`
2. **Dedup**: Load `SeenSet` from `sentinel_dispatches` — skip issues already dispatched with non-retriable outcomes
3. **Triage**: Match signal against hardcoded rules. `agent-todo: replicate` -> `Disposition::Dispatch` with reproduction scope
4. **Create crosslink issue**: `writer.create_issue(db, title, description, "medium")` + label with `"sentinel"` and `"bug"`
5. **Check capacity**: Count `sentinel_dispatches WHERE outcome = 'pending'`. If >= `max_concurrent_agents`, return `Disposition::Defer`
6. **Spawn agent**: Build `KickoffOpts` with scoped prompt, call `kickoff::run::run(crosslink_dir, db, writer, &opts)`
7. **Record dispatch**: Insert into `sentinel_dispatches` with `outcome = 'pending'`
8. **Collect (async)**: On subsequent sentinel cycles, check `.kickoff-status` for all pending dispatches. If done:
   - Read agent's crosslink comments for findings
   - Post summary to GH issue via `gh issue comment <N> --body "<summary>"`
   - Update `sentinel_dispatches.outcome` and `completed_at`

### Agent Prompt (Reproduction Scope)

The dispatched agent receives a tightly scoped prompt via `KickoffOpts.description`:

```
Reproduce the bug described in GitHub issue #<N>.

Title: <title>
Body:
<body>

Your task:
1. Read the issue carefully and understand the expected vs actual behavior
2. Explore the codebase to find the relevant code paths
3. Write a failing test that demonstrates the bug
4. Run the test suite to confirm your test fails for the right reason
5. Record your findings as a crosslink comment (--kind observation)
6. If you cannot reproduce, explain why (--kind resolution)

Constraints:
- You may ONLY create or modify files in tests/ directories
- Do NOT fix the bug — only reproduce it
- Do NOT push code or create PRs
- Time limit: 30 minutes
```

This is injected as `opts.description` and wired through `build_prompt()` in `kickoff/prompt.rs`. The agent gets `VerifyLevel::Local` and path restrictions via the allowed-tools whitelist.

**Fix scope prompt** (`agent-todo: fix`, V1):

```
Fix the bug described in GitHub issue #<N>.

Title: <title>
Body:
<body>

Your task:
1. Read the issue carefully and understand the expected vs actual behavior
2. Explore the codebase to find the root cause
3. Write a failing test that demonstrates the bug
4. Implement the fix
5. Run the full test suite to confirm the fix works and nothing else breaks
6. Record your findings as a crosslink comment (--kind resolution)
7. Push your branch and open a draft PR linking GH#<N>

Draft PR title: fix: <issue title> (sentinel #<dispatch-id>)

Constraints:
- You may modify files in src/ and tests/
- Push your branch when tests pass
- Open a DRAFT PR (not ready for review) — a human will review it
- Time limit: 60 minutes
```

This agent gets `VerifyLevel::Ci` and broader path access. The draft PR is the human review checkpoint.

### Structured Result Template

When a sentinel agent completes, results are posted to the originating GitHub issue using a fixed template per dispatch type.

**Reproduction template** (`agent-todo: replicate`):

```markdown
## Sentinel: Reproduction Report

| Field | Value |
|-------|-------|
| Status | Reproduced / Could not reproduce / Partial |
| Agent | `<compact-name>` |
| Model | claude-sonnet-4-6 |
| Attempt | 1 of 2 |
| Duration | 12m 34s |
| Test file | `tests/regression/test_issue_499.rs` |

### Findings

<agent's observation comments from crosslink, in chronological order>

### Test output

```
test tests::regression::test_issue_499 ... FAILED
<truncated to 50 lines>
```

### Next steps

- [ ] Review the failing test
- [ ] Label `agent-todo: fix` to trigger an automated fix attempt

---
*Posted by crosslink sentinel | dispatch #42 | [view dispatch history](link)*
```

**Fix template** (`agent-todo: fix`):

```markdown
## Sentinel: Fix Report

| Field | Value |
|-------|-------|
| Status | Fixed / Partial fix / Could not fix |
| Agent | `<compact-name>` |
| Model | claude-sonnet-4-6 |
| Attempt | 1 of 2 |
| Duration | 34m 12s |
| PR | #<pr-number> (draft) |
| Files changed | 3 |
| Lines | +45 / -12 |

### Findings

<agent's resolution comments from crosslink, in chronological order>

### Test results

```
test result: ok. 148 passed; 0 failed; 0 ignored
```

### Changes summary

<brief description of what was changed and why>

---
*Posted by crosslink sentinel | dispatch #57 | [view PR](#<pr-number>)*
```

The template is compiled at collection time from:
- `sentinel_dispatches` record (agent ID, model, attempt, timing)
- Agent's crosslink comments (`db.get_comments(crosslink_issue_id)` filtered by `kind = "observation"` or `kind = "resolution"`)
- Worktree state (`.kickoff-status`, `git diff --stat`, test output from `.kickoff-report.json` if present)
- PR number (from `gh pr list --head <branch> --json number` if `VerifyLevel::Ci`)

### Triage Engine (V0: Hardcoded, V2: Policy)

V0 ships `replicate`, V1 adds `fix`:

```rust
fn triage(signal: &Signal, decision: SignalDecision, config: &SentinelConfig) -> Disposition {
    // Determine model based on escalation state
    let model = match &decision {
        SignalDecision::New => config.default_agent.model.clone(),      // Sonnet
        SignalDecision::Escalate => config.escalation.model.clone(),    // Opus
        SignalDecision::Skip(reason) => return Disposition::Skip {
            reason: reason.to_string(),
        },
    };
    let attempt = match &decision {
        SignalDecision::New => 1,
        SignalDecision::Escalate => 2,
        _ => unreachable!(),
    };

    match (&signal.source, &signal.kind) {
        (SourceKind::GitHub, SignalKind::LabelAdded) => {
            let label = signal.metadata.get("label")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            match label {
                "agent-todo: replicate" => Disposition::Dispatch {
                    description: format!(
                        "Reproduce bug from GH#{}: {}",
                        signal.reference, signal.title
                    ),
                    scope: AgentScope {
                        allowed_paths: vec!["tests/".into()],
                        verify: VerifyLevel::Local,
                        timeout: Duration::from_secs(1800),
                        model,
                    },
                    attempt,
                },

                "agent-todo: fix" => Disposition::Dispatch {
                    description: format!(
                        "Fix bug from GH#{}: {}",
                        signal.reference, signal.title
                    ),
                    scope: AgentScope {
                        allowed_paths: vec!["src/".into(), "tests/".into()],
                        verify: VerifyLevel::Ci,  // pushes branch + opens draft PR
                        timeout: Duration::from_secs(3600),
                        model,
                    },
                    attempt,
                },

                _ => Disposition::Skip {
                    reason: format!("unrecognized agent-todo label: {label}"),
                },
            }
        }
        _ => Disposition::Skip {
            reason: "no matching rule".into(),
        },
    }
}
```

When 3+ hardcoded rules exist and patterns emerge, extract to `sentinel.toml` (V2):

```toml
[[rule]]
name = "replicate-bugs"
source = "github"
match = { label = "agent-todo: replicate" }
action = "dispatch"
scope = { paths = ["tests/"], verify = "local", timeout = "30m" }

[[rule]]
name = "fix-bugs"
source = "github"
match = { label = "agent-todo: fix" }
action = "dispatch"
scope = { paths = ["src/", "tests/"], verify = "ci", timeout = "1h" }
```

### Daemon Mode (`sentinel watch`)

Follows the existing `daemon.rs` pattern with key differences:

Separate process from the hydration daemon (Q1 decision). Different intervals, different failure modes, different side effects. A sentinel crash doesn't take down sync.

| Aspect | Hydration Daemon | Sentinel Daemon |
|--------|-----------------|---------------|
| PID file | `.crosslink/daemon.pid` | `.crosslink/sentinel.pid` |
| Interval | 30s (lightweight sync) | 10min (spawns agents) |
| Side effects | SQLite writes | Agent spawning, GH comments, branch pushes, draft PRs |
| In-flight tracking | None | Tracks pending dispatches + escalation state |
| Failure backoff | Linear (consecutive count) | Exponential (1x, 2x, 4x interval) |

```rust
pub fn watch(crosslink_dir: &Path, db: &Database, writer: Option<&SharedWriter>) -> Result<()> {
    let config = SentinelConfig::load(crosslink_dir)?;
    if !config.enabled {
        println!("Sentinel is disabled in hook-config.json");
        return Ok(());
    }

    let pid_file = crosslink_dir.join("sentinel.pid");
    // Write PID, register SIGTERM/SIGINT handlers, spawn stdin-closure watcher
    // (same pattern as daemon.rs:101-175)

    let mut sources: Vec<Box<dyn Source>> = vec![
        Box::new(GitHubLabelSource::new(&config)?),
    ];

    let interval = Duration::from_secs(config.interval_minutes * 60);
    let mut backoff_multiplier: u32 = 1;

    loop {
        if should_exit.load(Ordering::SeqCst) { break; }

        match run_sentinel_cycle(db, writer, &mut sources, crosslink_dir, &config) {
            Ok(stats) => {
                backoff_multiplier = 1; // reset on success
                record_run(db, &stats)?;
            }
            Err(e) => {
                tracing::error!("sentinel cycle failed: {e}");
                backoff_multiplier = (backoff_multiplier * 2).min(8);
            }
        }

        // Collect results from completed agents (every cycle)
        if let Err(e) = collect_completed(db, crosslink_dir) {
            tracing::warn!("result collection failed: {e}");
        }

        thread::sleep(interval * backoff_multiplier);
    }
}
```

### Configuration

New `"sentinel"` key in `.crosslink/hook-config.json`:

```jsonc
{
    "sentinel": {
        "enabled": true,
        "interval_minutes": 10,
        "max_concurrent_agents": 3,
        "sources": {
            "github_labels": {
                "enabled": true,
                "labels": ["agent-todo: replicate", "agent-todo: fix"]
            }
        },
        "default_agent": {
            "model": "claude-sonnet-4-6",
            "timeout_minutes": 30,
            "verify": "local"
        },
        "escalation": {
            "enabled": true,
            "model": "claude-opus-4-6",
            "cooldown_minutes": 30,
            "max_attempts": 2,
            "timeout_multiplier": 1.5
        },
        "retry": {
            "max_retries_per_signal": 2,
            "cooldown_minutes": 30
        }
    }
}
```

Register in `config_registry.rs` with group `Sentinel`:
- `sentinel.enabled` (Bool, hot-swappable)
- `sentinel.interval_minutes` (Integer, range 1-1440)
- `sentinel.max_concurrent_agents` (Integer, range 1-10)
- `sentinel.sources.github_labels.enabled` (Bool)
- `sentinel.sources.github_labels.labels` (StringArray)
- `sentinel.default_agent.model` (String)
- `sentinel.default_agent.timeout_minutes` (Integer, range 5-480)
- `sentinel.default_agent.verify` (Enum: local, ci, thorough)
- `sentinel.escalation.enabled` (Bool)
- `sentinel.escalation.model` (String)
- `sentinel.escalation.cooldown_minutes` (Integer, range 5-1440)
- `sentinel.escalation.max_attempts` (Integer, range 1-5)
- `sentinel.escalation.timeout_multiplier` (Float, range 1.0-3.0)
- `sentinel.retry.max_retries_per_signal` (Integer, range 1-5)
- `sentinel.retry.cooldown_minutes` (Integer, range 5-1440)

### CLI Integration

Add to `Commands` enum in `main.rs` (alongside existing Kickoff, Daemon, etc.):

```rust
/// Autonomous maintenance sentinel
#[command(subcommand)]
Sentinel {
    #[command(subcommand)]
    action: SentinelCommands,
},
```

```rust
#[derive(Subcommand)]
pub enum SentinelCommands {
    /// One-shot sentinel sweep
    Run {
        /// Print what would be dispatched without acting
        #[arg(long)]
        dry_run: bool,
        /// Only process signals matching this label
        #[arg(long)]
        label: Option<String>,
    },
    /// Start persistent sentinel daemon
    Watch {
        /// Poll interval in minutes
        #[arg(long, default_value = "10")]
        interval: u64,
    },
    /// Show sentinel daemon status and in-flight agents
    Status,
    /// Show past sentinel runs and outcomes
    History {
        #[arg(long, default_value = "10")]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Stop the sentinel daemon
    Stop,
}
```

Match dispatch in `main.rs` (line ~2678 area):

```rust
Commands::Sentinel { action } => {
    let crosslink_dir = find_crosslink_dir()?;
    let db = get_db()?;
    let writer = get_writer(&crosslink_dir);
    commands::sentinel::dispatch(action, &crosslink_dir, &db, writer.as_ref(), cli.quiet, cli.json)
}
```

### Human Filter Layer

Two tiers of autonomy. The `agent-todo:` label IS the human approval gate — applying it is the decision to let sentinel act.

| Tier | Actions | Human involvement | Scope |
|------|---------|-------------------|-------|
| **1 (auto)** | Poll sources, dedup, create crosslink triage issues | None | V0 |
| **2 (auto-dispatch)** | Spawn agent for `agent-todo:*` labeled issues. `replicate` = Local (tests only). `fix` = Ci (pushes branch, opens draft PR). | Human reviews the draft PR via normal GitHub flow | V0/V1 |

The key insight: the `agent-todo:` label is the human filter. A human reads the issue, decides it's appropriate for automated handling, and applies the label. Sentinel simply automates the response to that decision. It never invents new autonomy. The draft PR is the review checkpoint — nothing merges without a human approving it.

### Trust Model

Sentinel-spawned agents use the same trust path as kickoff:

1. `create_worktree()` from `kickoff/launch.rs` creates the worktree
2. `init_worktree_agent()` from `kickoff/launch.rs` initializes crosslink + agent identity in the worktree
3. Agent gets a dedicated ED25519 signing key via `signing::generate_agent_key()`
4. Key is auto-approved via `trust::publish_agent_key()`

**Per-rule constraints**:
- `replicate`: `VerifyLevel::Local` — no push, no PR. Path restrictions to test directories only.
- `fix`: `VerifyLevel::Ci` — pushes branch, opens draft PR. Broader path access (src/ + tests/). Requires `gh auth` credentials propagated to the worktree (copy `.gitconfig` credential helper or set `GH_TOKEN` env var at launch).
- All: Worktrees cleaned up automatically after result collection (via `kickoff::cleanup` integration).

### Result Collection (`collect.rs`)

Runs every sentinel cycle (in watch mode) or at the end of a one-shot run:

```rust
pub fn collect_completed(db: &Database, crosslink_dir: &Path) -> Result<CollectStats> {
    let pending = db.get_pending_dispatches()?;
    let mut stats = CollectStats::default();

    for dispatch in pending {
        let Some(agent_id) = &dispatch.agent_id else { continue };

        // Check if agent worktree still exists
        let root = kickoff::launch::repo_root()?;
        let wt_path = root.join(".worktrees").join(agent_id);
        if !wt_path.exists() {
            db.update_dispatch_outcome(dispatch.id, "orphaned", "worktree removed")?;
            stats.orphaned += 1;
            continue;
        }

        // Check sentinel
        let status_path = wt_path.join(".kickoff-status");
        let Ok(status) = std::fs::read_to_string(&status_path) else {
            continue; // still running
        };

        let outcome = if status.trim().starts_with("DONE") { "success" } else { "failure" };

        // Read agent findings from crosslink comments
        let findings = read_agent_findings(db, dispatch.crosslink_issue_id)?;

        // Post to GH issue
        if let Some(gh_num) = dispatch.gh_issue_number {
            post_gh_comment(gh_num, outcome, &findings)?;
        }

        db.update_dispatch_outcome(dispatch.id, outcome, &findings)?;
        stats.collected += 1;
    }

    Ok(stats)
}
```

### Error Handling

- **`gh` CLI not found**: `sentinel run` fails with install instructions (same as kickoff preflight)
- **`gh` not authenticated**: Fail with "run `gh auth login`" message
- **No repo detected**: Fail with "not in a git repository"
- **Database locked**: Retry with backoff (3 attempts, same as SharedWriter pattern)
- **Agent spawn failure**: Record dispatch as `outcome: "failure"`, log error, continue to next signal
- **GH API rate limit**: Back off exponentially, skip GitHub source for this cycle
- **Network offline**: Skip GitHub source, continue with internal sources only

## Design Decisions (Resolved)

### Q1: Separate process — DECIDED

Sentinel runs as its own process (`sentinel.pid`), independent from the hydration daemon. Different failure modes, different intervals, different side effects. A sentinel crash doesn't take down sync.

### Q2: Fix agents auto-dispatch with draft PR — DECIDED

`agent-todo: fix` auto-dispatches with `VerifyLevel::Ci`. The agent pushes a branch and opens a **draft PR**. The human reviews via normal GitHub PR flow. The `agent-todo:` label is the approval gate — applying it is the decision. The draft PR is the review checkpoint — nothing merges without a human.

### Q3: Structured result template — DECIDED

Fixed per-dispatch-type templates with sections: Status, Agent, Duration, Model, Test File / PR Link, Findings, Test Output, Next Steps. Scannable, machine-parseable, consistent across all sentinel comments. See "Structured Result Template" section in Architecture for the exact formats.

### Q4: Automatic model escalation — DECIDED

First attempt uses Sonnet. On failure, retry with Opus after a configurable cooldown (default 30 minutes). Maximum 2 attempts per signal (1 Sonnet + 1 Opus). Tracked via `attempt_number` and `model_used` columns in `sentinel_dispatches`. Escalation timeout gets a 1.5x multiplier (30min Sonnet -> 45min Opus). See "Escalation Flow" in the Deduplication Architecture section.

### Q5: Named `sentinel` — DECIDED

Distinctive, evokes watchfulness and response, no collision with existing subcommands or the `/maintain` skill. `crosslink sentinel run`, `crosslink sentinel watch`, `crosslink sentinel status`.

## Out of Scope

- Webhook receiver for real-time GH events (V3 — requires axum HTTP server)
- Slack/Discord notification or approval buttons (V3)
- Dependency auditing source (`cargo audit` / `npm audit`) (V2)
- CI failure source (`gh run list`) (V2)
- Internal hygiene source (stale issues, orphans) (V2)
- Policy file (`sentinel.toml`) for declarative rules (V2)
- Historical pattern detection ("module X breaks 3x more than average") (V3)
- Token usage tracking for sentinel agents (depends on existing `token_usage` table integration)
- Sentinel agent log aggregation or TUI dashboard tab
- Merge automation (sentinel never merges — only opens draft PRs)

## Milestones

### V0: Foundation
- `commands/sentinel/` module skeleton with CLI dispatch
- `sentinel_runs` + `sentinel_dispatches` tables (schema v16)
- `GitHubLabelSource` — poll `agent-todo: replicate` issues
- Multi-layer dedup (SeenSet + DB constraint + GH comment dedup)
- Dispatch via `kickoff::run::run()` with scoped reproduction prompt
- Result collection from `.kickoff-status` + crosslink comments
- Post structured results back to GH issue via `gh issue comment`
- `crosslink sentinel run` (one-shot) working end-to-end
- `crosslink sentinel history` (show past runs)
- `crosslink sentinel run --dry-run`
- Automatic Sonnet -> Opus escalation on failure

### V1: Daemon Mode + Fix Rule
- `crosslink sentinel watch` with configurable interval
- In-flight agent tracking + concurrent limit enforcement
- `crosslink sentinel status` (live dashboard)
- `crosslink sentinel stop`
- Exponential backoff on repeated failures
- `agent-todo: fix` dispatch rule — `VerifyLevel::Ci`, pushes branch, opens draft PR
- Fix result template with PR link

### V2: Multi-Source + Policy
- `InternalHygieneSource` (stale issues, orphans, missing labels)
- `GitHubCISource` (build failures on default branch)
- `sentinel.toml` policy file (declarative rules)
- `DependencySource` (`cargo audit` / `npm audit`)
- Outcome tracking + success rate metrics per model per rule

### V3: Proactive + Real-Time
- Scheduled maintenance sweeps (lint drift, test coverage regression)
- Webhook receiver for real-time GH events (axum endpoint)
- Slack/webhook notifications for dispatch results
- Historical pattern detection ("module X breaks 3x more than average")
- Self-tuning: auto-promote rules from Sonnet to Opus based on historical success rates
