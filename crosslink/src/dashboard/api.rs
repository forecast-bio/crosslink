//! HTTP API for the dashboard aggregator (GH #429 §7).
//!
//! Routes live under `/api/v1/dashboard/` so they don't collide with the
//! existing single-project API (`/api/v1/issues`, `/api/v1/agents`, etc.)
//! that `crosslink serve` / the deprecation path continues to expose.
//!
//! Each handler opens a fresh [`DashboardDb`] connection from the path
//! recorded in [`crate::server::state::AppState::dashboard_db_path`].
//! `SQLite` opens are cheap (~microseconds) so per-request opens are
//! fine for the polling-panel use case. If that ever becomes a hot
//! path we can pool connections; not worth the complexity now.
//!
//! This module exposes a pure-axum `Router` factory — the server's
//! `build_router` nests it under `/api/v1/dashboard`.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use super::actions;
use super::db::DashboardDb;
use super::projects::Project;
use super::reader;
use crate::server::state::AppState;

/// Build the `/api/v1/dashboard` router.
///
/// Note: the project-detail route uses `{*slug}` (wildcard capture)
/// because crosslink slugs have the shape `owner/repo` and therefore
/// contain a slash. A single-segment capture (`{slug}`) would fail to
/// match. Clients can hit `/projects/forecast-bio/crosslink` directly;
/// no URL-encoding required.
pub fn build_router() -> Router<AppState> {
    // Write endpoints split by slug *and* issue id. Axum picks the
    // most-specific route that matches, so `/issues/{id}/close` wins
    // over `/projects/{*slug}` as long as we nest the writes beneath
    // a distinct prefix. Using `/w/` (for "write") keeps the path
    // pattern unambiguous without making URLs ugly.
    Router::new()
        .route("/projects", get(list_projects))
        .route("/projects/{*slug}", get(get_project_detail))
        .route("/alerts", get(list_alerts))
        .route("/clone", post(clone_repo))
        .route("/w/{owner}/{repo}/issues/{id}/close", post(close_issue))
        .route("/w/{owner}/{repo}/issues/{id}/reopen", post(reopen_issue))
        .route("/w/{owner}/{repo}/issues/{id}/comment", post(comment_issue))
        .route("/w/{owner}/{repo}/issues/{id}/block", post(block_issue))
        .route("/w/{owner}/{repo}/issues/{id}/unblock", post(unblock_issue))
        .route("/w/{owner}/{repo}/issues/{id}/relate", post(relate_issue))
        .route("/w/{owner}/{repo}/issues/{id}/label", post(label_issue))
        .route("/w/{owner}/{repo}/issues/{id}/unlabel", post(unlabel_issue))
        .route("/w/{owner}/{repo}/milestones", post(create_milestone))
        .route(
            "/w/{owner}/{repo}/milestones/{id}/add",
            post(milestone_add_issue),
        )
        .route(
            "/w/{owner}/{repo}/milestones/{id}/remove",
            post(milestone_remove_issue),
        )
        .route(
            "/w/{owner}/{repo}/milestones/{id}/close",
            post(close_milestone),
        )
        .route("/w/{owner}/{repo}/locks/{id}/claim", post(claim_lock))
        .route("/w/{owner}/{repo}/locks/{id}/release", post(release_lock))
        .route("/w/{owner}/{repo}/locks/{id}/steal", post(steal_lock))
        .route(
            "/w/{owner}/{repo}/agents/{agent_id}/request",
            post(agent_request),
        )
        .route("/w/{owner}/{repo}/init", post(init_project))
        .route(
            "/w/{owner}/{repo}/integrity/sign-backfill",
            post(sign_backfill),
        )
}

/// Wire-format representation of a tracked project on the list endpoint.
/// Extends [`Project`] with the current `project_state` counters so the
/// frontend can render a tile without a second round-trip.
#[derive(Debug, Serialize)]
struct ProjectListItem {
    slug: String,
    status: String,
    pinned: bool,
    hub_sha: Option<String>,
    hub_fetched_at: Option<String>,
    last_activity_at: Option<String>,
    added_at: String,
    counters: ProjectCountersView,
    /// Whether the local workspace is initialised enough for dashboard
    /// write actions to succeed. See [`super::projects::WriteCapability`].
    write_capability: &'static str,
}

#[derive(Debug, Default, Serialize)]
struct ProjectCountersView {
    open_issues: i64,
    overdue_issues: i64,
    due_soon_issues: i64,
    blocked_issues: i64,
    active_agents: i64,
    stale_locks: i64,
    ci_status: Option<String>,
    updated_at: Option<String>,
}

/// Full detail payload for `/projects/{slug}`. Reads a live
/// [`reader::HubSnapshot`] off the cached clone so the frontend
/// gets the complete issue/agent/lock set — not just the aggregate
/// counters.
#[derive(Debug, Serialize)]
struct ProjectDetail {
    slug: String,
    status: String,
    pinned: bool,
    hub_sha: Option<String>,
    hub_fetched_at: Option<String>,
    last_activity_at: Option<String>,
    added_at: String,
    counters: ProjectCountersView,
    /// Full issue list from the hub branch.
    issues: Vec<crate::issue_file::IssueFile>,
    /// Per-agent heartbeats.
    agents: Vec<crate::locks::Heartbeat>,
    /// Lock entries (`issue_id` keyed).
    locks: Vec<SerializableLock>,
    /// Hub layout version (1 or 2).
    layout_version: u32,
    /// Agent control requests grouped by target agent.
    agent_requests: Vec<SerializableAgentRequests>,
    /// CI status for the hub-tip commit, when a pipeline reports it.
    /// Shape: `{sha, state: "passing|failing|pending", url?}` from
    /// `meta/ci-status.json` on the hub branch.
    ci_status: Option<reader::CiStatus>,
    /// Coarse signature state of the hub-tip commit. One of `"valid"`,
    /// `"unsigned"`, `"invalid"`, or `"unknown"`.
    signature_state: &'static str,
    /// Whether the local workspace is initialised enough for dashboard
    /// write actions to succeed. Same shape as on the list endpoint.
    write_capability: &'static str,
}

/// Flattened view of a target agent's request stream for JSON output.
#[derive(Debug, Serialize)]
struct SerializableAgentRequests {
    agent_id: String,
    requests: Vec<SerializableAgentRequest>,
}

#[derive(Debug, Serialize)]
struct SerializableAgentRequest {
    request_id: String,
    kind: String,
    subject_issue: Option<i64>,
    requested_by: String,
    requested_at: String,
    reason: Option<String>,
    /// `None` when pending; set once the target agent acknowledges.
    ack: Option<SerializableAgentRequestAck>,
}

#[derive(Debug, Serialize)]
struct SerializableAgentRequestAck {
    ack_at: String,
    acted: bool,
    result: String,
    notes: Option<String>,
}

impl From<reader::AgentRequestsForAgent> for SerializableAgentRequests {
    fn from(group: reader::AgentRequestsForAgent) -> Self {
        Self {
            agent_id: group.agent_id,
            requests: group
                .requests
                .into_iter()
                .map(|r| SerializableAgentRequest {
                    request_id: r.request.request_id,
                    kind: format!("{:?}", r.request.kind).to_lowercase(),
                    subject_issue: r.request.subject.issue_id,
                    requested_by: r.request.requested_by,
                    requested_at: r.request.requested_at,
                    reason: r.request.reason,
                    ack: r.ack.map(|a| SerializableAgentRequestAck {
                        ack_at: a.ack_at,
                        acted: a.acted,
                        result: a.result,
                        notes: a.notes,
                    }),
                })
                .collect(),
        }
    }
}

/// Flat lock representation for JSON output. The reader's `LockRecord`
/// holds a [`crate::locks::Lock`] inline; mirror its fields here so the
/// wire shape is a single-level object.
#[derive(Debug, Serialize)]
struct SerializableLock {
    issue_id: i64,
    agent_id: String,
    branch: Option<String>,
    claimed_at: chrono::DateTime<chrono::Utc>,
    signed_by: String,
}

impl From<reader::LockRecord> for SerializableLock {
    fn from(record: reader::LockRecord) -> Self {
        Self {
            issue_id: record.issue_id,
            agent_id: record.lock.agent_id,
            branch: record.lock.branch,
            claimed_at: record.lock.claimed_at,
            signed_by: record.lock.signed_by,
        }
    }
}

/// `GET /api/v1/dashboard/projects` — list tracked projects with
/// materialised tile state.
async fn list_projects(
    State(state): State<AppState>,
) -> Result<Json<Vec<ProjectListItem>>, ApiError> {
    let db_path = state
        .dashboard_db_path
        .as_ref()
        .ok_or_else(|| ApiError::bad_request("dashboard DB not configured for this server"))?
        .clone();

    let items = tokio::task::spawn_blocking(move || load_project_list(&db_path))
        .await
        .map_err(|e| ApiError::internal(format!("list task panicked: {e}")))??;

    Ok(Json(items))
}

fn load_project_list(db_path: &std::path::Path) -> Result<Vec<ProjectListItem>, ApiError> {
    let db = DashboardDb::open(db_path).map_err(|e| ApiError::internal(format!("open db: {e}")))?;
    let mut stmt = db
        .conn
        .prepare(
            "SELECT p.slug, p.status, p.pinned, p.hub_sha, p.hub_fetched_at,
                    p.last_activity_at, p.added_at, p.clone_path,
                    COALESCE(s.open_issues, 0),
                    COALESCE(s.overdue_issues, 0),
                    COALESCE(s.due_soon_issues, 0),
                    COALESCE(s.blocked_issues, 0),
                    COALESCE(s.active_agents, 0),
                    COALESCE(s.stale_locks, 0),
                    s.ci_status,
                    s.updated_at
             FROM projects p
             LEFT JOIN project_state s ON s.project_id = p.id
             ORDER BY p.pinned DESC, p.slug ASC",
        )
        .map_err(|e| ApiError::internal(format!("prepare: {e}")))?;

    let rows = stmt
        .query_map([], |row| {
            let clone_path: String = row.get(7)?;
            let write_capability =
                super::projects::write_capability(std::path::Path::new(&clone_path)).as_str();
            Ok(ProjectListItem {
                slug: row.get(0)?,
                status: row.get(1)?,
                pinned: row.get::<_, i64>(2)? != 0,
                hub_sha: row.get(3)?,
                hub_fetched_at: row.get(4)?,
                last_activity_at: row.get(5)?,
                added_at: row.get(6)?,
                counters: ProjectCountersView {
                    open_issues: row.get(8)?,
                    overdue_issues: row.get(9)?,
                    due_soon_issues: row.get(10)?,
                    blocked_issues: row.get(11)?,
                    active_agents: row.get(12)?,
                    stale_locks: row.get(13)?,
                    ci_status: row.get(14)?,
                    updated_at: row.get(15)?,
                },
                write_capability,
            })
        })
        .map_err(|e| ApiError::internal(format!("query: {e}")))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| ApiError::internal(format!("collect: {e}")))?;
    Ok(rows)
}

/// `GET /api/v1/dashboard/projects/{slug}` — full detail including a
/// freshly-read hub snapshot (issues, agents, locks).
async fn get_project_detail(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Json<ProjectDetail>, ApiError> {
    let db_path = state
        .dashboard_db_path
        .as_ref()
        .ok_or_else(|| ApiError::bad_request("dashboard DB not configured for this server"))?
        .clone();

    let detail = tokio::task::spawn_blocking(move || load_project_detail(&db_path, &slug))
        .await
        .map_err(|e| ApiError::internal(format!("detail task panicked: {e}")))??;
    Ok(Json(detail))
}

fn load_project_detail(db_path: &std::path::Path, slug: &str) -> Result<ProjectDetail, ApiError> {
    let db = DashboardDb::open(db_path).map_err(|e| ApiError::internal(format!("open db: {e}")))?;

    // Fetch the row + state in one query. Returns None if the slug
    // isn't tracked — which becomes a 404.
    let row: Option<(Project, ProjectCountersView)> = db
        .conn
        .query_row(
            "SELECT p.id, p.slug, p.clone_path, p.default_branch, p.hub_sha,
                    p.hub_fetched_at, p.status, p.added_at, p.last_activity_at,
                    p.pinned,
                    COALESCE(s.open_issues, 0), COALESCE(s.overdue_issues, 0),
                    COALESCE(s.due_soon_issues, 0), COALESCE(s.blocked_issues, 0),
                    COALESCE(s.active_agents, 0), COALESCE(s.stale_locks, 0),
                    s.ci_status, s.updated_at
             FROM projects p
             LEFT JOIN project_state s ON s.project_id = p.id
             WHERE p.slug = ?1",
            [slug],
            |r| {
                let project = Project {
                    id: r.get(0)?,
                    slug: r.get(1)?,
                    clone_path: std::path::PathBuf::from(r.get::<_, String>(2)?),
                    default_branch: r.get(3)?,
                    hub_sha: r.get(4)?,
                    hub_fetched_at: r.get(5)?,
                    status: r.get(6)?,
                    added_at: r.get(7)?,
                    last_activity_at: r.get(8)?,
                    pinned: r.get::<_, i64>(9)? != 0,
                };
                let counters = ProjectCountersView {
                    open_issues: r.get(10)?,
                    overdue_issues: r.get(11)?,
                    due_soon_issues: r.get(12)?,
                    blocked_issues: r.get(13)?,
                    active_agents: r.get(14)?,
                    stale_locks: r.get(15)?,
                    ci_status: r.get(16)?,
                    updated_at: r.get(17)?,
                };
                Ok((project, counters))
            },
        )
        .ok();

    let Some((project, counters)) = row else {
        return Err(ApiError::not_found(format!("project '{slug}' not tracked")));
    };

    // Read a fresh snapshot off the cache clone. If the clone is
    // missing (e.g. disk was wiped since `track`), return an empty
    // snapshot rather than erroring — the tile should still render.
    let snapshot = if project.clone_path.is_dir() {
        reader::read_snapshot(&project.clone_path).unwrap_or_else(|_| reader::HubSnapshot {
            hub_sha: None,
            layout_version: 1,
            issues: vec![],
            agents: vec![],
            locks: vec![],
            agent_requests: vec![],
            ci_status: None,
            signature_state: reader::SignatureState::Unknown,
            last_commit_at: None,
        })
    } else {
        reader::HubSnapshot {
            hub_sha: None,
            layout_version: 1,
            issues: vec![],
            agents: vec![],
            locks: vec![],
            agent_requests: vec![],
            ci_status: None,
            signature_state: reader::SignatureState::Unknown,
            last_commit_at: None,
        }
    };

    // Sort issues deterministically: display_id ascending, then
    // closed-after-open so the currently-actionable work floats up.
    // Local-only issues (no display_id yet) land at the end in uuid
    // order so they don't jump around between ticks.
    let mut issues = snapshot.issues;
    issues.sort_by(|a, b| {
        use std::cmp::Ordering;
        let by_status = match (a.status, b.status) {
            (s1, s2) if s1 == s2 => Ordering::Equal,
            (crate::models::IssueStatus::Open, _) => Ordering::Less,
            (_, crate::models::IssueStatus::Open) => Ordering::Greater,
            _ => Ordering::Equal,
        };
        if by_status != Ordering::Equal {
            return by_status;
        }
        match (a.display_id, b.display_id) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => a.uuid.cmp(&b.uuid),
        }
    });

    let write_capability = super::projects::write_capability(&project.clone_path).as_str();

    Ok(ProjectDetail {
        slug: project.slug,
        status: project.status,
        pinned: project.pinned,
        hub_sha: project.hub_sha,
        hub_fetched_at: project.hub_fetched_at,
        last_activity_at: project.last_activity_at,
        added_at: project.added_at,
        counters,
        layout_version: snapshot.layout_version,
        issues,
        agents: snapshot.agents,
        locks: snapshot.locks.into_iter().map(Into::into).collect(),
        agent_requests: snapshot
            .agent_requests
            .into_iter()
            .map(Into::into)
            .collect(),
        ci_status: snapshot.ci_status,
        signature_state: snapshot.signature_state.as_str(),
        write_capability,
    })
}

/// Wire-format alert row returned by `GET /api/v1/dashboard/alerts`.
/// Mirrors the `alerts` table plus the `slug` of the parent project
/// so the frontend can link-off without a second fetch.
#[derive(Debug, Serialize)]
struct AlertItem {
    id: i64,
    project_slug: String,
    kind: String,
    severity: String,
    subject_ref: Option<String>,
    detail: Option<String>,
    opened_at: String,
    resolved_at: Option<String>,
    acknowledged_at: Option<String>,
}

/// `GET /api/v1/dashboard/alerts` — list currently-open alerts across
/// all tracked projects, most recent first. No filtering for MVP;
/// add `?severity=...` / `?project=...` query params in P3 polish.
async fn list_alerts(State(state): State<AppState>) -> Result<Json<Vec<AlertItem>>, ApiError> {
    let db_path = state
        .dashboard_db_path
        .as_ref()
        .ok_or_else(|| ApiError::bad_request("dashboard DB not configured for this server"))?
        .clone();

    let items = tokio::task::spawn_blocking(move || load_open_alerts(&db_path))
        .await
        .map_err(|e| ApiError::internal(format!("alerts task panicked: {e}")))??;
    Ok(Json(items))
}

fn load_open_alerts(db_path: &std::path::Path) -> Result<Vec<AlertItem>, ApiError> {
    let db = DashboardDb::open(db_path).map_err(|e| ApiError::internal(format!("open db: {e}")))?;
    let mut stmt = db
        .conn
        .prepare(
            "SELECT a.id, p.slug, a.kind, a.severity, a.subject_ref,
                    a.detail, a.opened_at, a.resolved_at, a.acknowledged_at
             FROM alerts a
             JOIN projects p ON p.id = a.project_id
             WHERE a.resolved_at IS NULL
             ORDER BY a.opened_at DESC",
        )
        .map_err(|e| ApiError::internal(format!("prepare: {e}")))?;

    let rows = stmt
        .query_map([], |row| {
            Ok(AlertItem {
                id: row.get(0)?,
                project_slug: row.get(1)?,
                kind: row.get(2)?,
                severity: row.get(3)?,
                subject_ref: row.get(4)?,
                detail: row.get(5)?,
                opened_at: row.get(6)?,
                resolved_at: row.get(7)?,
                acknowledged_at: row.get(8)?,
            })
        })
        .map_err(|e| ApiError::internal(format!("query: {e}")))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| ApiError::internal(format!("collect: {e}")))?;
    Ok(rows)
}

/// Wire-format response for a completed write action.
#[derive(Debug, Serialize)]
struct ActionResponse {
    stdout: String,
    stderr: String,
}

/// Body for `POST /w/{owner}/{repo}/issues/{id}/comment`.
#[derive(Debug, Deserialize)]
struct CommentBody {
    content: String,
}

/// Body for `POST /w/{owner}/{repo}/init`.
#[derive(Debug, Deserialize)]
struct InitProjectBody {
    /// Agent identifier for `crosslink agent init`. Alphanumeric +
    /// hyphens + underscores.
    agent_id: String,
}

/// Body for `POST /api/v1/dashboard/clone`.
#[derive(Debug, Deserialize)]
struct CloneRepoBody {
    /// Git URL. Accepts `https://...`, `git@host:owner/repo.git`, etc.
    url: String,
    /// Override slug. Defaults to derived from `url`.
    #[serde(default)]
    slug: Option<String>,
    /// Clone-root override. Defaults to `$HOME`, so repos land at
    /// `~/<owner>/<repo>`.
    #[serde(default)]
    clone_root: Option<String>,
    /// Run `crosslink init --defaults` + `crosslink agent init` after
    /// cloning so dashboard writes work right away.
    #[serde(default)]
    init: bool,
    /// Required when `init` is true.
    #[serde(default)]
    agent_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct CloneRepoOutcome {
    slug: String,
    clone_path: String,
    initialized: bool,
}

/// `POST /api/v1/dashboard/clone`
///
/// Clone an arbitrary git URL into `~/<owner>/<repo>` (or a
/// caller-provided `clone_root`) and register it in the dashboard DB.
/// Optionally runs `crosslink init` + `crosslink agent init` in the
/// fresh clone so writes work immediately.
///
/// This is the standalone counterpart to the PAT-gated `track-all`
/// flow — lets operators add a single repo by URL without configuring
/// GitHub enumeration.
async fn clone_repo(
    State(state): State<AppState>,
    Json(body): Json<CloneRepoBody>,
) -> Result<Json<CloneRepoOutcome>, ApiError> {
    let db_path = state
        .dashboard_db_path
        .clone()
        .ok_or_else(|| ApiError::bad_request("dashboard DB not configured"))?;

    let url = body.url.trim().to_string();
    if url.is_empty() {
        return Err(ApiError::bad_request("url is required"));
    }

    // Derive slug from the URL if the caller didn't provide one.
    let slug = match body
        .slug
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(s) => s.to_string(),
        None => super::projects::slug_from_remote_url(&url).ok_or_else(|| {
            ApiError::bad_request(format!(
                "could not derive slug from URL `{url}`; pass `slug` in the body as `owner/repo`"
            ))
        })?,
    };

    // Default to `$HOME` so each repo lands at `~/<owner>/<repo>`
    // — same as track-all and consistent with the discover walker
    // which scans `$HOME` for crosslink-enabled repos.
    let clone_root = body
        .clone_root
        .clone()
        .unwrap_or_else(|| std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()));

    // If init was requested, require agent_id up front so we bail
    // before the network fetch.
    let init_agent_id = if body.init {
        let id = body
            .agent_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ApiError::bad_request("init=true requires agent_id"))?;
        Some(id.to_string())
    } else {
        None
    };

    // Derive a target path `<clone_root>/<repo>` from the slug — flat
    // shape to match the user's existing manual clone layout
    // (`~/crosslink`, `~/ferrotorch`, …) rather than nesting under
    // `<owner>/`. Slug still carries the owner for dashboard
    // identity; only the on-disk path drops it.
    let mut parts = slug.splitn(2, '/');
    let owner = parts.next().unwrap_or_default().to_string();
    let repo = parts.next().unwrap_or_default().to_string();
    if owner.is_empty() || repo.is_empty() {
        return Err(ApiError::bad_request(format!(
            "slug must be `owner/repo`, got `{slug}`"
        )));
    }
    let target = std::path::PathBuf::from(&clone_root).join(&repo);

    // Clone + register all on the blocking pool.
    let slug_for_task = slug.clone();
    let outcome =
        tokio::task::spawn_blocking(move || -> Result<CloneRepoOutcome, anyhow::Error> {
            if !target.is_dir() {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let status = std::process::Command::new("git")
                    .args(["clone", "--quiet", &url, target.to_string_lossy().as_ref()])
                    .status()?;
                anyhow::ensure!(status.success(), "git clone failed for {url}");
            }

            let initialized = if let Some(ref aid) = init_agent_id {
                if super::projects::write_capability(&target)
                    != super::projects::WriteCapability::Ready
                {
                    super::projects::run_init_and_agent_in(&target, aid)?;
                }
                true
            } else {
                false
            };

            super::projects::track_at_path(&target, Some(&slug_for_task), &db_path)?;

            Ok(CloneRepoOutcome {
                slug: slug_for_task,
                clone_path: target.to_string_lossy().into_owned(),
                initialized,
            })
        })
        .await
        .map_err(|e| ApiError::internal(format!("clone task panicked: {e}")))?
        .map_err(|e| ApiError::internal(format!("{e:#}")))?;

    Ok(Json(outcome))
}

/// `POST /api/v1/dashboard/w/{owner}/{repo}/init`
///
/// Retrofit handler: runs `crosslink init --defaults` and
/// `crosslink agent init <agent_id>` in the tracked workspace so a
/// repo that was cloned without `--init` can be brought to
/// `write_capability: "ready"` without dropping to a shell.
///
/// Idempotent-on-Ready: if the workspace is already fully initialised
/// this is a no-op returning a success response.
async fn init_project(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<InitProjectBody>,
) -> Result<Json<ActionResponse>, ApiError> {
    let agent_id = body.agent_id.trim().to_string();
    if agent_id.is_empty() {
        return Err(ApiError::bad_request(
            "agent_id cannot be empty".to_string(),
        ));
    }
    let (_db_path, project) = resolve_project(&state, &owner, &repo).await?;

    // Move into a blocking task — both shells do real FS work.
    let clone_path = project.clone_path.clone();
    let outcome =
        tokio::task::spawn_blocking(move || -> Result<(String, String), anyhow::Error> {
            if super::projects::write_capability(&clone_path)
                == super::projects::WriteCapability::Ready
            {
                return Ok((
                    format!("{} already initialised", project.slug),
                    String::new(),
                ));
            }
            super::projects::run_init_and_agent_in(&clone_path, &agent_id)?;
            Ok((
                format!("Initialised {} with agent {agent_id}", project.slug),
                String::new(),
            ))
        })
        .await
        .map_err(|e| ApiError::internal(format!("init task panicked: {e}")))?
        // `{:#}` includes the anyhow context chain (e.g. the underlying
        // OS error like "No such file or directory" under the
        // "spawn `crosslink init`" wrapper) instead of just the top
        // line. Without this, users see the wrapper and can't diagnose.
        .map_err(|e| ApiError::internal(format!("{e:#}")))?;

    Ok(Json(ActionResponse {
        stdout: outcome.0,
        stderr: outcome.1,
    }))
}

/// `POST /api/v1/dashboard/w/{owner}/{repo}/integrity/sign-backfill`
///
/// Runs `crosslink integrity sign-backfill --confirm` in the
/// tracked workspace. Used from `signature_invalid` alerts to let
/// the operator re-sign unsigned / invalidly-signed commits on the
/// hub branch without dropping to a shell.
async fn sign_backfill(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
) -> Result<Json<ActionResponse>, ApiError> {
    let (db_path, project) = resolve_project(&state, &owner, &repo).await?;
    let result = actions::run_cli(
        &db_path,
        &project,
        "sign_backfill",
        Some("integrity:sign-backfill"),
        &["integrity", "sign-backfill", "--confirm"],
    )
    .await
    .map_err(|e| ApiError::internal(format!("{e:#}")))?;
    Ok(Json(ActionResponse {
        stdout: result.stdout,
        stderr: result.stderr,
    }))
}

/// Shared setup: find the project by `{owner}/{repo}`, return a 404
/// if it isn't tracked.
async fn resolve_project(
    state: &AppState,
    owner: &str,
    repo: &str,
) -> Result<(std::path::PathBuf, Project), ApiError> {
    let slug = format!("{owner}/{repo}");
    let db_path = state
        .dashboard_db_path
        .as_ref()
        .ok_or_else(|| ApiError::bad_request("dashboard DB not configured for this server"))?
        .clone();

    let lookup_slug = slug.clone();
    let db_path_clone = db_path.clone();
    let project = tokio::task::spawn_blocking(move || -> Result<Option<Project>, ApiError> {
        let db = DashboardDb::open(&db_path_clone)
            .map_err(|e| ApiError::internal(format!("open db: {e}")))?;
        actions::find_project_by_slug(&db, &lookup_slug)
            .map_err(|e| ApiError::internal(format!("lookup: {e}")))
    })
    .await
    .map_err(|e| ApiError::internal(format!("lookup task panicked: {e}")))??;

    let project =
        project.ok_or_else(|| ApiError::not_found(format!("project '{slug}' not tracked")))?;
    Ok((db_path, project))
}

/// `POST /api/v1/dashboard/w/{owner}/{repo}/issues/{id}/close`
async fn close_issue(
    State(state): State<AppState>,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> Result<Json<ActionResponse>, ApiError> {
    let (db_path, project) = resolve_project(&state, &owner, &repo).await?;
    let id_str = id.to_string();
    let result = actions::run_cli(
        &db_path,
        &project,
        "close_issue",
        Some(&format!("issue:{id}")),
        &["issue", "close", &id_str],
    )
    .await
    .map_err(|e| ApiError::internal(format!("{e:#}")))?;
    Ok(Json(ActionResponse {
        stdout: result.stdout,
        stderr: result.stderr,
    }))
}

/// `POST /api/v1/dashboard/w/{owner}/{repo}/issues/{id}/reopen`
async fn reopen_issue(
    State(state): State<AppState>,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> Result<Json<ActionResponse>, ApiError> {
    let (db_path, project) = resolve_project(&state, &owner, &repo).await?;
    let id_str = id.to_string();
    let result = actions::run_cli(
        &db_path,
        &project,
        "reopen_issue",
        Some(&format!("issue:{id}")),
        &["issue", "reopen", &id_str],
    )
    .await
    .map_err(|e| ApiError::internal(format!("{e:#}")))?;
    Ok(Json(ActionResponse {
        stdout: result.stdout,
        stderr: result.stderr,
    }))
}

/// `POST /api/v1/dashboard/w/{owner}/{repo}/issues/{id}/comment`
///
/// Body: `{ "content": "..." }`.
async fn comment_issue(
    State(state): State<AppState>,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    Json(body): Json<CommentBody>,
) -> Result<Json<ActionResponse>, ApiError> {
    if body.content.trim().is_empty() {
        return Err(ApiError::bad_request("comment content cannot be empty"));
    }
    let (db_path, project) = resolve_project(&state, &owner, &repo).await?;
    let id_str = id.to_string();
    let result = actions::run_cli(
        &db_path,
        &project,
        "comment_issue",
        Some(&format!("issue:{id}")),
        &["issue", "comment", &id_str, &body.content],
    )
    .await
    .map_err(|e| ApiError::internal(format!("{e:#}")))?;
    Ok(Json(ActionResponse {
        stdout: result.stdout,
        stderr: result.stderr,
    }))
}

#[derive(Debug, Deserialize)]
struct BlockerBody {
    blocker_id: i64,
}

#[derive(Debug, Deserialize)]
struct RelateBody {
    other_id: i64,
}

#[derive(Debug, Deserialize)]
struct LabelBody {
    label: String,
}

#[derive(Debug, Deserialize)]
struct MilestoneCreateBody {
    name: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MilestoneIssueBody {
    issue_id: i64,
}

/// `POST /api/v1/dashboard/w/{owner}/{repo}/issues/{id}/block`
///
/// Body: `{ "blocker_id": N }`. Marks issue `{id}` as blocked by issue `N`.
async fn block_issue(
    State(state): State<AppState>,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    Json(body): Json<BlockerBody>,
) -> Result<Json<ActionResponse>, ApiError> {
    let (db_path, project) = resolve_project(&state, &owner, &repo).await?;
    let id_str = id.to_string();
    let blocker_str = body.blocker_id.to_string();
    let result = actions::run_cli(
        &db_path,
        &project,
        "block_issue",
        Some(&format!("issue:{id}")),
        &["issue", "block", &id_str, &blocker_str],
    )
    .await
    .map_err(|e| ApiError::internal(format!("{e:#}")))?;
    Ok(Json(ActionResponse {
        stdout: result.stdout,
        stderr: result.stderr,
    }))
}

/// `POST /api/v1/dashboard/w/{owner}/{repo}/issues/{id}/unblock`
async fn unblock_issue(
    State(state): State<AppState>,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    Json(body): Json<BlockerBody>,
) -> Result<Json<ActionResponse>, ApiError> {
    let (db_path, project) = resolve_project(&state, &owner, &repo).await?;
    let id_str = id.to_string();
    let blocker_str = body.blocker_id.to_string();
    let result = actions::run_cli(
        &db_path,
        &project,
        "unblock_issue",
        Some(&format!("issue:{id}")),
        &["issue", "unblock", &id_str, &blocker_str],
    )
    .await
    .map_err(|e| ApiError::internal(format!("{e:#}")))?;
    Ok(Json(ActionResponse {
        stdout: result.stdout,
        stderr: result.stderr,
    }))
}

/// `POST /api/v1/dashboard/w/{owner}/{repo}/issues/{id}/relate`
///
/// Body: `{ "other_id": N }`. Symmetric link — order doesn't matter.
async fn relate_issue(
    State(state): State<AppState>,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    Json(body): Json<RelateBody>,
) -> Result<Json<ActionResponse>, ApiError> {
    let (db_path, project) = resolve_project(&state, &owner, &repo).await?;
    let id_str = id.to_string();
    let other_str = body.other_id.to_string();
    let result = actions::run_cli(
        &db_path,
        &project,
        "relate_issue",
        Some(&format!("issue:{id}")),
        &["issue", "relate", &id_str, &other_str],
    )
    .await
    .map_err(|e| ApiError::internal(format!("{e:#}")))?;
    Ok(Json(ActionResponse {
        stdout: result.stdout,
        stderr: result.stderr,
    }))
}

/// `POST /api/v1/dashboard/w/{owner}/{repo}/issues/{id}/label`
///
/// Body: `{ "label": "bug" }`. Adds a single label. Empty rejected 400.
async fn label_issue(
    State(state): State<AppState>,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    Json(body): Json<LabelBody>,
) -> Result<Json<ActionResponse>, ApiError> {
    if body.label.trim().is_empty() {
        return Err(ApiError::bad_request("label cannot be empty"));
    }
    let (db_path, project) = resolve_project(&state, &owner, &repo).await?;
    let id_str = id.to_string();
    let result = actions::run_cli(
        &db_path,
        &project,
        "label_issue",
        Some(&format!("issue:{id}")),
        &["issue", "label", &id_str, &body.label],
    )
    .await
    .map_err(|e| ApiError::internal(format!("{e:#}")))?;
    Ok(Json(ActionResponse {
        stdout: result.stdout,
        stderr: result.stderr,
    }))
}

/// `POST /api/v1/dashboard/w/{owner}/{repo}/issues/{id}/unlabel`
async fn unlabel_issue(
    State(state): State<AppState>,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    Json(body): Json<LabelBody>,
) -> Result<Json<ActionResponse>, ApiError> {
    if body.label.trim().is_empty() {
        return Err(ApiError::bad_request("label cannot be empty"));
    }
    let (db_path, project) = resolve_project(&state, &owner, &repo).await?;
    let id_str = id.to_string();
    let result = actions::run_cli(
        &db_path,
        &project,
        "unlabel_issue",
        Some(&format!("issue:{id}")),
        &["issue", "unlabel", &id_str, &body.label],
    )
    .await
    .map_err(|e| ApiError::internal(format!("{e:#}")))?;
    Ok(Json(ActionResponse {
        stdout: result.stdout,
        stderr: result.stderr,
    }))
}

/// `POST /api/v1/dashboard/w/{owner}/{repo}/milestones`
///
/// Body: `{ "name": "...", "description": "..." }`. Name is required
/// and must be non-empty; description is optional.
async fn create_milestone(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<MilestoneCreateBody>,
) -> Result<Json<ActionResponse>, ApiError> {
    if body.name.trim().is_empty() {
        return Err(ApiError::bad_request("milestone name cannot be empty"));
    }
    let (db_path, project) = resolve_project(&state, &owner, &repo).await?;
    let mut args: Vec<&str> = vec!["milestone", "create", &body.name];
    if let Some(desc) = body.description.as_deref() {
        if !desc.trim().is_empty() {
            args.push("-d");
            args.push(desc);
        }
    }
    let result = actions::run_cli(
        &db_path,
        &project,
        "create_milestone",
        Some(&format!("milestone:{}", body.name)),
        &args,
    )
    .await
    .map_err(|e| ApiError::internal(format!("{e:#}")))?;
    Ok(Json(ActionResponse {
        stdout: result.stdout,
        stderr: result.stderr,
    }))
}

/// `POST /api/v1/dashboard/w/{owner}/{repo}/milestones/{id}/add`
///
/// Body: `{ "issue_id": N }`. Attaches issue `N` to milestone `{id}`.
async fn milestone_add_issue(
    State(state): State<AppState>,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    Json(body): Json<MilestoneIssueBody>,
) -> Result<Json<ActionResponse>, ApiError> {
    let (db_path, project) = resolve_project(&state, &owner, &repo).await?;
    let id_str = id.to_string();
    let issue_str = body.issue_id.to_string();
    let result = actions::run_cli(
        &db_path,
        &project,
        "milestone_add",
        Some(&format!("milestone:{id}")),
        &["milestone", "add", &id_str, &issue_str],
    )
    .await
    .map_err(|e| ApiError::internal(format!("{e:#}")))?;
    Ok(Json(ActionResponse {
        stdout: result.stdout,
        stderr: result.stderr,
    }))
}

/// `POST /api/v1/dashboard/w/{owner}/{repo}/milestones/{id}/remove`
async fn milestone_remove_issue(
    State(state): State<AppState>,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    Json(body): Json<MilestoneIssueBody>,
) -> Result<Json<ActionResponse>, ApiError> {
    let (db_path, project) = resolve_project(&state, &owner, &repo).await?;
    let id_str = id.to_string();
    let issue_str = body.issue_id.to_string();
    let result = actions::run_cli(
        &db_path,
        &project,
        "milestone_remove",
        Some(&format!("milestone:{id}")),
        &["milestone", "remove", &id_str, &issue_str],
    )
    .await
    .map_err(|e| ApiError::internal(format!("{e:#}")))?;
    Ok(Json(ActionResponse {
        stdout: result.stdout,
        stderr: result.stderr,
    }))
}

#[derive(Debug, Default, Deserialize)]
struct ClaimLockBody {
    #[serde(default)]
    branch: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AgentRequestBody {
    /// kill | pause | resume | reprioritise
    kind: String,
    #[serde(default)]
    subject_issue: Option<i64>,
    #[serde(default)]
    reason: Option<String>,
}

/// `POST /api/v1/dashboard/w/{owner}/{repo}/agents/{agent_id}/request`
///
/// Body: `{ "kind": "kill|pause|resume|reprioritise",
///          "subject_issue"?: N, "reason"?: "..." }`. Shells out to
/// `crosslink agent request`, which writes a signed JSON under
/// `agents/<agent_id>/requests/` on the hub branch. See design doc §9.
async fn agent_request(
    State(state): State<AppState>,
    Path((owner, repo, agent_id)): Path<(String, String, String)>,
    Json(body): Json<AgentRequestBody>,
) -> Result<Json<ActionResponse>, ApiError> {
    if body.kind.trim().is_empty() {
        return Err(ApiError::bad_request("request kind cannot be empty"));
    }
    // Validate kind before shelling out — gives a precise 400 instead of
    // a generic CLI error on the other side.
    crate::agent_requests::RequestKind::parse(body.kind.trim())
        .map_err(|e| ApiError::bad_request(e.to_string()))?;

    let (db_path, project) = resolve_project(&state, &owner, &repo).await?;

    let kind = body.kind.trim().to_string();
    let subject_str = body.subject_issue.map(|n| n.to_string());
    let reason = body.reason.as_ref().map(|s| s.trim().to_string());

    let mut args: Vec<&str> = vec!["agent", "request", &agent_id, &kind];
    if let Some(ref s) = subject_str {
        args.push("--subject-issue");
        args.push(s);
    }
    if let Some(ref r) = reason {
        if !r.is_empty() {
            args.push("--reason");
            args.push(r);
        }
    }

    let result = actions::run_cli(
        &db_path,
        &project,
        "agent_request",
        Some(&format!("agent:{agent_id}")),
        &args,
    )
    .await
    .map_err(|e| ApiError::internal(format!("{e:#}")))?;
    Ok(Json(ActionResponse {
        stdout: result.stdout,
        stderr: result.stderr,
    }))
}

/// `POST /api/v1/dashboard/w/{owner}/{repo}/locks/{id}/claim`
///
/// Body: optional `{ "branch": "..." }`. Claiming from the dashboard
/// is uncommon (agents normally claim their own locks), but we expose
/// it so operators can seed a lock during triage.
async fn claim_lock(
    State(state): State<AppState>,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    body: Option<Json<ClaimLockBody>>,
) -> Result<Json<ActionResponse>, ApiError> {
    let (db_path, project) = resolve_project(&state, &owner, &repo).await?;
    let id_str = id.to_string();
    let branch = body.and_then(|b| b.0.branch).unwrap_or_default();
    let mut args: Vec<&str> = vec!["locks", "claim", &id_str];
    if !branch.trim().is_empty() {
        args.push("-b");
        args.push(branch.trim());
    }
    let result = actions::run_cli(
        &db_path,
        &project,
        "claim_lock",
        Some(&format!("lock:{id}")),
        &args,
    )
    .await
    .map_err(|e| ApiError::internal(format!("{e:#}")))?;
    Ok(Json(ActionResponse {
        stdout: result.stdout,
        stderr: result.stderr,
    }))
}

/// `POST /api/v1/dashboard/w/{owner}/{repo}/locks/{id}/release`
async fn release_lock(
    State(state): State<AppState>,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> Result<Json<ActionResponse>, ApiError> {
    let (db_path, project) = resolve_project(&state, &owner, &repo).await?;
    let id_str = id.to_string();
    let result = actions::run_cli(
        &db_path,
        &project,
        "release_lock",
        Some(&format!("lock:{id}")),
        &["locks", "release", &id_str],
    )
    .await
    .map_err(|e| ApiError::internal(format!("{e:#}")))?;
    Ok(Json(ActionResponse {
        stdout: result.stdout,
        stderr: result.stderr,
    }))
}

/// `POST /api/v1/dashboard/w/{owner}/{repo}/locks/{id}/steal`
///
/// Hijacks a stale lock held by another agent. The CLI itself enforces
/// the staleness threshold, so we just pass through.
async fn steal_lock(
    State(state): State<AppState>,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> Result<Json<ActionResponse>, ApiError> {
    let (db_path, project) = resolve_project(&state, &owner, &repo).await?;
    let id_str = id.to_string();
    let result = actions::run_cli(
        &db_path,
        &project,
        "steal_lock",
        Some(&format!("lock:{id}")),
        &["locks", "steal", &id_str],
    )
    .await
    .map_err(|e| ApiError::internal(format!("{e:#}")))?;
    Ok(Json(ActionResponse {
        stdout: result.stdout,
        stderr: result.stderr,
    }))
}

/// `POST /api/v1/dashboard/w/{owner}/{repo}/milestones/{id}/close`
async fn close_milestone(
    State(state): State<AppState>,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> Result<Json<ActionResponse>, ApiError> {
    let (db_path, project) = resolve_project(&state, &owner, &repo).await?;
    let id_str = id.to_string();
    let result = actions::run_cli(
        &db_path,
        &project,
        "close_milestone",
        Some(&format!("milestone:{id}")),
        &["milestone", "close", &id_str],
    )
    .await
    .map_err(|e| ApiError::internal(format!("{e:#}")))?;
    Ok(Json(ActionResponse {
        stdout: result.stdout,
        stderr: result.stderr,
    }))
}

/// Minimal typed error with status-code mapping. Patterned after axum
/// idioms — manual `IntoResponse` implementation maps to the right
/// status without pulling in a full error-handling crate.
#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn not_found(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: msg.into(),
        }
    }
    fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: msg.into(),
        }
    }
    fn internal(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: msg.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let body = Json(serde_json::json!({
            "error": self.message,
            "status": self.status.as_u16(),
        }));
        (self.status, body).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt; // .oneshot(...)

    /// Build a minimal `AppState` wired to a temp DB.
    fn test_state(dashboard_db: Option<std::path::PathBuf>) -> (AppState, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let main_db_path = dir.path().join("crosslink.db");
        let main_db = crate::db::Database::open(&main_db_path).unwrap();
        let mut state = AppState::new(main_db, dir.path().join(".crosslink"));
        if let Some(p) = dashboard_db {
            state = state.with_dashboard_db(p);
        }
        (state, dir)
    }

    fn seed_project(db_path: &std::path::Path, slug: &str, clone_path: &std::path::Path) -> i64 {
        let db = DashboardDb::open(db_path).unwrap();
        db.conn
            .execute(
                "INSERT INTO projects (slug, clone_path, default_branch, status, added_at)
                 VALUES (?1, ?2, 'main', 'active', '2026-04-20T00:00:00Z')",
                rusqlite::params![slug, clone_path.to_string_lossy().as_ref()],
            )
            .unwrap();
        db.conn.last_insert_rowid()
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn test_list_projects_without_dashboard_db_returns_400() {
        let (state, _tmp) = test_state(None);
        let app = Router::new()
            .nest("/api/v1/dashboard", build_router())
            .with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/dashboard/projects")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_list_projects_empty_returns_empty_array() {
        let tmp = tempfile::tempdir().unwrap();
        let dashboard_db_path = tmp.path().join("dashboard.db");
        DashboardDb::open(&dashboard_db_path).unwrap();

        let (state, _tmp2) = test_state(Some(dashboard_db_path));
        let app = Router::new()
            .nest("/api/v1/dashboard", build_router())
            .with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/dashboard/projects")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json, serde_json::json!([]));
    }

    #[tokio::test]
    async fn test_list_projects_returns_tracked() {
        let tmp = tempfile::tempdir().unwrap();
        let dashboard_db_path = tmp.path().join("dashboard.db");
        DashboardDb::open(&dashboard_db_path).unwrap();
        let clone_path = tmp.path().join("owner").join("repo");
        seed_project(&dashboard_db_path, "owner/repo", &clone_path);

        let (state, _tmp2) = test_state(Some(dashboard_db_path));
        let app = Router::new()
            .nest("/api/v1/dashboard", build_router())
            .with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/dashboard/projects")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["slug"], "owner/repo");
        assert_eq!(arr[0]["status"], "active");
        // counters default to zeros when project_state hasn't been populated.
        assert_eq!(arr[0]["counters"]["open_issues"], 0);
    }

    #[tokio::test]
    async fn test_project_detail_unknown_slug_returns_404() {
        let tmp = tempfile::tempdir().unwrap();
        let dashboard_db_path = tmp.path().join("dashboard.db");
        DashboardDb::open(&dashboard_db_path).unwrap();

        let (state, _tmp2) = test_state(Some(dashboard_db_path));
        let app = Router::new()
            .nest("/api/v1/dashboard", build_router())
            .with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/dashboard/projects/does-not/exist")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_list_alerts_without_dashboard_db_returns_400() {
        let (state, _tmp) = test_state(None);
        let app = Router::new()
            .nest("/api/v1/dashboard", build_router())
            .with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/dashboard/alerts")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_list_alerts_empty_returns_empty_array() {
        let tmp = tempfile::tempdir().unwrap();
        let dashboard_db_path = tmp.path().join("dashboard.db");
        DashboardDb::open(&dashboard_db_path).unwrap();

        let (state, _tmp2) = test_state(Some(dashboard_db_path));
        let app = Router::new()
            .nest("/api/v1/dashboard", build_router())
            .with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/dashboard/alerts")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json, serde_json::json!([]));
    }

    #[tokio::test]
    async fn test_list_alerts_returns_open_rows_with_project_slug() {
        let tmp = tempfile::tempdir().unwrap();
        let dashboard_db_path = tmp.path().join("dashboard.db");
        let db = DashboardDb::open(&dashboard_db_path).unwrap();
        db.conn
            .execute(
                "INSERT INTO projects (slug, clone_path, default_branch, status, added_at)
                 VALUES ('owner/repo', '/tmp/x', 'main', 'active', '2026-04-20T00:00:00Z')",
                [],
            )
            .unwrap();
        let project_id = db.conn.last_insert_rowid();
        db.conn
            .execute(
                "INSERT INTO alerts
                   (project_id, kind, severity, subject_ref, detail, opened_at)
                 VALUES (?1, 'stale_lock', 'warning', 'lock:42', 'held too long', '2026-04-20T12:00:00Z')",
                rusqlite::params![project_id],
            )
            .unwrap();
        // A resolved alert should NOT show up.
        db.conn
            .execute(
                "INSERT INTO alerts
                   (project_id, kind, severity, subject_ref, detail, opened_at, resolved_at)
                 VALUES (?1, 'overdue_issue', 'warning', 'issue:1', 'was overdue', '2026-04-20T10:00:00Z', '2026-04-20T11:00:00Z')",
                rusqlite::params![project_id],
            )
            .unwrap();

        let (state, _tmp2) = test_state(Some(dashboard_db_path));
        let app = Router::new()
            .nest("/api/v1/dashboard", build_router())
            .with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/dashboard/alerts")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 1, "only one open alert should be returned");
        assert_eq!(arr[0]["project_slug"], "owner/repo");
        assert_eq!(arr[0]["kind"], "stale_lock");
        assert_eq!(arr[0]["subject_ref"], "lock:42");
    }

    #[tokio::test]
    async fn test_close_issue_returns_404_for_untracked_slug() {
        let tmp = tempfile::tempdir().unwrap();
        let dashboard_db_path = tmp.path().join("dashboard.db");
        DashboardDb::open(&dashboard_db_path).unwrap();

        let (state, _tmp2) = test_state(Some(dashboard_db_path));
        let app = Router::new()
            .nest("/api/v1/dashboard", build_router())
            .with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/dashboard/w/owner/repo/issues/42/close")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_comment_issue_rejects_empty_content() {
        let tmp = tempfile::tempdir().unwrap();
        let dashboard_db_path = tmp.path().join("dashboard.db");
        let db = DashboardDb::open(&dashboard_db_path).unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["init", "-q"])
            .status()
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO projects (slug, clone_path, default_branch, status, added_at)
                 VALUES ('owner/repo', ?1, 'main', 'active', '2026-04-20T00:00:00Z')",
                [repo.to_string_lossy().as_ref()],
            )
            .unwrap();

        let (state, _tmp2) = test_state(Some(dashboard_db_path));
        let app = Router::new()
            .nest("/api/v1/dashboard", build_router())
            .with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/dashboard/w/owner/repo/issues/1/comment")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"content":"   "}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_label_issue_rejects_empty_label() {
        let tmp = tempfile::tempdir().unwrap();
        let dashboard_db_path = tmp.path().join("dashboard.db");
        let db = DashboardDb::open(&dashboard_db_path).unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["init", "-q"])
            .status()
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO projects (slug, clone_path, default_branch, status, added_at)
                 VALUES ('owner/repo', ?1, 'main', 'active', '2026-04-20T00:00:00Z')",
                [repo.to_string_lossy().as_ref()],
            )
            .unwrap();

        let (state, _tmp2) = test_state(Some(dashboard_db_path));
        let app = Router::new()
            .nest("/api/v1/dashboard", build_router())
            .with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/dashboard/w/owner/repo/issues/1/label")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"label":"   "}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_create_milestone_rejects_empty_name() {
        let tmp = tempfile::tempdir().unwrap();
        let dashboard_db_path = tmp.path().join("dashboard.db");
        let db = DashboardDb::open(&dashboard_db_path).unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["init", "-q"])
            .status()
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO projects (slug, clone_path, default_branch, status, added_at)
                 VALUES ('owner/repo', ?1, 'main', 'active', '2026-04-20T00:00:00Z')",
                [repo.to_string_lossy().as_ref()],
            )
            .unwrap();

        let (state, _tmp2) = test_state(Some(dashboard_db_path));
        let app = Router::new()
            .nest("/api/v1/dashboard", build_router())
            .with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/dashboard/w/owner/repo/milestones")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name":"  "}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_agent_request_rejects_unknown_kind() {
        let tmp = tempfile::tempdir().unwrap();
        let dashboard_db_path = tmp.path().join("dashboard.db");
        let db = DashboardDb::open(&dashboard_db_path).unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["init", "-q"])
            .status()
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO projects (slug, clone_path, default_branch, status, added_at)
                 VALUES ('owner/repo', ?1, 'main', 'active', '2026-04-20T00:00:00Z')",
                [repo.to_string_lossy().as_ref()],
            )
            .unwrap();

        let (state, _tmp2) = test_state(Some(dashboard_db_path));
        let app = Router::new()
            .nest("/api/v1/dashboard", build_router())
            .with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/dashboard/w/owner/repo/agents/jus4/request")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"kind":"bogus"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_agent_request_returns_404_for_untracked_slug() {
        let tmp = tempfile::tempdir().unwrap();
        let dashboard_db_path = tmp.path().join("dashboard.db");
        DashboardDb::open(&dashboard_db_path).unwrap();

        let (state, _tmp2) = test_state(Some(dashboard_db_path));
        let app = Router::new()
            .nest("/api/v1/dashboard", build_router())
            .with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/dashboard/w/nobody/noop/agents/jus4/request")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"kind":"pause"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_project_detail_surfaces_pending_agent_requests() {
        let tmp = tempfile::tempdir().unwrap();
        let dashboard_db_path = tmp.path().join("dashboard.db");
        DashboardDb::open(&dashboard_db_path).unwrap();

        let clone = tmp.path().join("clone");
        std::fs::create_dir_all(clone.join("agents/jus4/requests")).unwrap();
        // Write a pending request; no ack.
        let req = crate::agent_requests::AgentRequest {
            request_id: "01HXY000000000000000000001".into(),
            kind: crate::agent_requests::RequestKind::Pause,
            subject: crate::agent_requests::RequestSubject { issue_id: Some(42) },
            requested_by: "SHA256:driver".into(),
            requested_at: "2026-04-20T18:30:00Z".into(),
            reason: Some("stuck".into()),
        };
        std::fs::write(
            clone.join(format!("agents/jus4/requests/{}.json", req.request_id)),
            serde_json::to_vec(&req).unwrap(),
        )
        .unwrap();

        seed_project(&dashboard_db_path, "owner/repo", &clone);

        let (state, _tmp2) = test_state(Some(dashboard_db_path));
        let app = Router::new()
            .nest("/api/v1/dashboard", build_router())
            .with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/dashboard/projects/owner/repo")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        let groups = json["agent_requests"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["agent_id"], "jus4");
        let reqs = groups[0]["requests"].as_array().unwrap();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0]["kind"], "pause");
        assert_eq!(reqs[0]["subject_issue"], 42);
        assert!(reqs[0]["ack"].is_null());
    }

    #[tokio::test]
    async fn test_release_lock_returns_404_for_untracked_slug() {
        let tmp = tempfile::tempdir().unwrap();
        let dashboard_db_path = tmp.path().join("dashboard.db");
        DashboardDb::open(&dashboard_db_path).unwrap();

        let (state, _tmp2) = test_state(Some(dashboard_db_path));
        let app = Router::new()
            .nest("/api/v1/dashboard", build_router())
            .with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/dashboard/w/nobody/noop/locks/7/release")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_block_issue_returns_404_for_untracked_slug() {
        let tmp = tempfile::tempdir().unwrap();
        let dashboard_db_path = tmp.path().join("dashboard.db");
        DashboardDb::open(&dashboard_db_path).unwrap();

        let (state, _tmp2) = test_state(Some(dashboard_db_path));
        let app = Router::new()
            .nest("/api/v1/dashboard", build_router())
            .with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/dashboard/w/nobody/noop/issues/1/block")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"blocker_id":2}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_project_detail_returns_empty_snapshot_when_clone_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let dashboard_db_path = tmp.path().join("dashboard.db");
        DashboardDb::open(&dashboard_db_path).unwrap();
        let clone_path = tmp.path().join("does-not-exist");
        seed_project(&dashboard_db_path, "owner/repo", &clone_path);

        let (state, _tmp2) = test_state(Some(dashboard_db_path));
        let app = Router::new()
            .nest("/api/v1/dashboard", build_router())
            .with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/dashboard/projects/owner/repo")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["slug"], "owner/repo");
        assert_eq!(json["issues"], serde_json::json!([]));
        assert_eq!(json["agents"], serde_json::json!([]));
        assert_eq!(json["locks"], serde_json::json!([]));
    }
}
