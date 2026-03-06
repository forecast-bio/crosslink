#[allow(dead_code)]
mod checkpoint;
mod clock_skew;
mod commands;
#[allow(dead_code)]
mod compaction;
mod daemon;
mod db;
#[allow(dead_code)]
mod events;
mod hydration;
mod identity;
mod issue_file;
mod knowledge;
mod lock_check;
mod locks;
mod models;
#[allow(dead_code)]
mod shared_writer;
mod signing;
mod sync;
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
#[command(version)]
struct Cli {
    /// Quiet mode: only output essential data (IDs, counts)
    #[arg(short, long, global = true)]
    quiet: bool,

    /// Output as JSON (supported by list, show, search, session status)
    #[arg(long, global = true)]
    json: bool,

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
    },

    /// Create a subissue under a parent issue
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
    },

    /// Search issues by text
    Search {
        /// Search query
        query: String,
    },

    /// Show issue details
    Show {
        /// Issue ID
        #[arg(value_parser = parse_issue_id_clap)]
        id: i64,
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

    /// Start a timer for an issue
    Start {
        /// Issue ID
        #[arg(value_parser = parse_issue_id_clap)]
        id: i64,
    },

    /// Stop the current timer
    Stop,

    /// Show current timer status
    Timer,

    /// Mark tests as run (resets test reminder)
    Tested,

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

    /// Sync locks and issue state from remote
    Sync,

    /// Migrate local SQLite issues to shared coordination branch
    MigrateToShared,

    /// Import shared issues from coordination branch into local SQLite
    MigrateFromShared,

    /// Rename coordination branch from crosslink/locks to crosslink/hub
    MigrateRenameBranch,

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
    /// Manage container-based agent execution
    Container {
        #[command(subcommand)]
        action: ContainerCommands,
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
        #[arg(long)]
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
        #[arg(long)]
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
    },
    /// Display a knowledge page
    Show {
        /// Page slug
        slug: String,
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
    },
    /// Update an existing knowledge page
    Edit {
        /// Page slug
        slug: String,
        /// Append content to the page
        #[arg(long)]
        append: Option<String>,
        /// Replace page content entirely
        #[arg(long)]
        content: Option<String>,
        /// Add tags (repeatable)
        #[arg(long)]
        tag: Vec<String>,
        /// Add source URL (repeatable)
        #[arg(long)]
        source: Vec<String>,
    },
    /// Remove a knowledge page
    Remove {
        /// Page slug
        slug: String,
    },
    /// Manually sync from remote
    Sync,
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
        #[arg(long)]
        dry_run: bool,
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
        #[arg(long)]
        dry_run: bool,
        /// Branch to use (auto-creates feature branch if omitted)
        #[arg(long)]
        branch: Option<String>,
        /// Path to a design document (markdown) with structured requirements
        #[arg(long, value_name = "PATH")]
        doc: Option<PathBuf>,
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
        #[arg(long)]
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
    /// Show current swarm status (agents, phases, progress)
    Status,
    /// Reconstruct state and show next steps for resuming
    Resume,
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
            eprintln!("Warning: SharedWriter unavailable: {}", e);
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

fn main() -> Result<()> {
    let cli = Cli::parse();

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

        Commands::Create {
            title,
            description,
            priority,
            template,
            label,
            work,
            defer_id,
        } => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            let writer = get_writer(&crosslink_dir);
            let opts = commands::create::CreateOpts {
                labels: &label,
                work,
                quiet: cli.quiet,
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

        Commands::Quick {
            title,
            description,
            priority,
            template,
            label,
        } => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            let writer = get_writer(&crosslink_dir);
            let opts = commands::create::CreateOpts {
                labels: &label,
                work: true,
                quiet: cli.quiet,
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

        Commands::Subissue {
            parent,
            title,
            description,
            priority,
            label,
            work,
        } => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            let writer = get_writer(&crosslink_dir);
            let opts = commands::create::CreateOpts {
                labels: &label,
                work,
                quiet: cli.quiet,
                crosslink_dir: Some(&crosslink_dir),
                defer_id: false,
            };
            commands::create::run_subissue(
                &db,
                writer.as_ref(),
                parent,
                &title,
                description.as_deref(),
                &priority,
                &opts,
            )
        }

        Commands::List {
            status,
            label,
            priority,
        } => {
            let db = get_db()?;
            if cli.json {
                commands::list::run_json(&db, Some(&status), label.as_deref(), priority.as_deref())
            } else {
                commands::list::run(&db, Some(&status), label.as_deref(), priority.as_deref())
            }
        }

        Commands::Search { query } => {
            let db = get_db()?;
            if cli.json {
                commands::search::run_json(&db, &query)
            } else {
                commands::search::run(&db, &query)
            }
        }

        Commands::Show { id } => {
            let db = get_db()?;
            if cli.json {
                commands::show::run_json(&db, id)
            } else {
                commands::show::run(&db, id)
            }
        }

        Commands::Update {
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

        Commands::Close { id, no_changelog } => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            let writer = get_writer(&crosslink_dir);
            if cli.quiet {
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

        Commands::CloseAll {
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

        Commands::Reopen { id } => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            let writer = get_writer(&crosslink_dir);
            commands::status::reopen(&db, writer.as_ref(), id)
        }

        Commands::Delete { id, force } => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            let writer = get_writer(&crosslink_dir);
            commands::delete::run(&db, writer.as_ref(), id, force)
        }

        Commands::Comment { id, text, kind } => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            let writer = get_writer(&crosslink_dir);
            commands::comment::run(&db, writer.as_ref(), id, &text, &kind)
        }

        Commands::Intervene {
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

        Commands::Label { id, label } => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            let writer = get_writer(&crosslink_dir);
            commands::label::add(&db, writer.as_ref(), id, &label)
        }

        Commands::Unlabel { id, label } => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            let writer = get_writer(&crosslink_dir);
            commands::label::remove(&db, writer.as_ref(), id, &label)
        }

        Commands::Block { id, blocker } => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            let writer = get_writer(&crosslink_dir);
            commands::deps::block(&db, writer.as_ref(), id, blocker)
        }

        Commands::Unblock { id, blocker } => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            let writer = get_writer(&crosslink_dir);
            commands::deps::unblock(&db, writer.as_ref(), id, blocker)
        }

        Commands::Blocked => {
            let db = get_db()?;
            commands::deps::list_blocked(&db)
        }

        Commands::Ready => {
            let db = get_db()?;
            commands::deps::list_ready(&db)
        }

        Commands::Relate { id, related } => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            let writer = get_writer(&crosslink_dir);
            commands::relate::add(&db, writer.as_ref(), id, related)
        }

        Commands::Unrelate { id, related } => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            let writer = get_writer(&crosslink_dir);
            commands::relate::remove(&db, writer.as_ref(), id, related)
        }

        Commands::Related { id } => {
            let db = get_db()?;
            commands::relate::list(&db, id)
        }

        Commands::Next => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            commands::next::run(&db, &crosslink_dir)
        }

        Commands::Tree { status } => {
            let db = get_db()?;
            commands::tree::run(&db, Some(&status))
        }

        Commands::Start { id } => {
            let db = get_db()?;
            commands::timer::start(&db, id)
        }

        Commands::Stop => {
            let db = get_db()?;
            commands::timer::stop(&db)
        }

        Commands::Timer => {
            let db = get_db()?;
            commands::timer::status(&db)
        }

        Commands::Tested => {
            let crosslink_dir = find_crosslink_dir()?;
            commands::tested::run(&crosslink_dir)
        }

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
            commands::session::run(action, &db, &crosslink_dir)
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

        Commands::Sync => {
            let crosslink_dir = find_crosslink_dir()?;
            let db = get_db()?;
            commands::locks_cmd::sync_cmd(&crosslink_dir, &db)
        }

        Commands::MigrateToShared => {
            let crosslink_dir = find_crosslink_dir()?;
            let db = get_db()?;
            commands::migrate::to_shared(&crosslink_dir, &db)
        }

        Commands::MigrateFromShared => {
            let crosslink_dir = find_crosslink_dir()?;
            let db = get_db()?;
            commands::migrate::from_shared(&crosslink_dir, &db)
        }
        Commands::MigrateRenameBranch => {
            let crosslink_dir = find_crosslink_dir()?;
            commands::migrate::rename_branch(&crosslink_dir)
        }
        Commands::Integrity { action } => {
            let crosslink_dir = find_crosslink_dir()?;
            let db = get_db()?;
            commands::integrity_cmd::run(action.as_ref(), &crosslink_dir, &db)
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
            commands::kickoff::dispatch(action, &crosslink_dir, &db, writer.as_ref(), cli.quiet)
        }
        Commands::Swarm { action } => {
            let crosslink_dir = find_crosslink_dir()?;
            match action {
                SwarmCommands::Init { doc } => commands::swarm::init(&crosslink_dir, &doc),
                SwarmCommands::Status => commands::swarm::status(&crosslink_dir),
                SwarmCommands::Resume => commands::swarm::resume(&crosslink_dir),
            }
        }
        Commands::Tui => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            commands::tui::run(&db, &crosslink_dir)
        }
    }
}
