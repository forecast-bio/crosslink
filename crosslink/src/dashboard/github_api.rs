//! GitHub integration REST surface (design doc §14 Phase 4).
//!
//! Token management + org repo enumeration. Token is stored encrypted
//! in the dashboard DB; see [`super::github`] for the storage layer.
//!
//! Endpoints:
//! - `GET  /api/v1/dashboard/github/config` — token-present flag +
//!   default org (never returns the raw token).
//! - `POST /api/v1/dashboard/github/config` — set token and/or default
//!   org. Empty string for `token` deletes the stored secret.
//! - `GET  /api/v1/dashboard/github/orgs/{org}/repos` — enumerate
//!   crosslink-touched repos in `org` (has a `crosslink/hub` branch).
//! - `POST /api/v1/dashboard/github/orgs/{org}/track-all` — walk the
//!   org, clone+track every repo that already has a hub branch.

use anyhow::Result;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use super::db::DashboardDb;
use super::github;
use crate::server::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/github/config", get(get_config).post(set_config))
        .route("/github/orgs/{org}/repos", get(list_repos))
        .route("/github/orgs/{org}/track-all", post(track_all))
}

#[derive(Debug, Serialize)]
struct ConfigView {
    token_present: bool,
    /// First 8 + last 4 chars of the stored token (masked) so the UI
    /// can show *which* token is configured without revealing it.
    token_fingerprint: Option<String>,
    default_org: Option<String>,
    /// Where the effective token comes from:
    /// - `"stored"` — encrypted PAT in the dashboard DB
    /// - `"gh-cli"` — falling back to `gh auth token`
    /// - `null`     — no token available from either source
    token_source: Option<&'static str>,
}

const fn source_tag(s: github::TokenSource) -> &'static str {
    match s {
        github::TokenSource::Stored => "stored",
        github::TokenSource::GhCli => "gh-cli",
    }
}

// The nested Option<Option<String>> on default_org is load-bearing
// PATCH semantics: absent field = "no change", `null` = "clear org",
// string value = "set org". Flattening it to Option<String> loses the
// "no change" case.
#[allow(clippy::option_option)]
#[derive(Debug, Deserialize)]
struct SetConfigBody {
    /// `Some("")` deletes the stored token; `None` leaves it unchanged.
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    default_org: Option<Option<String>>,
}

async fn get_config(State(state): State<AppState>) -> Result<Json<ConfigView>, GitHubApiError> {
    let db_path = require_db_path(&state)?;
    let view = tokio::task::spawn_blocking(move || -> Result<ConfigView, GitHubApiError> {
        let db = DashboardDb::open(&db_path)
            .map_err(|e| GitHubApiError::Internal(format!("open db: {e}")))?;
        let effective = github::get_effective_token(&db, &db_path)
            .map_err(|e| GitHubApiError::Internal(format!("read token: {e}")))?;
        let default_org = github::get_plain(&db, github::KEY_DEFAULT_ORG)
            .map_err(|e| GitHubApiError::Internal(format!("read org: {e}")))?;
        Ok(match effective {
            Some((tok, src)) => ConfigView {
                token_present: true,
                token_fingerprint: Some(mask_token(&tok)),
                default_org,
                token_source: Some(source_tag(src)),
            },
            None => ConfigView {
                token_present: false,
                token_fingerprint: None,
                default_org,
                token_source: None,
            },
        })
    })
    .await
    .map_err(|e| GitHubApiError::Internal(format!("task panicked: {e}")))??;
    Ok(Json(view))
}

async fn set_config(
    State(state): State<AppState>,
    Json(body): Json<SetConfigBody>,
) -> Result<Json<ConfigView>, GitHubApiError> {
    let db_path = require_db_path(&state)?;

    // Minimum-viable token shape check — github PATs are ~40-ish
    // base36 chars starting with `ghp_` / `github_pat_`. We don't
    // reject exotic tokens (enterprise, SSO) — just catch obvious
    // paste errors.
    if let Some(ref t) = body.token {
        if !t.is_empty() && t.len() < 10 {
            return Err(GitHubApiError::BadRequest(
                "token looks too short — paste the full PAT".into(),
            ));
        }
    }

    let view = tokio::task::spawn_blocking(move || -> Result<ConfigView, GitHubApiError> {
        let db = DashboardDb::open(&db_path)
            .map_err(|e| GitHubApiError::Internal(format!("open db: {e}")))?;
        if let Some(t) = body.token.as_deref() {
            github::set_token(&db, t, &db_path)
                .map_err(|e| GitHubApiError::Internal(format!("set token: {e}")))?;
        }
        if let Some(org_change) = body.default_org {
            github::set_plain(&db, github::KEY_DEFAULT_ORG, org_change.as_deref())
                .map_err(|e| GitHubApiError::Internal(format!("set org: {e}")))?;
        }
        let effective = github::get_effective_token(&db, &db_path)
            .map_err(|e| GitHubApiError::Internal(format!("read token: {e}")))?;
        let default_org = github::get_plain(&db, github::KEY_DEFAULT_ORG)
            .map_err(|e| GitHubApiError::Internal(format!("read org: {e}")))?;
        Ok(match effective {
            Some((tok, src)) => ConfigView {
                token_present: true,
                token_fingerprint: Some(mask_token(&tok)),
                default_org,
                token_source: Some(source_tag(src)),
            },
            None => ConfigView {
                token_present: false,
                token_fingerprint: None,
                default_org,
                token_source: None,
            },
        })
    })
    .await
    .map_err(|e| GitHubApiError::Internal(format!("task panicked: {e}")))??;
    Ok(Json(view))
}

/// Show a non-reversible hint of the token: first 8 + "…" + last 4.
/// GitHub PATs aren't predictable from the prefix, so this is safe to
/// display on screen.
fn mask_token(t: &str) -> String {
    if t.len() <= 12 {
        return "*".repeat(t.len());
    }
    let (head, tail) = t.split_at(8);
    let tail_start = tail.len().saturating_sub(4);
    format!("{head}…{}", &tail[tail_start..])
}

#[derive(Debug, Serialize)]
struct RepoHit {
    owner: String,
    repo: String,
    full_name: String,
    default_branch: String,
    ssh_url: String,
    https_url: String,
    /// True if the repo has a `crosslink/hub` branch (our criterion
    /// for "crosslink-touched"). Non-matching repos are filtered out
    /// server-side; this field is always `true` in responses.
    has_hub_branch: bool,
}

async fn list_repos(
    State(state): State<AppState>,
    Path(org): Path<String>,
) -> Result<Json<Vec<RepoHit>>, GitHubApiError> {
    let db_path = require_db_path(&state)?;
    let token = load_token(&db_path).await?;

    let hits = enumerate_org_crosslink_repos(&org, &token)
        .await
        .map_err(|e| GitHubApiError::Upstream(e.to_string()))?;
    Ok(Json(hits))
}

#[derive(Debug, Deserialize)]
struct TrackAllBody {
    /// Root under which to clone new repos. Defaults to `$HOME`, so
    /// each repo lands at `~/<repo>` (flat, matching the user's
    /// manual-clone layout — `~/crosslink`, `~/ferrotorch`, not
    /// `~/<owner>/<repo>`). Existing clones matching the repo's
    /// slug are adopted in-place.
    #[serde(default)]
    clone_root: Option<String>,
    /// When `true`, run `crosslink init --defaults` and
    /// `crosslink agent init <agent_id>` in each freshly-cloned repo
    /// so dashboard write actions (close issue, release lock, etc.)
    /// work without manual bootstrap. Default `false`.
    #[serde(default)]
    init: bool,
    /// Agent identifier for `crosslink agent init`. Required when
    /// `init == true` and a PAT-only track-all is running.
    #[serde(default)]
    agent_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct TrackAllOutcome {
    tracked: Vec<String>,
    skipped: Vec<SkippedRepo>,
}

#[derive(Debug, Serialize)]
struct SkippedRepo {
    slug: String,
    reason: String,
}

async fn track_all(
    State(state): State<AppState>,
    Path(org): Path<String>,
    Json(body): Json<TrackAllBody>,
) -> Result<Json<TrackAllOutcome>, GitHubApiError> {
    let db_path = require_db_path(&state)?;
    let token = load_token(&db_path).await?;

    let hits = enumerate_org_crosslink_repos(&org, &token)
        .await
        .map_err(|e| GitHubApiError::Upstream(e.to_string()))?;

    // Default to `$HOME` so each repo lands at `~/<owner>/<repo>`
    // alongside the user's other clones. The dashboard's
    // filesystem-discover walker already scans `$HOME` for repos
    // with `.crosslink/`, so this layout gets picked up natively
    // without needing to special-case a "tracked" subdir.
    let clone_root = body
        .clone_root
        .clone()
        .unwrap_or_else(|| std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()));

    // Up-front validation: if init was requested, we need an agent_id
    // before we start any clones — rejecting here saves the user from
    // partially-initialised state.
    let init_config = if body.init {
        let id = body
            .agent_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                GitHubApiError::BadRequest(
                    "init=true requires agent_id (alphanumeric + hyphens + underscores)".into(),
                )
            })?;
        Some(id.to_string())
    } else {
        None
    };

    let mut tracked = Vec::new();
    let mut skipped = Vec::new();
    for hit in hits {
        let slug = hit.full_name.clone();
        // Clone directly into <clone_root>/<repo> — same flat shape
        // the user already keeps manual clones in (`~/crosslink`,
        // `~/ferrotorch`, etc.). If two orgs happen to share a repo
        // name, `ensure_clone_and_track` will either adopt the
        // existing clone in-place (`--slug` override resolves the
        // collision) or surface the already-tracked error; either
        // way we don't silently park one of them under a sub-path.
        let target = std::path::PathBuf::from(&clone_root).join(&hit.repo);
        let result = tokio::task::spawn_blocking({
            let db_path = db_path.clone();
            let target = target.clone();
            let ssh_url = hit.ssh_url.clone();
            let https_url = hit.https_url.clone();
            let slug = slug.clone();
            let init = init_config.clone();
            move || {
                ensure_clone_and_track(
                    &db_path,
                    &target,
                    &ssh_url,
                    &https_url,
                    &slug,
                    init.as_deref(),
                )
            }
        })
        .await
        .map_err(|e| GitHubApiError::Internal(format!("track task panicked: {e}")))?;
        match result {
            Ok(()) => tracked.push(slug),
            Err(e) => skipped.push(SkippedRepo {
                slug,
                reason: e.to_string(),
            }),
        }
    }

    Ok(Json(TrackAllOutcome { tracked, skipped }))
}

/// Clone `ssh_url` (falling back to `https_url`) into `target` if the
/// dir doesn't already exist, then register it in the dashboard DB.
/// Idempotent — already-tracked slugs are left alone and surface as
/// "already tracked".
///
/// When `init_agent_id` is `Some`, also run `crosslink init --defaults`
/// and `crosslink agent init <id>` in the freshly-cloned workspace so
/// dashboard write actions work without a manual bootstrap step. If
/// init fails, we still register the project — the tile just surfaces
/// as `write_capability: "not_initialized"` / `"agent_missing"` and
/// the operator can retry via the retrofit endpoint.
fn ensure_clone_and_track(
    db_path: &std::path::Path,
    target: &std::path::Path,
    ssh_url: &str,
    https_url: &str,
    slug: &str,
    init_agent_id: Option<&str>,
) -> Result<()> {
    let freshly_cloned = !target.is_dir();
    if freshly_cloned {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let clone_res = std::process::Command::new("git")
            .args([
                "clone",
                "--quiet",
                ssh_url,
                target.to_string_lossy().as_ref(),
            ])
            .status();
        let cloned = matches!(clone_res, Ok(s) if s.success());
        if !cloned {
            // Retry via HTTPS — common in CI where SSH isn't set up.
            let https = std::process::Command::new("git")
                .args([
                    "clone",
                    "--quiet",
                    https_url,
                    target.to_string_lossy().as_ref(),
                ])
                .status()?;
            anyhow::ensure!(https.success(), "git clone failed for {slug}");
        }
    }

    // Run init + agent init BEFORE track_at_path so the `write_capability`
    // reflected in the very first tile render is correct. The helper
    // is idempotent-on-Ready (skips if .crosslink/ + driver-key.pub
    // are already in place).
    if let Some(agent_id) = init_agent_id {
        if super::projects::write_capability(target) != super::projects::WriteCapability::Ready {
            super::projects::run_init_and_agent_in(target, agent_id)?;
        }
    }

    super::projects::track_at_path(target, Some(slug), db_path)?;
    Ok(())
}

fn require_db_path(state: &AppState) -> Result<std::path::PathBuf, GitHubApiError> {
    state
        .dashboard_db_path
        .clone()
        .ok_or_else(|| GitHubApiError::BadRequest("dashboard DB not configured".into()))
}

async fn load_token(db_path: &std::path::Path) -> Result<String, GitHubApiError> {
    let db_path_owned = db_path.to_path_buf();
    let resolved = tokio::task::spawn_blocking(move || {
        let db = DashboardDb::open(&db_path_owned).ok()?;
        github::get_effective_token(&db, &db_path_owned)
            .ok()
            .flatten()
    })
    .await
    .map_err(|e| GitHubApiError::Internal(format!("load token task panicked: {e}")))?;
    resolved.map(|(tok, _src)| tok).ok_or_else(|| {
        GitHubApiError::BadRequest(
            "no GitHub token available — store a PAT via POST /github/config, \
             or run `gh auth login` in a shell"
                .into(),
        )
    })
}

/// Hit the GitHub API to list repos in `org` and keep only those with
/// a `crosslink/hub` branch. Uses the v3 REST API with per-page=100
/// pagination and a short circuit: checks the branch via the
/// lightweight `/branches/<name>` endpoint (404 = no hub).
async fn enumerate_org_crosslink_repos(org: &str, token: &str) -> Result<Vec<RepoHit>> {
    let client = reqwest::Client::builder()
        .user_agent("crosslink-dashboard")
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let mut out = Vec::new();
    let mut page = 1u32;
    loop {
        let url =
            format!("https://api.github.com/orgs/{org}/repos?per_page=100&page={page}&type=all");
        let resp = client
            .get(&url)
            .bearer_auth(token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            anyhow::bail!("GitHub API returned 401 — token invalid or lacks org access");
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitHub API {status}: {}", body.trim());
        }
        let repos: Vec<RepoListItem> = resp.json().await?;
        if repos.is_empty() {
            break;
        }
        for repo in &repos {
            // Check for crosslink/hub cheaply — a 200 means yes, 404
            // means no, anything else we propagate.
            let check_url = format!(
                "https://api.github.com/repos/{}/{}/branches/crosslink%2Fhub",
                repo.owner.login, repo.name
            );
            let check = client
                .get(&check_url)
                .bearer_auth(token)
                .header("Accept", "application/vnd.github+json")
                .send()
                .await?;
            if check.status().is_success() {
                out.push(RepoHit {
                    owner: repo.owner.login.clone(),
                    repo: repo.name.clone(),
                    full_name: repo.full_name.clone(),
                    default_branch: repo.default_branch.clone(),
                    ssh_url: repo.ssh_url.clone(),
                    https_url: repo.clone_url.clone(),
                    has_hub_branch: true,
                });
            }
        }
        if repos.len() < 100 {
            break;
        }
        page += 1;
    }
    Ok(out)
}

#[derive(Debug, Deserialize)]
struct RepoListItem {
    name: String,
    full_name: String,
    default_branch: String,
    ssh_url: String,
    clone_url: String,
    owner: RepoOwner,
}

#[derive(Debug, Deserialize)]
struct RepoOwner {
    login: String,
}

#[derive(Debug)]
enum GitHubApiError {
    BadRequest(String),
    Upstream(String),
    Internal(String),
}

impl IntoResponse for GitHubApiError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            Self::Upstream(m) => (StatusCode::BAD_GATEWAY, m),
            Self::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
        };
        (status, Json(serde_json::json!({"error": msg}))).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mask_token_short() {
        assert_eq!(mask_token(""), "");
        assert_eq!(mask_token("abc"), "***");
        assert_eq!(mask_token("0123456789ab"), "************");
    }

    #[test]
    fn test_mask_token_realistic() {
        let s = mask_token("ghp_1234567890abcdefghij");
        assert!(s.starts_with("ghp_1234"));
        assert!(s.ends_with("ghij"));
        assert!(s.contains('…'));
    }
}
