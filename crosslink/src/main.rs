mod commands;
mod daemon;
mod db;
mod hydration;
mod identity;
mod issue_file;
mod lock_check;
mod locks;
mod models;
mod shared_writer;
mod sync;
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
        id: i64,
    },

    /// Update an issue
    Update {
        /// Issue ID
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
        id: i64,
    },

    /// Delete an issue
    Delete {
        /// Issue ID
        id: i64,
        /// Skip confirmation
        #[arg(short, long)]
        force: bool,
    },

    /// Add a comment to an issue
    Comment {
        /// Issue ID
        id: i64,
        /// Comment text
        text: String,
    },

    /// Add a label to an issue
    Label {
        /// Issue ID
        id: i64,
        /// Label name
        label: String,
    },

    /// Remove a label from an issue
    Unlabel {
        /// Issue ID
        id: i64,
        /// Label name
        label: String,
    },

    /// Mark an issue as blocked by another
    Block {
        /// Issue ID that is blocked
        id: i64,
        /// Issue ID that is blocking
        blocker: i64,
    },

    /// Remove a blocking relationship
    Unblock {
        /// Issue ID that was blocked
        id: i64,
        /// Issue ID that was blocking
        blocker: i64,
    },

    /// List blocked issues
    Blocked,

    /// List issues ready to work on (no open blockers)
    Ready,

    /// Link two related issues
    Relate {
        /// First issue ID
        id: i64,
        /// Second issue ID
        related: i64,
    },

    /// Remove a relation between issues
    Unrelate {
        /// First issue ID
        id: i64,
        /// Second issue ID
        related: i64,
    },

    /// List related issues
    Related {
        /// Issue ID
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

    /// Review crosslink policy configuration
    Review {
        #[command(subcommand)]
        command: ReviewCommands,
    },
}

#[derive(Subcommand)]
enum ArchiveCommands {
    /// Archive a closed issue
    Add {
        /// Issue ID
        id: i64,
    },
    /// Unarchive an issue (restore to closed)
    Remove {
        /// Issue ID
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
    },
    /// Show current agent identity
    Status,
}

#[derive(Subcommand)]
enum LocksCommands {
    /// List all active locks
    List,
    /// Check if a specific issue is locked
    Check {
        /// Issue ID
        id: i64,
    },
    /// Claim a lock on an issue
    Claim {
        /// Issue ID
        id: i64,
        /// Branch name for context
        #[arg(short, long)]
        branch: Option<String>,
    },
    /// Release a lock on an issue
    Release {
        /// Issue ID
        id: i64,
    },
    /// Steal a stale lock from another agent
    Steal {
        /// Issue ID
        id: i64,
    },
}

#[derive(Subcommand)]
enum ReviewCommands {
    /// Compare deployed policy files against embedded defaults
    Diff {
        /// Filter by section: tracking, rules, languages, hooks
        #[arg(short, long)]
        section: Option<String>,
    },
}

fn find_crosslink_dir() -> Result<PathBuf> {
    let mut current = env::current_dir()?;

    loop {
        let candidate = current.join(".crosslink");
        if candidate.is_dir() {
            return Ok(candidate);
        }

        if !current.pop() {
            bail!("Not a crosslink repository (or any parent). Run 'crosslink init' first.");
        }
    }
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

/// Parse an issue ID string, supporting both regular IDs and offline local IDs.
///
/// - `"42"` → `42` (regular display ID)
/// - `"L1"` or `"l1"` → `-1` (offline local ID, stored as negative in SQLite)
///
/// Used when offline issue creation is enabled (display_id: null in JSON).
#[allow(dead_code)]
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
        Commands::Init { force } => {
            let cwd = env::current_dir()?;
            commands::init::run(&cwd, force)
        }

        Commands::Create {
            title,
            description,
            priority,
            template,
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

        Commands::Comment { id, text } => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            let writer = get_writer(&crosslink_dir);
            commands::comment::run(&db, writer.as_ref(), id, &text)
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
            match action {
                ArchiveCommands::Add { id } => commands::archive::archive(&db, id),
                ArchiveCommands::Remove { id } => commands::archive::unarchive(&db, id),
                ArchiveCommands::List => commands::archive::list(&db),
                ArchiveCommands::Older { days } => commands::archive::archive_older(&db, days),
            }
        }

        Commands::Milestone { action } => {
            let db = get_db()?;
            match action {
                MilestoneCommands::Create { name, description } => {
                    commands::milestone::create(&db, &name, description.as_deref())
                }
                MilestoneCommands::List { status } => commands::milestone::list(&db, Some(&status)),
                MilestoneCommands::Show { id } => commands::milestone::show(&db, id),
                MilestoneCommands::Add { id, issues } => commands::milestone::add(&db, id, &issues),
                MilestoneCommands::Remove { id, issue } => {
                    commands::milestone::remove(&db, id, issue)
                }
                MilestoneCommands::Close { id } => commands::milestone::close(&db, id),
                MilestoneCommands::Delete { id } => commands::milestone::delete(&db, id),
            }
        }

        Commands::Session { action } => {
            let db = get_db()?;
            let crosslink_dir = find_crosslink_dir()?;
            match action {
                SessionCommands::Start => commands::session::start(&db, &crosslink_dir),
                SessionCommands::End { notes } => {
                    commands::session::end(&db, notes.as_deref(), &crosslink_dir)
                }
                SessionCommands::Status => commands::session::status(&db),
                SessionCommands::Work { id } => commands::session::work(&db, id, &crosslink_dir),
                SessionCommands::LastHandoff => commands::session::last_handoff(&db),
                SessionCommands::Action { text } => commands::session::action(&db, &text),
            }
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
            match action {
                CpitdCommands::Scan {
                    paths,
                    min_tokens,
                    ignore,
                    dry_run,
                } => commands::cpitd::scan(&db, &paths, min_tokens, &ignore, dry_run, cli.quiet),
                CpitdCommands::Status => commands::cpitd::status(&db),
                CpitdCommands::Clear => commands::cpitd::clear(&db),
            }
        }

        Commands::Agent { action } => {
            let crosslink_dir = find_crosslink_dir()?;
            match action {
                AgentCommands::Init {
                    agent_id,
                    description,
                } => commands::agent::init(&crosslink_dir, &agent_id, description.as_deref()),
                AgentCommands::Status => commands::agent::status(&crosslink_dir),
            }
        }

        Commands::Locks { action } => {
            let crosslink_dir = find_crosslink_dir()?;
            let db = get_db()?;
            match action {
                LocksCommands::List => commands::locks_cmd::list(&crosslink_dir, &db, cli.json),
                LocksCommands::Check { id } => commands::locks_cmd::check(&crosslink_dir, id),
                LocksCommands::Claim { id, branch } => {
                    commands::locks_cmd::claim(&crosslink_dir, id, branch.as_deref())
                }
                LocksCommands::Release { id } => commands::locks_cmd::release(&crosslink_dir, id),
                LocksCommands::Steal { id } => commands::locks_cmd::steal(&crosslink_dir, id),
            }
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
        Commands::Review { command } => {
            let crosslink_dir = find_crosslink_dir()?;
            let claude_dir = crosslink_dir
                .parent()
                .ok_or_else(|| anyhow::anyhow!("Cannot determine project root"))?
                .join(".claude");
            match command {
                ReviewCommands::Diff { section } => {
                    commands::review::diff(&crosslink_dir, &claude_dir, section.as_deref())
                }
            }
        }
    }
}
