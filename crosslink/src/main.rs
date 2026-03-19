mod checkpoint;
mod clock_skew;
mod commands;
mod compaction;
mod daemon;
mod db;
mod events;
mod external;
mod findings;
mod hydration;
mod identity;
mod issue_file;
mod issue_filing;
mod knowledge;
mod lock_check;
mod locks;
mod models;
mod orchestrator;
mod pipeline;
mod seam;
mod server;
mod shared_writer;
mod signing;
mod sync;
mod trust_model;
mod tui;
mod utils;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use std::env;
use std::path::PathBuf;

use db::Database;

#[derive(Parser)]
#[command(name = "crosslink")]
#[command(about = "A simple, lean issue tracker CLI")]
#[command(version = option_env!("CROSSLINK_VERSION").unwrap_or(env!("CARGO_PKG_VERSION")))]
struct Cli {
    /// Quiet mode: only output essential data (IDs, counts)
    #[arg(short, long, global = true)]
    quiet: bool,

    /// Output as JSON (supported by list, show, search, session status)
    #[arg(long, global = true)]
    json: bool,

    /// Log level for diagnostic output (error, warn, info, debug, trace)
    #[arg(long, global = true, default_value = "warn", env = "CROSSLINK_LOG")]
    log_level: String,

    /// Log format (text, json)
    #[arg(
        long,
        global = true,
        default_value = "text",
        env = "CROSSLINK_LOG_FORMAT"
    )]
    log_format: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize crosslink in the current directory
    Init {
        /// Force update hooks even if already initialized
        #[arg(short, long)]
        force: bool,
        /// Override auto-detected Python prefix for hook commands (e.g. "uv run python3")
        #[arg(long)]
        python_prefix: Option<String>,
        /// Skip automatic cpitd installation
        #[arg(long)]
        skip_cpitd: bool,
        /// Skip driver SSH signing key setup
        #[arg(long)]
        skip_signing: bool,
        /// Path to SSH key for commit signing (auto-detected if omitted)
        #[arg(long)]
        signing_key: Option<String>,
        /// Re-run TUI walkthrough even if config exists
        #[arg(long)]
        reconfigure: bool,
        /// Skip TUI and use opinionated defaults
        #[arg(long)]
        defaults: bool,
    },

    /// Issue lifecycle commands (create, show, list, close, ...)
    Issue {
        #[command(subcommand)]
        action: IssueCommands,
    },

    /// Time tracking (start, stop, show)
    Timer {
        #[command(subcommand)]
        action: TimerCommands,
    },

    /// Export issues to JSON or markdown
    Export {
        /// Output file path (defaults to stdout)
        #[arg(short, long)]
        output: Option<String>,
        /// Format (json, markdown)
        #[arg(short, long, default_value = "json")]
        format: String,
    },

    /// Import issues from JSON file
    Import {
        /// Input file path
        input: String,
    },

    /// Archive management
    Archive {
        #[command(subcommand)]
        action: ArchiveCommands,
    },

    /// Milestone management
    Milestone {
        #[command(subcommand)]
        action: MilestoneCommands,
    },

    /// Session management
    Session {
        #[command(subcommand)]
        action: SessionCommands,
    },

    /// Daemon management
    Daemon {
        #[command(subcommand)]
        action: DaemonCommands,
    },

    /// Code clone detection via cpitd
    Cpitd {
        #[command(subcommand)]
        action: CpitdCommands,
    },

    /// Agent identity management
    Agent {
        #[command(subcommand)]
        action: AgentCommands,
    },

    /// Manage signing trust (approve/revoke agent keys)
    Trust {
        #[command(subcommand)]
        action: TrustCommands,
    },

    /// View and manage issue locks
    Locks {
        #[command(subcommand)]
        action: LocksCommands,
    },

    /// Push a heartbeat for the current agent (used by hooks)
    #[command(hide = true)]
    Heartbeat,

    /// Sync locks and issue state from remote
    Sync,

    /// Schema migration (to-shared, from-shared, rename-branch)
    Migrate {
        #[command(subcommand)]
        action: MigrateCommands,
    },

    /// View and modify repo-level configuration
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },

    /// Measure and check context injection overhead
    Context {
        #[command(subcommand)]
        command: ContextCommands,
    },

    /// Manage crosslink workflow configuration
    Workflow {
        #[command(subcommand)]
        command: WorkflowCommands,
    },

    /// Manage house style syncing
    Style {
        #[command(subcommand)]
        command: StyleCommands,
    },

    /// Manage shared knowledge pages
    Knowledge {
        #[command(subcommand)]
        command: KnowledgeCommands,
    },

    /// Data integrity checks and repair
    Integrity {
        #[command(subcommand)]
        action: Option<IntegrityCommands>,
    },

    /// Run event compaction manually
    Compact {
        /// Force compaction even if lease is held by another agent
        #[arg(long)]
        force: bool,
    },

    /// Prune git history of hub and knowledge branches for storage efficiency
    Prune {
        /// Show what would be pruned without modifying anything
        #[arg(long = "dry-run")]
        dry_run: bool,
        /// Skip confirmation and execute the prune
        #[arg(long)]
        force: bool,
        /// Preserve the last N commits (default: 1, squash to current state)
        #[arg(long = "keep-commits", default_value = "1")]
        keep_commits: usize,
        /// Only prune the hub branch
        #[arg(long = "hub-only")]
        hub_only: bool,
        /// Only prune the knowledge branch
        #[arg(long = "knowledge-only")]
        knowledge_only: bool,
    },

    /// Launch an agent to implement a feature (local process or container)
    Kickoff {
        #[command(subcommand)]
        action: KickoffCommands,
    },
    /// Multi-agent swarm coordination (plan, status, resume)
    Swarm {
        #[command(subcommand)]
        action: SwarmCommands,
    },
    /// Interactive terminal dashboard (read-only)
    Tui,
    /// Mission control: tmux dashboard showing all active agents
    #[command(alias = "mission-control")]
    Mc {
        /// Panel layout: tiled, even-horizontal, even-vertical
        #[arg(long, default_value = "tiled")]
        layout: String,
    },
    /// Start the crosslink web dashboard server
    Serve {
        /// Port to listen on
        #[arg(long, default_value = "3100")]
        port: u16,
        /// Directory to serve the React dashboard from (optional)
        #[arg(long)]
        dashboard_dir: Option<PathBuf>,
    },
    /// Manage container-based agent execution
    Container {
        #[command(subcommand)]
        action: ContainerCommands,
    },

    // === Hidden top-level shortcuts (delegate to `issue <verb>`) ===
    /// Create a new issue (shortcut for `issue create`)
    #[command(hide = true)]
    Create {
        /// Issue title
        title: String,
        /// Issue description
        #[arg(short, long)]
        description: Option<String>,
        /// Priority (low, medium, high, critical)
        #[arg(short, long, default_value = "medium")]
        priority: String,
        /// Template (bug, feature, refactor, research)
        #[arg(short, long)]
        template: Option<String>,
        /// Add labels to the issue
        #[arg(short, long)]
        label: Vec<String>,
        /// Set as current session work item
        #[arg(short, long)]
        work: bool,
        /// Skip compaction after creation (batch mode -- display ID assigned later)
        #[arg(long)]
        defer_id: bool,
        /// Parent issue ID (creates a subissue)
        #[arg(long, value_parser = parse_issue_id_clap)]
        parent: Option<i64>,
    },

    /// Quick-create an issue (shortcut for `issue quick`)
    #[command(hide = true)]
    Quick {
        /// Issue title
        title: String,
        /// Issue description
        #[arg(short, long)]
        description: Option<String>,
        /// Priority (low, medium, high, critical)
        #[arg(short, long, default_value = "medium")]
        priority: String,
        /// Template (bug, feature, refactor, research)
        #[arg(short, long)]
        template: Option<String>,
        /// Add labels to the issue
        #[arg(short, long)]
        label: Vec<String>,
        /// Parent issue ID (creates a subissue)
        #[arg(long, value_parser = parse_issue_id_clap)]
        parent: Option<i64>,
    },

    /// List issues (shortcut for `issue list`)
    #[command(hide = true)]
    List {
        /// Filter by status (open, closed, all)
        #[arg(short, long, default_value = "open")]
        status: String,
        /// Filter by label
        #[arg(short, long)]
        label: Option<String>,
        /// Filter by priority
        #[arg(short, long)]
        priority: Option<String>,
    },

    /// Show issue details (shortcut for `issue show`)
    #[command(hide = true)]
    Show {
        /// Issue ID
        #[arg(value_parser = parse_issue_id_clap)]
        id: i64,
    },

    /// Close an issue (shortcut for `issue close`)
    #[command(hide = true)]
    Close {
        /// Issue ID
        #[arg(value_parser = parse_issue_id_clap)]
        id: i64,
        /// Skip changelog entry
        #[arg(long)]
        no_changelog: bool,
    },

    // === Hidden aliases for common agent mistakes ===
    /// Alias for `issue create`
    #[command(hide = true)]
    New {
        /// Issue title
        title: String,
        /// Issue description
        #[arg(short, long)]
        description: Option<String>,
        /// Priority (low, medium, high, critical)
        #[arg(short, long, default_value = "medium")]
        priority: String,
        /// Template (bug, feature, refactor, research)
        #[arg(short, long)]
        template: Option<String>,
        /// Add labels to the issue
        #[arg(short, long)]
        label: Vec<String>,
        /// Set as current session work item
        #[arg(short, long)]
        work: bool,
        /// Parent issue ID (creates a subissue)
        #[arg(long, value_parser = parse_issue_id_clap)]
        parent: Option<i64>,
    },

    /// Alias for `issue list`
    #[command(hide = true)]
    Issues {
        #[command(subcommand)]
        action: Option<IssuesAliasCommands>,
    },

    /// Alias for `issue create --parent`
    #[command(hide = true)]
    Subissue {
        /// Parent issue ID
        #[arg(value_parser = parse_issue_id_clap)]
        parent: i64,
        /// Subissue title
        title: String,
        /// Subissue description
        #[arg(short, long)]
        description: Option<String>,
        /// Priority (low, medium, high, critical)
        #[arg(short, long, default_value = "medium")]
        priority: String,
        /// Add labels to the subissue
        #[arg(short, long)]
        label: Vec<String>,
        /// Set as current session work item
        #[arg(short, long)]
        work: bool,
    },

    /// Alias for `timer start`
    #[command(hide = true, name = "start")]
    TimerStart {
        /// Issue ID
        #[arg(value_parser = parse_issue_id_clap)]
        id: i64,
    },

    /// Alias for `timer stop`
    #[command(hide = true, name = "stop")]
    TimerStop,

    // === Hidden migration aliases ===
    /// Alias for `migrate to-shared`
    #[command(hide = true, name = "migrate-to-shared")]
    MigrateToShared,

    /// Alias for `migrate from-shared`
    #[command(hide = true, name = "migrate-from-shared")]
    MigrateFromShared,

    /// Alias for `migrate rename-branch`
    #[command(hide = true, name = "migrate-rename-branch")]
    MigrateRenameBranch,
}

/// Issue lifecycle subcommands
#[derive(Subcommand)]
enum IssueCommands {
    /// Create a new issue
    Create {
        /// Issue title
        title: String,
        /// Issue description
        #[arg(short, long)]
        description: Option<String>,
        /// Priority (low, medium, high, critical)
        #[arg(short, long, default_value = "medium")]
        priority: String,
        /// Template (bug, feature, refactor, research)
        #[arg(short, long)]
        template: Option<String>,
        /// Add labels to the issue
        #[arg(short, long)]
        label: Vec<String>,
        /// Set as current session work item
        #[arg(short, long)]
        work: bool,
        /// Skip compaction after creation (batch mode -- display ID assigned later)
        #[arg(long)]
        defer_id: bool,
        /// Parent issue ID (creates a subissue)
        #[arg(long, value_parser = parse_issue_id_clap)]
        parent: Option<i64>,
    },

    /// Quick-create an issue and start working on it (create + label + session work)
    Quick {
        /// Issue title
        title: String,
        /// Issue description
        #[arg(short, long)]
        description: Option<String>,
        /// Priority (low, medium, high, critical)
        #[arg(short, long, default_value = "medium")]
        priority: String,
        /// Template (bug, feature, refactor, research)
        #[arg(short, long)]
        template: Option<String>,
        /// Add labels to the issue
        #[arg(short, long)]
        label: Vec<String>,
        /// Parent issue ID (creates a subissue)
        #[arg(long, value_parser = parse_issue_id_clap)]
        parent: Option<i64>,
    },

    /// List issues
    List {
        /// Filter by status (open, closed, all)
        #[arg(short, long, default_value = "open")]
        status: String,
        /// Filter by label
        #[arg(short, long)]
        label: Option<String>,
        /// Filter by priority
        #[arg(short, long)]
        priority: Option<String>,
        /// Query an external repository (URL, local path, or @alias)
        #[arg(long)]
        repo: Option<String>,
        /// Force refresh of cached external data
        #[arg(long)]
        refresh: bool,
    },

    /// Search issues by text
    Search {
        /// Search query
        query: String,
        /// Query an external repository (URL, local path, or @alias)
        #[arg(long)]
        repo: Option<String>,
        /// Force refresh of cached external data
        #[arg(long)]
        refresh: bool,
    },

    /// Show issue details
    Show {
        /// Issue ID
        #[arg(value_parser = parse_issue_id_clap)]
        id: i64,
        /// Query an external repository (URL, local path, or @alias)
        #[arg(long)]
        repo: Option<String>,
        /// Force refresh of cached external data
        #[arg(long)]
        refresh: bool,
    },

    /// Update an issue
    Update {
        /// Issue ID
        #[arg(value_parser = parse_issue_id_clap)]
        id: i64,
        /// New title
        #[arg(short, long)]
        title: Option<String>,
        /// New description
        #[arg(short, long)]
        description: Option<String>,
        /// New priority
        #[arg(short, long)]
        priority: Option<String>,
    },

    /// Close an issue
    Close {
        /// Issue ID
        #[arg(value_parser = parse_issue_id_clap)]
        id: i64,
        /// Skip changelog entry
        #[arg(long)]
        no_changelog: bool,
    },

    /// Close all issues matching filters
    CloseAll {
        /// Filter by label
        #[arg(short, long)]
        label: Option<String>,
        /// Filter by priority
        #[arg(short, long)]
        priority: Option<String>,
        /// Skip changelog entries
        #[arg(long)]
        no_changelog: bool,
    },

    /// Reopen a closed issue
    Reopen {
        /// Issue ID
        #[arg(value_parser = parse_issue_id_clap)]
        id: i64,
    },

    /// Delete an issue
    Delete {
        /// Issue ID
        #[arg(value_parser = parse_issue_id_clap)]
        id: i64,
        /// Skip confirmation
        #[arg(short, long)]
        force: bool,
    },

    /// Add a comment to an issue
    Comment {
        /// Issue ID
        #[arg(value_parser = parse_issue_id_clap)]
        id: i64,
        /// Comment text
        text: String,
        /// Comment kind (note, plan, decision, observation, blocker, resolution, result, handoff, human)
        #[arg(long, default_value = "note")]
        kind: String,
    },

    /// Log a driver intervention on an issue
    Intervene {
        /// Issue ID
        #[arg(value_parser = parse_issue_id_clap)]
        id: i64,
        /// Description of the intervention
        description: String,
        /// Trigger type (tool_rejected, tool_blocked, redirect, context_provided, manual_action, question_answered)
        #[arg(long)]
        trigger: String,
        /// Context: what the agent was attempting when intervention occurred
        #[arg(long)]
        context: Option<String>,
    },

    /// Add a label to an issue
    Label {
        /// Issue ID
        #[arg(value_parser = parse_issue_id_clap)]
        id: i64,
        /// Label name
        label: String,
    },

    /// Remove a label from an issue
    Unlabel {
        /// Issue ID
        #[arg(value_parser = parse_issue_id_clap)]
        id: i64,
        /// Label name
        label: String,
    },

    /// Mark an issue as blocked by another
    Block {
        /// Issue ID that is blocked
        #[arg(value_parser = parse_issue_id_clap)]
        id: i64,
        /// Issue ID that is blocking
        #[arg(value_parser = parse_issue_id_clap)]
        blocker: i64,
    },

    /// Remove a blocking relationship
    Unblock {
        /// Issue ID that was blocked
        #[arg(value_parser = parse_issue_id_clap)]
        id: i64,
        /// Issue ID that was blocking
        #[arg(value_parser = parse_issue_id_clap)]
        blocker: i64,
    },

    /// List blocked issues
    Blocked,

    /// List issues ready to work on (no open blockers)
    Ready,

    /// Link two related issues
    Relate {
        /// First issue ID
        #[arg(value_parser = parse_issue_id_clap)]
        id: i64,
        /// Second issue ID
        #[arg(value_parser = parse_issue_id_clap)]
        related: i64,
    },

    /// Remove a relation between issues
    Unrelate {
        /// First issue ID
        #[arg(value_parser = parse_issue_id_clap)]
        id: i64,
        /// Second issue ID
        #[arg(value_parser = parse_issue_id_clap)]
        related: i64,
    },

    /// List related issues
    Related {
        /// Issue ID
        #[arg(value_parser = parse_issue_id_clap)]
        id: i64,
    },

    /// Suggest the next issue to work on
    Next,

    /// Show issues as a tree hierarchy
    Tree {
        /// Filter by status (open, closed, all)
        #[arg(short, long, default_value = "all")]
        status: String,
    },

    /// Mark tests as run (resets test reminder)
    Tested,
}

/// Timer subcommands
#[derive(Subcommand)]
enum TimerCommands {
    /// Start a timer for an issue
    Start {
        /// Issue ID
        #[arg(value_parser = parse_issue_id_clap)]
        id: i64,
    },
    /// Stop the current timer
    Stop,
    /// Show current timer status
    Show,
}

/// Migration subcommands
#[derive(Subcommand)]
enum MigrateCommands {
    /// Migrate local SQLite issues to shared coordination branch
    ToShared,
    /// Import shared issues from coordination branch into local SQLite
    FromShared,
    /// Rename coordination branch from crosslink/locks to crosslink/hub
    RenameBranch,
}

/// Helper enum for `crosslink issues <subcommand>` alias
#[derive(Subcommand)]
enum IssuesAliasCommands {
    /// Alias for `issue list`
    List {
        /// Filter by status (open, closed, all)
        #[arg(short, long, default_value = "open")]
        status: String,
        /// Filter by label
        #[arg(short, long)]
        label: Option<String>,
        /// Filter by priority
        #[arg(short, long)]
        priority: Option<String>,
    },
}

#[derive(Subcommand)]
enum ContainerCommands {
    /// Build the crosslink agent container image
    Build {
        /// Rebuild from scratch (no cache)
        #[arg(long)]
        force: bool,
        /// Image tag (default: latest)
        #[arg(long)]
        tag: Option<String>,
        /// Path to a custom Dockerfile
        #[arg(long)]
        dockerfile: Option<String>,
    },
    /// Start a task container for a worktree
    Start {
        /// Path to the worktree directory
        worktree: String,
        /// Container name (default: derived from worktree slug)
        #[arg(long)]
        name: Option<String>,
        /// Path to the prompt file (default: KICKOFF.md in worktree)
        #[arg(long)]
        prompt: Option<String>,
        /// Crosslink issue ID being worked on
        #[arg(long)]
        issue: Option<i64>,
        /// Memory limit (default: auto-detect from host)
        #[arg(long)]
        memory: Option<String>,
    },
    /// List running task containers
    Ps,
    /// Stream logs from a container
    Logs {
        /// Container name
        name: String,
        /// Follow log output
        #[arg(short, long)]
        follow: bool,
        /// Number of lines to show (default: 100)
        #[arg(long)]
        tail: Option<u32>,
    },
    /// Stop a running container
    Stop {
        /// Container name
        name: String,
    },
    /// Remove a stopped container
    Rm {
        /// Container name
        name: String,
    },
    /// Stop and remove a container
    Kill {
        /// Container name
        name: String,
    },
    /// Open a shell inside a running container
    Shell {
        /// Container name
        name: String,
    },
    /// Snapshot a container as a cached image (preserves installed toolchains)
    Snapshot {
        /// Container name
        name: String,
        /// Image tag for the snapshot (default: cached)
        #[arg(long)]
        tag: Option<String>,
    },
}

#[derive(Subcommand)]
enum ArchiveCommands {
    /// Archive a closed issue
    Add {
        /// Issue ID
        #[arg(value_parser = parse_issue_id_clap)]
        id: i64,
    },
    /// Unarchive an issue (restore to closed)
    Remove {
        /// Issue ID
        #[arg(value_parser = parse_issue_id_clap)]
        id: i64,
    },
    /// List archived issues
    List,
    /// Archive all issues closed more than N days ago
    Older {
        /// Days threshold
        days: i64,
    },
}

#[derive(Subcommand)]
enum MilestoneCommands {
    /// Create a new milestone
    Create {
        /// Milestone name
        name: String,
        /// Description
        #[arg(short, long)]
        description: Option<String>,
    },
    /// List milestones
    List {
        /// Filter by status (open, closed, all)
        #[arg(short, long, default_value = "open")]
        status: String,
    },
    /// Show milestone details
    Show {
        /// Milestone ID
        id: i64,
    },
    /// Add issues to a milestone
    Add {
        /// Milestone ID
        id: i64,
        /// Issue IDs to add
        issues: Vec<i64>,
    },
    /// Remove an issue from a milestone
    Remove {
        /// Milestone ID
        id: i64,
        /// Issue ID to remove
        issue: i64,
    },
    /// Close a milestone
    Close {
        /// Milestone ID
        id: i64,
    },
    /// Delete a milestone
    Delete {
        /// Milestone ID
        id: i64,
    },
}

#[derive(Subcommand)]
enum SessionCommands {
    /// Start a new session
    Start,
    /// End the current session
    End {
        /// Handoff notes for the next session
        #[arg(short, long)]
        notes: Option<String>,
    },
    /// Show current session status
    Status,
    /// Set the issue being worked on
    Work {
        /// Issue ID
        #[arg(value_parser = parse_issue_id_clap)]
        id: i64,
    },
    /// Show handoff notes from the previous session
    LastHandoff,
    /// Record last action for context compression breadcrumbs
    Action {
        /// Description of what you just did or are doing
        text: String,
    },
}

#[derive(Subcommand)]
enum CpitdCommands {
    /// Scan for code clones and create issues
    Scan {
        /// Paths to scan (defaults to current directory)
        paths: Vec<String>,
        /// Minimum token sequence length to report
        #[arg(long, default_value = "50")]
        min_tokens: u32,
        /// Glob patterns to exclude (repeatable)
        #[arg(long)]
        ignore: Vec<String>,
        /// Show what would be created without creating issues
        #[arg(long = "dry-run")]
        dry_run: bool,
    },
    /// Show open clone issues
    Status,
    /// Close all open clone issues
    Clear,
}

#[derive(Subcommand)]
enum DaemonCommands {
    /// Start the background daemon
    Start,
    /// Stop the background daemon
    Stop,
    /// Check daemon status
    Status,
    /// Internal: run the daemon loop (used by start)
    #[command(hide = true)]
    Run {
        #[arg(long)]
        dir: PathBuf,
    },
}

#[derive(Subcommand)]
enum AgentCommands {
    /// Initialize agent identity on this machine
    Init {
        /// Agent ID (alphanumeric, hyphens, underscores)
        agent_id: String,
        /// Agent description
        #[arg(short, long)]
        description: Option<String>,
        /// Skip SSH key generation
        #[arg(long)]
        no_key: bool,
        /// Overwrite existing agent configuration
        #[arg(long)]
        force: bool,
    },
    /// Show current agent identity
    Status,
    /// Bootstrap agent identity in a new or existing repo clone
    Bootstrap {
        /// Git repository URL to clone
        #[arg(long)]
        repo: String,
        /// Agent ID (alphanumeric, hyphens, underscores)
        #[arg(long)]
        identity: String,
        /// Branch to checkout after cloning
        #[arg(long)]
        branch: Option<String>,
        /// Agent description
        #[arg(short, long)]
        description: Option<String>,
        /// Skip SSH key generation
        #[arg(long)]
        no_key: bool,
        /// Target directory (default: current directory)
        #[arg(long, default_value = ".")]
        target: String,
    },
}

#[derive(Subcommand)]
enum TrustCommands {
    /// Approve an agent's signing key
    Approve {
        /// Agent ID to approve
        agent_id: String,
    },
    /// Revoke an agent's signing key
    Revoke {
        /// Agent ID to revoke
        agent_id: String,
    },
    /// List all trusted signers
    List,
    /// Show agent keys awaiting approval
    Pending,
    /// Check trust status of a specific agent
    Check {
        /// Agent ID to check
        agent_id: String,
    },
}

#[derive(Subcommand)]
enum LocksCommands {
    /// List all active locks
    List,
    /// Check if a specific issue is locked
    Check {
        /// Issue ID
        #[arg(value_parser = parse_issue_id_clap)]
        id: i64,
    },
    /// Claim a lock on an issue
    Claim {
        /// Issue ID
        #[arg(value_parser = parse_issue_id_clap)]
        id: i64,
        /// Branch name for context
        #[arg(short, long)]
        branch: Option<String>,
    },
    /// Release a lock on an issue
    Release {
        /// Issue ID
        #[arg(value_parser = parse_issue_id_clap)]
        id: i64,
    },
    /// Steal a stale lock from another agent
    Steal {
        /// Issue ID
        #[arg(value_parser = parse_issue_id_clap)]
        id: i64,
    },
}

#[derive(Subcommand)]
enum WorkflowCommands {
    /// Compare deployed policy files against embedded defaults
    Diff {
        /// Filter by section: tracking, rules, languages, hooks
        #[arg(short, long)]
        section: Option<String>,
        /// CI mode: exit 1 if any files have drifted without '# crosslink:custom' marker
        #[arg(long)]
        check: bool,
    },
    /// Show chronological comment trail for an issue
    Trail {
        /// Issue ID
        #[arg(value_parser = parse_issue_id_clap)]
        id: i64,
        /// Filter by comment kind(s), comma-separated (e.g. plan,decision)
        #[arg(long)]
        kind: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum StyleCommands {
    /// Set the house style source (a git repo URL + optional ref)
    Set {
        /// Git repository URL for the house style
        url: String,
        /// Branch or tag to track (default: main)
        #[arg(long, name = "ref")]
        ref_name: Option<String>,
    },
    /// Sync: pull latest from the house style source
    Sync {
        /// Show what would change without writing
        #[arg(long = "dry-run")]
        dry_run: bool,
    },
    /// Diff: show what's drifted from house style
    Diff,
    /// Show current house style configuration
    Show,
    /// Remove house style association
    Unset,
}

#[derive(Subcommand)]
enum KnowledgeCommands {
    /// Create a new knowledge page
    Add {
        /// Page slug (filename without .md)
        slug: String,
        /// Page title
        #[arg(short, long)]
        title: Option<String>,
        /// Tags for the page (repeatable)
        #[arg(long)]
        tag: Vec<String>,
        /// Source URL (repeatable)
        #[arg(long)]
        source: Vec<String>,
        /// Page content (body text after frontmatter)
        #[arg(long)]
        content: Option<String>,
        /// Import from a design document file
        #[arg(long, value_name = "PATH")]
        from_doc: Option<PathBuf>,
        /// (Rejected — external sources are read-only)
        #[arg(long, hide = true)]
        repo: Option<String>,
    },
    /// Display a knowledge page
    Show {
        /// Page slug
        slug: String,
        /// Query an external repository (URL, local path, or @alias)
        #[arg(long)]
        repo: Option<String>,
        /// Force refresh of cached external data
        #[arg(long)]
        refresh: bool,
    },
    /// List all knowledge pages
    List {
        /// Filter by tag
        #[arg(long)]
        tag: Option<String>,
        /// Filter by contributor
        #[arg(long)]
        contributor: Option<String>,
        /// Filter pages updated since date (YYYY-MM-DD)
        #[arg(long)]
        since: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Query an external repository (URL, local path, or @alias)
        #[arg(long)]
        repo: Option<String>,
        /// Force refresh of cached external data
        #[arg(long)]
        refresh: bool,
    },
    /// Update an existing knowledge page
    Edit {
        /// Page slug
        slug: String,
        /// Append content to the page (mutually exclusive with section flags)
        #[arg(long, group = "content_mode")]
        append: Option<String>,
        /// Replace page content entirely
        #[arg(long)]
        content: Option<String>,
        /// Replace the content of a specific markdown section (requires --content)
        #[arg(long, value_name = "HEADING", group = "content_mode")]
        replace_section: Option<String>,
        /// Append to a specific markdown section (requires --content)
        #[arg(long, value_name = "HEADING", group = "content_mode")]
        append_to_section: Option<String>,
        /// Add tags (repeatable)
        #[arg(long)]
        tag: Vec<String>,
        /// Add source URL (repeatable)
        #[arg(long)]
        source: Vec<String>,
        /// Replace content from a document file
        #[arg(long, value_name = "PATH")]
        from_doc: Option<PathBuf>,
        /// (Rejected — external sources are read-only)
        #[arg(long, hide = true)]
        repo: Option<String>,
    },
    /// Remove a knowledge page
    Remove {
        /// Page slug
        slug: String,
        /// (Rejected — external sources are read-only)
        #[arg(long, hide = true)]
        repo: Option<String>,
    },
    /// Manually sync from remote
    Sync {
        /// (Rejected — external sources are read-only)
        #[arg(long, hide = true)]
        repo: Option<String>,
    },
    /// Bulk import markdown files as knowledge pages
    Import {
        /// Directory containing .md files to import
        directory: PathBuf,
        /// Extra tags to apply to all imports (repeatable)
        #[arg(long)]
        tag: Vec<String>,
        /// Overwrite existing pages
        #[arg(long)]
        overwrite: bool,
        /// Preview imports without writing
        #[arg(long = "dry-run")]
        dry_run: bool,
        /// (Rejected — external sources are read-only)
        #[arg(long, hide = true)]
        repo: Option<String>,
    },
    /// Search knowledge page content
    Search {
        /// Search query (case-insensitive substring match)
        query: Option<String>,
        /// Number of context lines around each match
        #[arg(short = 'C', long, default_value = "1")]
        context: usize,
        /// Search by source URL domain instead of content
        #[arg(long)]
        source: Option<String>,
        /// Filter results by tag
        #[arg(long)]
        tag: Option<String>,
        /// Filter results updated since date (YYYY-MM-DD)
        #[arg(long)]
        since: Option<String>,
        /// Filter results by contributor
        #[arg(long)]
        contributor: Option<String>,
        /// Query an external repository (URL, local path, or @alias)
        #[arg(long)]
        repo: Option<String>,
        /// Force refresh of cached external data
        #[arg(long)]
        refresh: bool,
    },
}

#[derive(Subcommand)]
enum IntegrityCommands {
    /// Check counter consistency (next_display_id, next_comment_id)
    Counters {
        /// Repair inconsistencies by recalculating from data
        #[arg(long)]
        repair: bool,
    },
    /// Verify SQLite matches JSON issue files
    Hydration {
        /// Re-hydrate SQLite from JSON
        #[arg(long)]
        repair: bool,
    },
    /// Check for stale or orphaned locks
    Locks {
        /// Release stale locks
        #[arg(long)]
        repair: bool,
    },
    /// Verify SQLite schema version
    Schema {
        /// Re-run migrations to update schema
        #[arg(long)]
        repair: bool,
    },
}

#[derive(Subcommand)]
enum KickoffCommands {
    /// Launch a new agent to implement a feature
    Run {
        /// Human-readable feature description
        description: String,
        /// Existing issue to work on (creates one if omitted)
        #[arg(long)]
        issue: Option<i64>,
        /// Container runtime: none (local process), docker, podman
        #[arg(long, default_value = "none")]
        container: String,
        /// Verification level: local, ci, thorough
        #[arg(long, default_value = "local")]
        verify: String,
        /// LLM model to use
        #[arg(long, default_value = "opus")]
        model: String,
        /// Container image (for --container docker/podman)
        #[arg(long, default_value = "ghcr.io/forecast-bio/crosslink-agent:latest")]
        image: String,
        /// Max runtime before killing agent (e.g. "1h", "30m")
        #[arg(long, default_value = "1h")]
        timeout: String,
        /// Print the agent prompt without launching
        #[arg(long = "dry-run")]
        dry_run: bool,
        /// Branch to use (auto-creates feature branch if omitted)
        #[arg(long)]
        branch: Option<String>,
        /// Path to a design document (markdown) with structured requirements
        #[arg(long, value_name = "PATH")]
        doc: Option<PathBuf>,
        /// Pass --dangerously-skip-permissions to the claude CLI (for sandboxed agents)
        #[arg(long)]
        skip_permissions: bool,
    },
    /// Check status of a running kickoff agent
    Status {
        /// Agent ID or branch name
        agent: String,
    },
    /// Tail an agent's event log
    Logs {
        /// Agent ID or branch name
        agent: String,
        /// Number of recent events to show
        #[arg(short, long, default_value = "20")]
        lines: usize,
    },
    /// Stop a running kickoff agent
    Stop {
        /// Agent ID or branch name
        agent: String,
        /// Force kill (SIGKILL instead of SIGTERM)
        #[arg(long)]
        force: bool,
    },
    /// Analyze a design document against the codebase (read-only)
    Plan {
        /// Path to design document
        doc: PathBuf,
        /// Existing issue to associate with
        #[arg(long)]
        issue: Option<i64>,
        /// LLM model to use
        #[arg(long, default_value = "opus")]
        model: String,
        /// Max runtime (e.g. "30m", "1h")
        #[arg(long, default_value = "30m")]
        timeout: String,
        /// Print the analysis prompt without launching
        #[arg(long = "dry-run")]
        dry_run: bool,
    },
    /// Display a gap report from a previous plan analysis
    ShowPlan {
        /// Agent ID or branch slug
        agent: String,
    },
    /// Display the spec validation report from a completed agent
    Report {
        /// Agent ID or branch slug (required unless --all)
        agent: Option<String>,
        /// Output as raw JSON
        #[arg(long)]
        json: bool,
        /// Output as PR-ready markdown
        #[arg(long)]
        markdown: bool,
        /// Show aggregated reports from all agent worktrees
        #[arg(long)]
        all: bool,
    },
    /// List all kickoff agents across worktrees, tmux, and Docker
    List {
        /// Filter by status: running, done, failed, all
        #[arg(long, default_value = "all")]
        status: String,
    },
    /// Remove completed/stale agent worktrees, tmux sessions, and containers
    Cleanup {
        /// Show what would be cleaned without doing anything
        #[arg(long = "dry-run")]
        dry_run: bool,
        /// Also clean up potentially stale agents (not just confirmed-done)
        #[arg(long)]
        force: bool,
        /// Keep the N most recently completed agents
        #[arg(long, default_value = "0")]
        keep: usize,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum ConfigCommands {
    /// Show all configuration with default annotations
    Show,
    /// Get a specific config value
    Get {
        /// Config key name
        key: String,
    },
    /// Set a config value
    Set {
        /// Config key name
        key: String,
        /// Value to set (for arrays: comma-separated, or use --add/--remove)
        value: Option<String>,
        /// Add a value to an array field
        #[arg(long)]
        add: Option<String>,
        /// Remove a value from an array field
        #[arg(long)]
        remove: Option<String>,
    },
    /// List all available config keys with descriptions
    List,
    /// Reset config to defaults (all keys, or a single key)
    Reset {
        /// Specific key to reset (omit for full reset)
        key: Option<String>,
    },
    /// Show differences from default config
    Diff,
}

#[derive(Subcommand)]
enum SwarmCommands {
    /// Initialize a swarm plan from a design document
    Init {
        /// Path to design document (markdown)
        #[arg(long, value_name = "PATH")]
        doc: PathBuf,
    },
    /// Show current swarm status (agents, phases, progress, next steps)
    Status,
    /// Reconstruct state and show next steps for resuming
    Resume,
    /// Sync agent statuses from live worktree state into phase JSON
    SyncStatus,
    /// Associate an external agent/branch with a swarm slot
    Adopt {
        /// Agent slug or branch name of the external agent
        agent: String,
        /// Swarm slot slug to assign the agent to
        #[arg(long, value_name = "SLUG")]
        slot: String,
    },
    /// Archive the current swarm and clear the active slot
    Archive,
    /// Reset the active swarm (archives by default)
    Reset {
        /// Delete without archiving
        #[arg(long)]
        no_archive: bool,
    },
    /// List active and archived swarms
    #[command(name = "list")]
    ListSwarms,
    /// Launch all planned agents for a phase
    Launch {
        /// Phase slug (e.g. "phase-1")
        phase: String,
        /// Retry only previously failed agents
        #[arg(long)]
        retry_failed: bool,
        /// Check budget before launching; block if insufficient
        #[arg(long)]
        budget_aware: bool,
    },
    /// Run the project test suite as a phase gate
    Gate {
        /// Phase slug (e.g. "phase-1")
        phase: String,
    },
    /// Record a checkpoint after a phase completes
    Checkpoint {
        /// Phase slug (e.g. "phase-1")
        phase: String,
        /// Handoff notes for the next session
        #[arg(long)]
        notes: Option<String>,
        /// Checkpoint even if gate hasn't passed
        #[arg(long)]
        force: bool,
    },
    /// Set budget parameters (window duration, model)
    Config {
        /// Budget time window (e.g. "5h", "3h30m")
        #[arg(long, value_name = "DURATION")]
        budget_window: String,
        /// Model to estimate costs for
        #[arg(long, default_value = "opus")]
        model: String,
    },
    /// Estimate wall-clock cost for a phase
    Estimate {
        /// Phase slug (e.g. "phase-1")
        phase: String,
    },
    /// Scan completed agents and update cost history
    Harvest,
    /// Plan a multi-phase build across budget windows
    Plan {
        /// Budget window duration (e.g. "5h"); uses saved config if omitted
        #[arg(long, value_name = "DURATION")]
        budget_window: Option<String>,
    },
    /// Show the current window plan (alias for plan with saved config)
    PlanShow,
    /// Launch parallel adversarial review agents across codebase partitions
    Review {
        /// Number of review agents to launch
        #[arg(long, default_value = "4")]
        agents: usize,
        /// Review mandate type
        #[arg(long, default_value = "adversarial")]
        mandate: String,
        /// Output path for consolidated findings document
        #[arg(long, value_name = "PATH")]
        doc: Option<PathBuf>,
        /// Also file issues for findings after review
        #[arg(long)]
        file_issues: bool,
        /// Also launch fix agents after filing issues
        #[arg(long)]
        fix: bool,
    },
    /// Launch parallel fix agents, one per issue
    Fix {
        /// Comma-separated issue numbers (e.g., "326,327,328")
        #[arg(long, value_name = "IDS")]
        issues: Option<String>,
        /// Label filter to select issues (e.g., "review-finding")
        #[arg(long, value_name = "LABEL")]
        from_label: Option<String>,
        /// Maximum number of concurrent agents
        #[arg(long, default_value = "6")]
        max_agents: usize,
        /// Check budget before launching
        #[arg(long)]
        budget_aware: bool,
    },
    /// Merge changes from completed agent worktrees into a single branch
    Merge {
        /// Target branch name for merged changes
        #[arg(long, default_value = "swarm-combined")]
        branch: String,
        /// Only analyze conflicts, don't apply changes
        #[arg(long)]
        dry_run: bool,
        /// Agent slugs to merge (default: all completed agents from current swarm)
        #[arg(long, value_name = "SLUGS")]
        agents: Option<String>,
    },
    /// Move an agent to a different phase
    #[command(name = "move")]
    MoveAgent {
        /// Agent slug to move
        agent: String,
        /// Target phase name
        #[arg(long, value_name = "PHASE")]
        to_phase: String,
    },
    /// Merge two phases into one
    MergePhases {
        /// First phase name
        phase_a: String,
        /// Second phase name
        phase_b: String,
    },
    /// Split a phase after a specific agent
    SplitPhase {
        /// Phase name to split
        phase: String,
        /// Split after this agent slug
        #[arg(long, value_name = "SLUG")]
        after: String,
    },
    /// Remove an agent from the plan
    RemoveAgent {
        /// Agent slug to remove
        agent: String,
    },
    /// Reorder a phase to a new position
    Reorder {
        /// Phase name to move
        phase: String,
        /// New position (1-based)
        #[arg(long)]
        position: usize,
    },
    /// Rename a phase
    RenamePhase {
        /// Current phase name
        old: String,
        /// New phase name
        new: String,
    },
    /// Continue a paused pipeline (e.g., after human checkpoint)
    ReviewContinue,
    /// Show pipeline status
    ReviewStatus,
    /// Run the full review→fix pipeline (standalone pipeline driver with stage logging)
    Pipeline {
        /// Number of agents
        #[arg(long, default_value = "4")]
        agents: usize,
        /// Review mandate
        #[arg(long, default_value = "adversarial")]
        mandate: String,
        /// Target branch for merging fixes
        #[arg(long, default_value = "main")]
        target_branch: String,
        /// Automatically fix findings
        #[arg(long)]
        auto_fix: bool,
        /// Automatically file issues for findings
        #[arg(long)]
        auto_file_issues: bool,
    },
    /// Initialize trust model configuration (writes swarm.toml)
    TrustInit {
        /// Trust model type: local-only, multi-tenant, public-api
        #[arg(long, default_value = "local-only")]
        model: String,
    },
}

#[derive(Subcommand)]
enum ContextCommands {
    /// Measure context injection sizes and estimate token overhead
    Measure {
        /// Show additional details (hook config contents, etc.)
        #[arg(short, long)]
        verbose: bool,
    },
    /// Verify all expected crosslink files are deployed and valid
    Check,
}

fn init_tracing(log_level: &str, log_format: &str) {
    use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};
    let filter = EnvFilter::try_new(log_level).unwrap_or_else(|_| EnvFilter::new("warn"));
    if log_format == "json" {
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer().json().with_writer(std::io::stderr))
            .init();
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer().with_target(false).with_writer(std::io::stderr))
            .init();
    }
}

fn find_crosslink_dir() -> Result<PathBuf> {
    let mut current = env::current_dir()?;

    // First, walk up from cwd looking for .crosslink (works in main repo)
    let start = current.clone();
    loop {
        let candidate = current.join(".crosslink");
        if candidate.is_dir() {
            return Ok(candidate);
        }

        if !current.pop() {
            break;
        }
    }

    // Not found — check if we're in a git worktree and look in the main repo root
    if let Some(main_root) = utils::resolve_main_repo_root(&start) {
        let candidate = main_root.join(".crosslink");
        if candidate.is_dir() {
            return Ok(candidate);
        }
    }

    bail!("Not a crosslink repository (or any parent). Run 'crosslink init' first.");
}

fn get_db() -> Result<Database> {
    let crosslink_dir = find_crosslink_dir()?;
    let db_path = crosslink_dir.join("issues.db");
    Database::open(&db_path).context("Failed to open database")
}

/// Try to create a SharedWriter for multi-agent mode.
/// Returns None if agent.json is absent or sync cache isn't initialized.
fn get_writer(crosslink_dir: &std::path::Path) -> Option<shared_writer::SharedWriter> {
    match shared_writer::SharedWriter::new(crosslink_dir) {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!("SharedWriter unavailable: {}", e);
            None
        }
    }
}

/// Clap value parser for issue IDs (supports `L1` offline notation).
fn parse_issue_id_clap(s: &str) -> std::result::Result<i64, String> {
    parse_issue_id(s).map_err(|e| e.to_string())
}

/// Parse an issue ID string, supporting both regular IDs and offline local IDs.
///
/// - `"42"` → `42` (regular display ID)
/// - `"L1"` or `"l1"` → `-1` (offline local ID, stored as negative in SQLite)
///
/// Used when offline issue creation is enabled (display_id: null in JSON).
fn parse_issue_id(s: &str) -> Result<i64> {
    if let Some(n) = s.strip_prefix('L').or_else(|| s.strip_prefix('l')) {
        let num: i64 = n
            .parse()
            .with_context(|| format!("Invalid local issue ID: {}", s))?;
        if num <= 0 {
            bail!("Local issue ID must be positive: {}", s);
        }
        Ok(-num)
    } else {
        s.parse()
            .with_context(|| format!("Invalid issue ID: {}", s))
    }
}

/// Emit a hint to stderr (suppressed in quiet mode).
fn hint(quiet: bool, msg: &str) {
    if !quiet {
        tracing::info!("hint: {}", msg);
    }
}

/// Dispatch an IssueCommands variant.
fn dispatch_issue(action: IssueCommands, quiet: bool, json: bool) -> Result<()> {
    match action {
        IssueCommands::Create {
            title,
            description,
            priority,
            template,
            label,
            work,
            defer_id,
            parent,
        } => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            let writer = get_writer(&crosslink_dir);
            if let Some(parent_id) = parent {
                let opts = commands::create::CreateOpts {
                    labels: &label,
                    work,
                    quiet,
                    crosslink_dir: Some(&crosslink_dir),
                    defer_id: false,
                };
                commands::create::run_subissue(
                    &db,
                    writer.as_ref(),
                    parent_id,
                    &title,
                    description.as_deref(),
                    &priority,
                    &opts,
                )
            } else {
                let opts = commands::create::CreateOpts {
                    labels: &label,
                    work,
                    quiet,
                    crosslink_dir: Some(&crosslink_dir),
                    defer_id,
                };
                commands::create::run(
                    &db,
                    writer.as_ref(),
                    &title,
                    description.as_deref(),
                    &priority,
                    template.as_deref(),
                    &opts,
                )
            }
        }

        IssueCommands::Quick {
            title,
            description,
            priority,
            template,
            label,
            parent,
        } => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            let writer = get_writer(&crosslink_dir);
            if let Some(parent_id) = parent {
                let opts = commands::create::CreateOpts {
                    labels: &label,
                    work: true,
                    quiet,
                    crosslink_dir: Some(&crosslink_dir),
                    defer_id: false,
                };
                commands::create::run_subissue(
                    &db,
                    writer.as_ref(),
                    parent_id,
                    &title,
                    description.as_deref(),
                    &priority,
                    &opts,
                )
            } else {
                let opts = commands::create::CreateOpts {
                    labels: &label,
                    work: true,
                    quiet,
                    crosslink_dir: Some(&crosslink_dir),
                    defer_id: false,
                };
                commands::create::run(
                    &db,
                    writer.as_ref(),
                    &title,
                    description.as_deref(),
                    &priority,
                    template.as_deref(),
                    &opts,
                )
            }
        }

        IssueCommands::List {
            status,
            label,
            priority,
            repo,
            refresh,
        } => {
            if let Some(repo_value) = repo {
                let crosslink_dir = find_crosslink_dir()?;
                commands::external_issues::list(
                    &crosslink_dir,
                    &repo_value,
                    Some(&status),
                    label.as_deref(),
                    priority.as_deref(),
                    refresh,
                    json,
                    quiet,
                )
            } else {
                let db = get_db()?;
                if json {
                    commands::list::run_json(
                        &db,
                        Some(&status),
                        label.as_deref(),
                        priority.as_deref(),
                    )
                } else {
                    commands::list::run(&db, Some(&status), label.as_deref(), priority.as_deref())
                }
            }
        }

        IssueCommands::Search {
            query,
            repo,
            refresh,
        } => {
            if let Some(repo_value) = repo {
                let crosslink_dir = find_crosslink_dir()?;
                commands::external_issues::search(
                    &crosslink_dir,
                    &repo_value,
                    &query,
                    refresh,
                    json,
                    quiet,
                )
            } else {
                let db = get_db()?;
                if json {
                    commands::search::run_json(&db, &query)
                } else {
                    commands::search::run(&db, &query)
                }
            }
        }

        IssueCommands::Show { id, repo, refresh } => {
            if let Some(repo_value) = repo {
                let crosslink_dir = find_crosslink_dir()?;
                commands::external_issues::show(
                    &crosslink_dir,
                    &repo_value,
                    id,
                    refresh,
                    json,
                    quiet,
                )
            } else {
                let db = get_db()?;
                if json {
                    commands::show::run_json(&db, id)
                } else {
                    commands::show::run(&db, id)
                }
            }
        }

        IssueCommands::Update {
            id,
            title,
            description,
            priority,
        } => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            let writer = get_writer(&crosslink_dir);
            commands::update::run(
                &db,
                writer.as_ref(),
                id,
                title.as_deref(),
                description.as_deref(),
                priority.as_deref(),
            )
        }

        IssueCommands::Close { id, no_changelog } => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            let writer = get_writer(&crosslink_dir);
            if quiet {
                commands::status::close_quiet(
                    &db,
                    writer.as_ref(),
                    id,
                    !no_changelog,
                    &crosslink_dir,
                )
            } else {
                commands::status::close(&db, writer.as_ref(), id, !no_changelog, &crosslink_dir)
            }
        }

        IssueCommands::CloseAll {
            label,
            priority,
            no_changelog,
        } => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            let writer = get_writer(&crosslink_dir);
            commands::status::close_all(
                &db,
                writer.as_ref(),
                label.as_deref(),
                priority.as_deref(),
                !no_changelog,
                &crosslink_dir,
            )
        }

        IssueCommands::Reopen { id } => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            let writer = get_writer(&crosslink_dir);
            commands::status::reopen(&db, writer.as_ref(), id)
        }

        IssueCommands::Delete { id, force } => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            let writer = get_writer(&crosslink_dir);
            commands::delete::run(&db, writer.as_ref(), id, force)
        }

        IssueCommands::Comment { id, text, kind } => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            let writer = get_writer(&crosslink_dir);
            commands::comment::run(&db, writer.as_ref(), id, &text, &kind)
        }

        IssueCommands::Intervene {
            id,
            description,
            trigger,
            context,
        } => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            let writer = get_writer(&crosslink_dir);
            commands::intervene::run(
                &db,
                writer.as_ref(),
                id,
                &description,
                &trigger,
                context.as_deref(),
                &crosslink_dir,
            )
        }

        IssueCommands::Label { id, label } => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            let writer = get_writer(&crosslink_dir);
            commands::label::add(&db, writer.as_ref(), id, &label)
        }

        IssueCommands::Unlabel { id, label } => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            let writer = get_writer(&crosslink_dir);
            commands::label::remove(&db, writer.as_ref(), id, &label)
        }

        IssueCommands::Block { id, blocker } => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            let writer = get_writer(&crosslink_dir);
            commands::deps::block(&db, writer.as_ref(), id, blocker)
        }

        IssueCommands::Unblock { id, blocker } => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            let writer = get_writer(&crosslink_dir);
            commands::deps::unblock(&db, writer.as_ref(), id, blocker)
        }

        IssueCommands::Blocked => {
            let db = get_db()?;
            commands::deps::list_blocked(&db, json)
        }

        IssueCommands::Ready => {
            let db = get_db()?;
            commands::deps::list_ready(&db, json)
        }

        IssueCommands::Relate { id, related } => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            let writer = get_writer(&crosslink_dir);
            commands::relate::add(&db, writer.as_ref(), id, related)
        }

        IssueCommands::Unrelate { id, related } => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            let writer = get_writer(&crosslink_dir);
            commands::relate::remove(&db, writer.as_ref(), id, related)
        }

        IssueCommands::Related { id } => {
            let db = get_db()?;
            commands::relate::list(&db, id)
        }

        IssueCommands::Next => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            commands::next::run(&db, &crosslink_dir)
        }

        IssueCommands::Tree { status } => {
            let db = get_db()?;
            commands::tree::run(&db, Some(&status), json)
        }

        IssueCommands::Tested => {
            let crosslink_dir = find_crosslink_dir()?;
            commands::tested::run(&crosslink_dir)
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let log_format = match &cli.command {
        Commands::Serve { .. } if cli.log_format == "text" => "json",
        _ => cli.log_format.as_str(),
    };
    init_tracing(&cli.log_level, log_format);

    match cli.command {
        Commands::Init {
            force,
            python_prefix,
            skip_cpitd,
            skip_signing,
            signing_key,
            reconfigure,
            defaults,
        } => {
            let cwd = env::current_dir()?;
            let opts = commands::init::InitOpts {
                force,
                python_prefix: python_prefix.as_deref(),
                skip_cpitd,
                skip_signing,
                signing_key: signing_key.as_deref(),
                reconfigure,
                defaults,
            };
            commands::init::run(&cwd, &opts)
        }

        // === Canonical grouped commands ===
        Commands::Issue { action } => dispatch_issue(action, cli.quiet, cli.json),

        Commands::Timer { action } => {
            let db = get_db()?;
            match action {
                TimerCommands::Start { id } => commands::timer::start(&db, id),
                TimerCommands::Stop => commands::timer::stop(&db),
                TimerCommands::Show => commands::timer::status(&db),
            }
        }

        Commands::Migrate { action } => match action {
            MigrateCommands::ToShared => {
                let crosslink_dir = find_crosslink_dir()?;
                let db = get_db()?;
                commands::migrate::to_shared(&crosslink_dir, &db)
            }
            MigrateCommands::FromShared => {
                let crosslink_dir = find_crosslink_dir()?;
                let db = get_db()?;
                commands::migrate::from_shared(&crosslink_dir, &db)
            }
            MigrateCommands::RenameBranch => {
                let crosslink_dir = find_crosslink_dir()?;
                commands::migrate::rename_branch(&crosslink_dir)
            }
        },

        // === Hidden top-level shortcuts ===
        Commands::Create {
            title,
            description,
            priority,
            template,
            label,
            work,
            defer_id,
            parent,
        } => dispatch_issue(
            IssueCommands::Create {
                title,
                description,
                priority,
                template,
                label,
                work,
                defer_id,
                parent,
            },
            cli.quiet,
            cli.json,
        ),

        Commands::Quick {
            title,
            description,
            priority,
            template,
            label,
            parent,
        } => dispatch_issue(
            IssueCommands::Quick {
                title,
                description,
                priority,
                template,
                label,
                parent,
            },
            cli.quiet,
            cli.json,
        ),

        Commands::List {
            status,
            label,
            priority,
        } => dispatch_issue(
            IssueCommands::List {
                status,
                label,
                priority,
                repo: None,
                refresh: false,
            },
            cli.quiet,
            cli.json,
        ),

        Commands::Show { id } => dispatch_issue(
            IssueCommands::Show {
                id,
                repo: None,
                refresh: false,
            },
            cli.quiet,
            cli.json,
        ),

        Commands::Close { id, no_changelog } => dispatch_issue(
            IssueCommands::Close { id, no_changelog },
            cli.quiet,
            cli.json,
        ),

        // === Hidden aliases (emit hints) ===
        Commands::New {
            title,
            description,
            priority,
            template,
            label,
            work,
            parent,
        } => {
            hint(
                cli.quiet,
                "did you mean 'crosslink issue create'? Using that.",
            );
            dispatch_issue(
                IssueCommands::Create {
                    title,
                    description,
                    priority,
                    template,
                    label,
                    work,
                    defer_id: false,
                    parent,
                },
                cli.quiet,
                cli.json,
            )
        }

        Commands::Issues { action } => {
            if let Some(IssuesAliasCommands::List {
                status,
                label,
                priority,
            }) = action
            {
                hint(
                    cli.quiet,
                    "did you mean 'crosslink issue list'? Using that.",
                );
                dispatch_issue(
                    IssueCommands::List {
                        status,
                        label,
                        priority,
                        repo: None,
                        refresh: false,
                    },
                    cli.quiet,
                    cli.json,
                )
            } else {
                hint(
                    cli.quiet,
                    "did you mean 'crosslink issue list'? Using that.",
                );
                dispatch_issue(
                    IssueCommands::List {
                        status: "open".to_string(),
                        label: None,
                        priority: None,
                        repo: None,
                        refresh: false,
                    },
                    cli.quiet,
                    cli.json,
                )
            }
        }

        Commands::Subissue {
            parent,
            title,
            description,
            priority,
            label,
            work,
        } => {
            hint(
                cli.quiet,
                "did you mean 'crosslink issue create --parent'? Using that.",
            );
            dispatch_issue(
                IssueCommands::Create {
                    title,
                    description,
                    priority,
                    template: None,
                    label,
                    work,
                    defer_id: false,
                    parent: Some(parent),
                },
                cli.quiet,
                cli.json,
            )
        }

        Commands::TimerStart { id } => {
            hint(
                cli.quiet,
                "did you mean 'crosslink timer start'? Using that.",
            );
            let db = get_db()?;
            commands::timer::start(&db, id)
        }

        Commands::TimerStop => {
            hint(
                cli.quiet,
                "did you mean 'crosslink timer stop'? Using that.",
            );
            let db = get_db()?;
            commands::timer::stop(&db)
        }

        // === Hidden migration aliases ===
        Commands::MigrateToShared => {
            hint(
                cli.quiet,
                "did you mean 'crosslink migrate to-shared'? Using that.",
            );
            let crosslink_dir = find_crosslink_dir()?;
            let db = get_db()?;
            commands::migrate::to_shared(&crosslink_dir, &db)
        }

        Commands::MigrateFromShared => {
            hint(
                cli.quiet,
                "did you mean 'crosslink migrate from-shared'? Using that.",
            );
            let crosslink_dir = find_crosslink_dir()?;
            let db = get_db()?;
            commands::migrate::from_shared(&crosslink_dir, &db)
        }

        Commands::MigrateRenameBranch => {
            hint(
                cli.quiet,
                "did you mean 'crosslink migrate rename-branch'? Using that.",
            );
            let crosslink_dir = find_crosslink_dir()?;
            commands::migrate::rename_branch(&crosslink_dir)
        }

        // === Remaining top-level commands (unchanged) ===
        Commands::Export { output, format } => {
            let db = get_db()?;
            match format.as_str() {
                "json" => commands::export::run_json(&db, output.as_deref()),
                "markdown" | "md" => commands::export::run_markdown(&db, output.as_deref()),
                _ => {
                    bail!("Unknown format '{}'. Use 'json' or 'markdown'", format);
                }
            }
        }

        Commands::Import { input } => {
            let db = get_db()?;
            let path = std::path::Path::new(&input);
            commands::import::run_json(&db, path)
        }

        Commands::Archive { action } => {
            let db = get_db()?;
            commands::archive::run(action, &db)
        }

        Commands::Milestone { action } => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            commands::milestone::run(action, &db, &crosslink_dir)
        }

        Commands::Session { action } => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            commands::session::run(action, &db, &crosslink_dir, cli.json)
        }

        Commands::Daemon { action } => match action {
            DaemonCommands::Start => {
                let crosslink_dir = find_crosslink_dir()?;
                daemon::start(&crosslink_dir)
            }
            DaemonCommands::Stop => {
                let crosslink_dir = find_crosslink_dir()?;
                daemon::stop(&crosslink_dir)
            }
            DaemonCommands::Status => {
                let crosslink_dir = find_crosslink_dir()?;
                daemon::status(&crosslink_dir)
            }
            DaemonCommands::Run { dir } => daemon::run_daemon(&dir),
        },

        Commands::Cpitd { action } => {
            let db = get_db()?;
            commands::cpitd::run(action, &db, cli.quiet)
        }

        Commands::Agent { action } => {
            let crosslink_dir = find_crosslink_dir()?;
            commands::agent::run(action, &crosslink_dir)
        }

        Commands::Trust { action } => {
            let crosslink_dir = find_crosslink_dir()?;
            commands::trust::run(action, &crosslink_dir)
        }

        Commands::Locks { action } => {
            let crosslink_dir = find_crosslink_dir()?;
            let db = get_db()?;
            commands::locks_cmd::run(action, &crosslink_dir, &db, cli.json)
        }

        Commands::Heartbeat => {
            let crosslink_dir = find_crosslink_dir()?;
            let agent = crate::identity::AgentConfig::load(&crosslink_dir)?;
            match agent {
                Some(agent) => {
                    let sync = crate::sync::SyncManager::new(&crosslink_dir)?;
                    let _ = sync.init_cache();
                    let db = get_db()?;
                    let active_issue = db
                        .get_current_session_for_agent(None)?
                        .and_then(|s| s.active_issue_id);
                    sync.push_heartbeat(&agent, active_issue)?;
                    Ok(())
                }
                None => Ok(()),
            }
        }

        Commands::Sync => {
            let crosslink_dir = find_crosslink_dir()?;
            let db = get_db()?;
            commands::locks_cmd::sync_cmd(&crosslink_dir, &db)
        }

        Commands::Integrity { action } => {
            let crosslink_dir = find_crosslink_dir()?;
            let db = get_db()?;
            commands::integrity_cmd::run(action.as_ref(), &crosslink_dir, &db)
        }

        Commands::Prune {
            dry_run,
            force,
            keep_commits,
            hub_only,
            knowledge_only,
        } => {
            let crosslink_dir = find_crosslink_dir()?;
            let opts = commands::prune::PruneOpts {
                dry_run,
                force,
                keep_commits,
                hub_only,
                knowledge_only,
            };
            commands::prune::run(&crosslink_dir, &opts, cli.json)
        }

        Commands::Compact { force } => {
            let crosslink_dir = find_crosslink_dir()?;
            let db = get_db()?;
            commands::compact::run(&crosslink_dir, &db, force)
        }

        Commands::Container { action } => commands::container::run(action),

        Commands::Style { command } => {
            let crosslink_dir = find_crosslink_dir()?;
            commands::style::run(command, &crosslink_dir)
        }

        Commands::Knowledge { command } => {
            let crosslink_dir = find_crosslink_dir()?;
            commands::knowledge::dispatch(command, &crosslink_dir, cli.json)
        }

        Commands::Config { command } => {
            let crosslink_dir = find_crosslink_dir()?;
            commands::config::run(command, &crosslink_dir)
        }
        Commands::Context { command } => {
            let crosslink_dir = find_crosslink_dir()?;
            commands::context::run(command, &crosslink_dir)
        }
        Commands::Workflow { command } => {
            let crosslink_dir = find_crosslink_dir()?;
            commands::workflow::run(command, &crosslink_dir, get_db)
        }

        Commands::Kickoff { action } => {
            let crosslink_dir = find_crosslink_dir()?;
            let db = get_db()?;
            let writer = get_writer(&crosslink_dir);
            commands::kickoff::dispatch(
                action,
                &crosslink_dir,
                &db,
                writer.as_ref(),
                cli.quiet,
                cli.json,
            )
        }
        Commands::Swarm { action } => {
            let crosslink_dir = find_crosslink_dir()?;
            match action {
                SwarmCommands::Init { doc } => commands::swarm::init(&crosslink_dir, &doc),
                SwarmCommands::Status => commands::swarm::status(&crosslink_dir, cli.json),
                SwarmCommands::Resume => commands::swarm::resume(&crosslink_dir),
                SwarmCommands::SyncStatus => commands::swarm::sync_status(&crosslink_dir),
                SwarmCommands::Adopt { agent, slot } => {
                    commands::swarm::adopt(&crosslink_dir, &agent, &slot)
                }
                SwarmCommands::Archive => commands::swarm::archive(&crosslink_dir),
                SwarmCommands::Reset { no_archive } => {
                    commands::swarm::reset(&crosslink_dir, no_archive)
                }
                SwarmCommands::ListSwarms => commands::swarm::list_swarms(&crosslink_dir),
                SwarmCommands::Launch {
                    phase,
                    budget_aware,
                    retry_failed,
                } => {
                    let db = get_db()?;
                    let writer = get_writer(&crosslink_dir);
                    if retry_failed {
                        commands::swarm::launch_retry_failed(
                            &crosslink_dir,
                            &db,
                            writer.as_ref(),
                            &phase,
                            cli.quiet,
                        )
                    } else if budget_aware {
                        commands::swarm::launch_budget_aware(
                            &crosslink_dir,
                            &db,
                            writer.as_ref(),
                            &phase,
                            cli.quiet,
                        )
                    } else {
                        commands::swarm::launch(
                            &crosslink_dir,
                            &db,
                            writer.as_ref(),
                            &phase,
                            cli.quiet,
                        )
                    }
                }
                SwarmCommands::Gate { phase } => commands::swarm::gate(&crosslink_dir, &phase),
                SwarmCommands::Checkpoint {
                    phase,
                    notes,
                    force,
                } => commands::swarm::checkpoint(&crosslink_dir, &phase, notes.as_deref(), force),
                SwarmCommands::Config {
                    budget_window,
                    model,
                } => commands::swarm::config_budget(&crosslink_dir, &budget_window, &model),
                SwarmCommands::Estimate { phase } => {
                    commands::swarm::estimate(&crosslink_dir, &phase)
                }
                SwarmCommands::Harvest => commands::swarm::harvest_costs(&crosslink_dir),
                SwarmCommands::Plan { budget_window } => {
                    commands::swarm::plan(&crosslink_dir, budget_window.as_deref())
                }
                SwarmCommands::PlanShow => commands::swarm::plan_show(&crosslink_dir),
                SwarmCommands::Review {
                    agents,
                    mandate,
                    doc,
                    file_issues,
                    fix,
                } => commands::swarm::review(
                    &crosslink_dir,
                    agents,
                    &mandate,
                    doc.as_deref(),
                    file_issues,
                    fix,
                ),
                SwarmCommands::Fix {
                    issues,
                    from_label,
                    max_agents,
                    budget_aware,
                } => commands::swarm::fix(
                    &crosslink_dir,
                    issues.as_deref(),
                    from_label.as_deref(),
                    max_agents,
                    budget_aware,
                ),
                SwarmCommands::Merge {
                    branch,
                    dry_run,
                    agents,
                } => commands::swarm::merge(&crosslink_dir, &branch, dry_run, agents.as_deref()),
                SwarmCommands::MoveAgent { agent, to_phase } => {
                    commands::swarm::move_agent(&crosslink_dir, &agent, &to_phase)
                }
                SwarmCommands::MergePhases { phase_a, phase_b } => {
                    commands::swarm::merge_phases(&crosslink_dir, &phase_a, &phase_b)
                }
                SwarmCommands::SplitPhase { phase, after } => {
                    commands::swarm::split_phase(&crosslink_dir, &phase, &after)
                }
                SwarmCommands::RemoveAgent { agent } => {
                    commands::swarm::remove_agent(&crosslink_dir, &agent)
                }
                SwarmCommands::Reorder { phase, position } => {
                    commands::swarm::reorder_phase(&crosslink_dir, &phase, position)
                }
                SwarmCommands::RenamePhase { old, new } => {
                    commands::swarm::rename_phase(&crosslink_dir, &old, &new)
                }
                SwarmCommands::ReviewContinue => commands::swarm::review_continue(&crosslink_dir),
                SwarmCommands::ReviewStatus => commands::swarm::review_status(&crosslink_dir),
                SwarmCommands::Pipeline {
                    agents,
                    mandate,
                    target_branch,
                    auto_fix,
                    auto_file_issues,
                } => commands::swarm::run_pipeline_cmd(
                    &crosslink_dir,
                    agents,
                    &mandate,
                    &target_branch,
                    auto_fix,
                    auto_file_issues,
                ),
                SwarmCommands::TrustInit { model } => {
                    commands::swarm::trust_init(&crosslink_dir, &model)
                }
            }
        }
        Commands::Tui => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            commands::tui::run(&db, &crosslink_dir)
        }
        Commands::Mc { layout } => {
            let crosslink_dir = find_crosslink_dir()?;
            commands::mission_control::run(&crosslink_dir, &layout)
        }
        Commands::Serve {
            port,
            dashboard_dir,
        } => {
            let crosslink_dir = find_crosslink_dir()?;
            let db = get_db()?;
            tokio::runtime::Runtime::new()?.block_on(server::run(
                port,
                dashboard_dir,
                db,
                crosslink_dir,
            ))
        }
    }
}
