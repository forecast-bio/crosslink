//! Dashboard export endpoints (design doc §14 Phase 5 — Polish).
//!
//! Lets the operator snapshot the aggregator's current view as
//! portable files for status reports, spreadsheet analysis, and
//! archival. The data matches what the UI sees — same queries as
//! [`super::api::list_projects`] / [`super::api::list_alerts`] — so
//! export reflects the panel, not a parallel re-derivation.
//!
//! Endpoints (all GET, all auth-gated by the outer router):
//! - `/api/v1/dashboard/export/projects.csv`
//! - `/api/v1/dashboard/export/projects.json`
//! - `/api/v1/dashboard/export/alerts.csv`
//! - `/api/v1/dashboard/export/alerts.json`
//!
//! Screenshot export is intentionally deferred — that's a frontend
//! concern (`html2canvas` or the browser's native print flow) and
//! doesn't need a server round-trip.

use std::fmt::Write as _;

use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};

use super::db::DashboardDb;
use crate::server::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/export/projects.csv", get(export_projects_csv))
        .route("/export/projects.json", get(export_projects_json))
        .route("/export/alerts.csv", get(export_alerts_csv))
        .route("/export/alerts.json", get(export_alerts_json))
}

#[derive(Debug)]
enum ExportError {
    BadRequest(String),
    Internal(String),
}

impl IntoResponse for ExportError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            Self::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
        };
        (status, Json(serde_json::json!({"error": msg}))).into_response()
    }
}

/// Shape of one project row in exports. Kept independent of the
/// `ProjectListItem` wire type so the JSON/CSV schemas can evolve
/// without breaking the live tile rendering.
#[derive(Debug, serde::Serialize)]
struct ProjectExportRow {
    slug: String,
    status: String,
    pinned: bool,
    hub_sha: Option<String>,
    hub_fetched_at: Option<String>,
    last_activity_at: Option<String>,
    added_at: String,
    open_issues: i64,
    overdue_issues: i64,
    due_soon_issues: i64,
    blocked_issues: i64,
    active_agents: i64,
    stale_locks: i64,
    ci_status: Option<String>,
    counters_updated_at: Option<String>,
}

#[derive(Debug, serde::Serialize)]
struct AlertExportRow {
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

fn require_db_path(state: &AppState) -> Result<std::path::PathBuf, ExportError> {
    state
        .dashboard_db_path
        .clone()
        .ok_or_else(|| ExportError::BadRequest("dashboard DB not configured".into()))
}

async fn load_projects(db_path: std::path::PathBuf) -> Result<Vec<ProjectExportRow>, ExportError> {
    tokio::task::spawn_blocking(move || {
        let db = DashboardDb::open(&db_path)
            .map_err(|e| ExportError::Internal(format!("open db: {e}")))?;
        let mut stmt = db
            .conn
            .prepare(
                "SELECT p.slug, p.status, p.pinned, p.hub_sha, p.hub_fetched_at,
                        p.last_activity_at, p.added_at,
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
            .map_err(|e| ExportError::Internal(format!("prepare: {e}")))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(ProjectExportRow {
                    slug: row.get(0)?,
                    status: row.get(1)?,
                    pinned: row.get::<_, i64>(2)? != 0,
                    hub_sha: row.get(3)?,
                    hub_fetched_at: row.get(4)?,
                    last_activity_at: row.get(5)?,
                    added_at: row.get(6)?,
                    open_issues: row.get(7)?,
                    overdue_issues: row.get(8)?,
                    due_soon_issues: row.get(9)?,
                    blocked_issues: row.get(10)?,
                    active_agents: row.get(11)?,
                    stale_locks: row.get(12)?,
                    ci_status: row.get(13)?,
                    counters_updated_at: row.get(14)?,
                })
            })
            .map_err(|e| ExportError::Internal(format!("query: {e}")))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| ExportError::Internal(format!("collect: {e}")))?;
        Ok(rows)
    })
    .await
    .map_err(|e| ExportError::Internal(format!("task panicked: {e}")))?
}

async fn load_alerts(
    db_path: std::path::PathBuf,
    open_only: bool,
) -> Result<Vec<AlertExportRow>, ExportError> {
    tokio::task::spawn_blocking(move || {
        let db = DashboardDb::open(&db_path)
            .map_err(|e| ExportError::Internal(format!("open db: {e}")))?;
        let sql = if open_only {
            "SELECT a.id, p.slug, a.kind, a.severity, a.subject_ref,
                    a.detail, a.opened_at, a.resolved_at, a.acknowledged_at
             FROM alerts a
             JOIN projects p ON p.id = a.project_id
             WHERE a.resolved_at IS NULL
             ORDER BY a.opened_at DESC"
        } else {
            "SELECT a.id, p.slug, a.kind, a.severity, a.subject_ref,
                    a.detail, a.opened_at, a.resolved_at, a.acknowledged_at
             FROM alerts a
             JOIN projects p ON p.id = a.project_id
             ORDER BY a.opened_at DESC"
        };
        let mut stmt = db
            .conn
            .prepare(sql)
            .map_err(|e| ExportError::Internal(format!("prepare: {e}")))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(AlertExportRow {
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
            .map_err(|e| ExportError::Internal(format!("query: {e}")))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| ExportError::Internal(format!("collect: {e}")))?;
        Ok(rows)
    })
    .await
    .map_err(|e| ExportError::Internal(format!("task panicked: {e}")))?
}

/// RFC 4180 CSV field escaping: wrap in quotes if the value contains a
/// comma, quote, CR, or LF; double any embedded quotes.
fn csv_escape(field: &str) -> String {
    let needs_quote = field
        .bytes()
        .any(|b| b == b',' || b == b'"' || b == b'\n' || b == b'\r');
    if !needs_quote {
        return field.to_string();
    }
    let mut escaped = String::with_capacity(field.len() + 2);
    escaped.push('"');
    for ch in field.chars() {
        if ch == '"' {
            escaped.push('"');
        }
        escaped.push(ch);
    }
    escaped.push('"');
    escaped
}

const fn csv_bool(b: bool) -> &'static str {
    if b {
        "true"
    } else {
        "false"
    }
}

fn csv_opt(s: Option<&str>) -> String {
    s.map_or_else(String::new, csv_escape)
}

fn projects_to_csv(rows: &[ProjectExportRow]) -> String {
    let mut out = String::new();
    out.push_str(
        "slug,status,pinned,hub_sha,hub_fetched_at,last_activity_at,added_at,\
         open_issues,overdue_issues,due_soon_issues,blocked_issues,active_agents,\
         stale_locks,ci_status,counters_updated_at\n",
    );
    for r in rows {
        let _ = writeln!(
            out,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            csv_escape(&r.slug),
            csv_escape(&r.status),
            csv_bool(r.pinned),
            csv_opt(r.hub_sha.as_deref()),
            csv_opt(r.hub_fetched_at.as_deref()),
            csv_opt(r.last_activity_at.as_deref()),
            csv_escape(&r.added_at),
            r.open_issues,
            r.overdue_issues,
            r.due_soon_issues,
            r.blocked_issues,
            r.active_agents,
            r.stale_locks,
            csv_opt(r.ci_status.as_deref()),
            csv_opt(r.counters_updated_at.as_deref()),
        );
    }
    out
}

fn alerts_to_csv(rows: &[AlertExportRow]) -> String {
    let mut out = String::new();
    out.push_str(
        "id,project_slug,kind,severity,subject_ref,detail,\
         opened_at,resolved_at,acknowledged_at\n",
    );
    for r in rows {
        let _ = writeln!(
            out,
            "{},{},{},{},{},{},{},{},{}",
            r.id,
            csv_escape(&r.project_slug),
            csv_escape(&r.kind),
            csv_escape(&r.severity),
            csv_opt(r.subject_ref.as_deref()),
            csv_opt(r.detail.as_deref()),
            csv_escape(&r.opened_at),
            csv_opt(r.resolved_at.as_deref()),
            csv_opt(r.acknowledged_at.as_deref()),
        );
    }
    out
}

fn csv_response(filename: &str, body: String) -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        body,
    )
        .into_response()
}

fn json_response(filename: &str, body: String) -> Response {
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                "application/json; charset=utf-8".to_string(),
            ),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        body,
    )
        .into_response()
}

async fn export_projects_csv(State(state): State<AppState>) -> Result<Response, ExportError> {
    let db_path = require_db_path(&state)?;
    let rows = load_projects(db_path).await?;
    Ok(csv_response(
        "crosslink-projects.csv",
        projects_to_csv(&rows),
    ))
}

async fn export_projects_json(State(state): State<AppState>) -> Result<Response, ExportError> {
    let db_path = require_db_path(&state)?;
    let rows = load_projects(db_path).await?;
    let body = serde_json::to_string_pretty(&rows)
        .map_err(|e| ExportError::Internal(format!("serialize: {e}")))?;
    Ok(json_response("crosslink-projects.json", body))
}

async fn export_alerts_csv(State(state): State<AppState>) -> Result<Response, ExportError> {
    let db_path = require_db_path(&state)?;
    // Default: open alerts only. Matches the /alerts API and what the
    // UI shows. Historical dump lives under ?all=1 in a follow-up.
    let rows = load_alerts(db_path, true).await?;
    Ok(csv_response("crosslink-alerts.csv", alerts_to_csv(&rows)))
}

async fn export_alerts_json(State(state): State<AppState>) -> Result<Response, ExportError> {
    let db_path = require_db_path(&state)?;
    let rows = load_alerts(db_path, true).await?;
    let body = serde_json::to_string_pretty(&rows)
        .map_err(|e| ExportError::Internal(format!("serialize: {e}")))?;
    Ok(json_response("crosslink-alerts.json", body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::state::AppState;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

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

    fn seed_alert(db_path: &std::path::Path, project_id: i64, kind: &str, severity: &str) {
        let db = DashboardDb::open(db_path).unwrap();
        db.conn
            .execute(
                "INSERT INTO alerts (project_id, kind, severity, detail, opened_at)
                 VALUES (?1, ?2, ?3, 'oops', '2026-04-20T00:00:00Z')",
                rusqlite::params![project_id, kind, severity],
            )
            .unwrap();
    }

    async fn body_bytes(resp: axum::response::Response) -> Vec<u8> {
        axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec()
    }

    #[tokio::test]
    async fn test_export_projects_csv_without_db_returns_400() {
        let (state, _tmp) = test_state(None);
        let app = axum::Router::new()
            .nest("/api/v1/dashboard", router())
            .with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/dashboard/export/projects.csv")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_export_projects_csv_returns_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let dashboard_db_path = tmp.path().join("dashboard.db");
        DashboardDb::open(&dashboard_db_path).unwrap();
        seed_project(
            &dashboard_db_path,
            "owner/repo",
            &tmp.path().join("owner").join("repo"),
        );

        let (state, _tmp2) = test_state(Some(dashboard_db_path));
        let app = axum::Router::new()
            .nest("/api/v1/dashboard", router())
            .with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/dashboard/export/projects.csv")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let cd = resp
            .headers()
            .get(header::CONTENT_DISPOSITION)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(ct.starts_with("text/csv"));
        assert!(cd.contains("crosslink-projects.csv"));

        let bytes = body_bytes(resp).await;
        let body = String::from_utf8(bytes).unwrap();
        let mut lines = body.lines();
        assert!(lines.next().unwrap().starts_with("slug,status,pinned,"));
        let row = lines.next().unwrap();
        assert!(row.starts_with("owner/repo,active,false,"));
    }

    #[tokio::test]
    async fn test_export_projects_json_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let dashboard_db_path = tmp.path().join("dashboard.db");
        DashboardDb::open(&dashboard_db_path).unwrap();
        seed_project(
            &dashboard_db_path,
            "owner/repo",
            &tmp.path().join("owner").join("repo"),
        );

        let (state, _tmp2) = test_state(Some(dashboard_db_path));
        let app = axum::Router::new()
            .nest("/api/v1/dashboard", router())
            .with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/dashboard/export/projects.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = body_bytes(resp).await;
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["slug"], "owner/repo");
    }

    #[tokio::test]
    async fn test_export_alerts_csv_filters_open() {
        let tmp = tempfile::tempdir().unwrap();
        let dashboard_db_path = tmp.path().join("dashboard.db");
        DashboardDb::open(&dashboard_db_path).unwrap();
        let pid = seed_project(
            &dashboard_db_path,
            "owner/repo",
            &tmp.path().join("owner").join("repo"),
        );
        seed_alert(&dashboard_db_path, pid, "stale_lock", "warning");

        let (state, _tmp2) = test_state(Some(dashboard_db_path));
        let app = axum::Router::new()
            .nest("/api/v1/dashboard", router())
            .with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/dashboard/export/alerts.csv")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = body_bytes(resp).await;
        let body = String::from_utf8(bytes).unwrap();
        assert!(body.contains("stale_lock,warning"));
        assert!(body.contains("owner/repo"));
    }

    #[test]
    fn test_csv_escape_plain() {
        assert_eq!(csv_escape("simple"), "simple");
        assert_eq!(csv_escape(""), "");
    }

    #[test]
    fn test_csv_escape_comma() {
        assert_eq!(csv_escape("a,b"), "\"a,b\"");
    }

    #[test]
    fn test_csv_escape_quote() {
        assert_eq!(csv_escape("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn test_csv_escape_newline() {
        assert_eq!(csv_escape("a\nb"), "\"a\nb\"");
    }

    #[test]
    fn test_projects_csv_header() {
        let csv = projects_to_csv(&[]);
        assert!(csv.starts_with("slug,status,pinned,"));
        assert_eq!(csv.lines().count(), 1);
    }

    #[test]
    fn test_projects_csv_row() {
        let row = ProjectExportRow {
            slug: "owner/repo".into(),
            status: "active".into(),
            pinned: true,
            hub_sha: Some("abc123".into()),
            hub_fetched_at: Some("2026-04-20T00:00:00Z".into()),
            last_activity_at: None,
            added_at: "2026-04-20T00:00:00Z".into(),
            open_issues: 3,
            overdue_issues: 1,
            due_soon_issues: 0,
            blocked_issues: 0,
            active_agents: 2,
            stale_locks: 0,
            ci_status: Some("passing".into()),
            counters_updated_at: Some("2026-04-20T00:01:00Z".into()),
        };
        let csv = projects_to_csv(std::slice::from_ref(&row));
        let line = csv.lines().nth(1).expect("row present");
        assert!(line.starts_with("owner/repo,active,true,abc123,"));
        assert!(line.contains(",3,1,0,0,2,0,passing,"));
    }

    #[test]
    fn test_alerts_csv_row_escapes_detail_with_comma() {
        let row = AlertExportRow {
            id: 42,
            project_slug: "owner/repo".into(),
            kind: "ci_failure".into(),
            severity: "critical".into(),
            subject_ref: None,
            detail: Some("build, test failing".into()),
            opened_at: "2026-04-20T00:00:00Z".into(),
            resolved_at: None,
            acknowledged_at: None,
        };
        let csv = alerts_to_csv(std::slice::from_ref(&row));
        let line = csv.lines().nth(1).expect("row present");
        assert!(line.contains("\"build, test failing\""));
    }
}
