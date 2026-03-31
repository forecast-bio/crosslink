//! Smoke tests for the crosslink HTTP server API and basic WebSocket connectivity.
//!
//! Each test spawns a fresh `crosslink serve` instance via the `SmokeHarness`
//! and exercises the REST API using raw HTTP/1.1 requests over `TcpStream`.
//! This avoids adding HTTP client dependencies while still testing the real
//! server binary end-to-end.

#[allow(unused_imports)]
use super::harness::{assert_stdout_contains, SmokeHarness};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

// ---------------------------------------------------------------------------
// HTTP helper
// ---------------------------------------------------------------------------

/// Send a raw HTTP/1.1 request and return (status_code, body_string).
///
/// Uses `Connection: close` so the server closes the socket after responding,
/// which avoids having to handle chunked transfer-encoding or keep-alive.
fn http_request(port: u16, method: &str, path: &str, body: Option<&str>) -> (u16, String) {
    http_request_with_auth(port, method, path, body, None)
}

fn authed_request(h: &SmokeHarness, method: &str, path: &str, body: Option<&str>) -> (u16, String) {
    http_request_with_auth(
        h.server_port.expect("server not started"),
        method,
        path,
        body,
        h.auth_token.as_deref(),
    )
}

fn http_request_with_auth(
    port: u16,
    method: &str,
    path: &str,
    body: Option<&str>,
    auth_token: Option<&str>,
) -> (u16, String) {
    let mut stream =
        TcpStream::connect(format!("127.0.0.1:{}", port)).expect("Failed to connect to server");
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(5))).ok();

    let body_str = body.unwrap_or("");
    let auth_header = match auth_token {
        Some(token) => format!("Authorization: Bearer {token}\r\n"),
        None => String::new(),
    };
    let request = format!(
        "{method} {path} HTTP/1.1\r\n\
Host: 127.0.0.1:{port}\r\n\
Content-Type: application/json\r\n\
{auth_header}\
Content-Length: {len}\r\n\
Connection: close\r\n\
\r\n\
{body_str}",
        len = body_str.len()
    );
    stream
        .write_all(request.as_bytes())
        .expect("Failed to write request");

    let mut response = String::new();
    // read_to_string blocks until the server closes the connection (Connection: close).
    let _ = stream.read_to_string(&mut response);

    parse_http_response(&response)
}

/// Parse a raw HTTP response into (status_code, body).
///
/// Handles both regular and chunked transfer-encoding responses.
fn parse_http_response(raw: &str) -> (u16, String) {
    let status = raw
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);

    // The body starts after the first blank line (\r\n\r\n).
    let body = if let Some(idx) = raw.find("\r\n\r\n") {
        let after_headers = &raw[idx + 4..];
        // Check if response uses chunked transfer-encoding.
        let headers_lower = raw[..idx].to_lowercase();
        if headers_lower.contains("transfer-encoding: chunked") {
            decode_chunked(after_headers)
        } else {
            after_headers.to_string()
        }
    } else {
        String::new()
    };

    (status, body)
}

/// Decode a chunked transfer-encoding body.
fn decode_chunked(raw: &str) -> String {
    let mut result = String::new();
    let mut remaining = raw;

    while let Some(line_end) = remaining.find("\r\n") {
        // Each chunk starts with the hex size followed by \r\n.
        let size_str = remaining[..line_end].trim();
        let Ok(size) = usize::from_str_radix(size_str, 16) else {
            break;
        };
        if size == 0 {
            break; // Terminal chunk
        }
        let chunk_start = line_end + 2;
        let chunk_end = chunk_start + size;
        if chunk_end > remaining.len() {
            // Partial chunk — take what we have.
            result.push_str(&remaining[chunk_start..]);
            break;
        }
        result.push_str(&remaining[chunk_start..chunk_end]);
        // Skip the trailing \r\n after the chunk data.
        remaining = if chunk_end + 2 <= remaining.len() {
            &remaining[chunk_end + 2..]
        } else {
            ""
        };
    }

    result
}

/// Parse the response body as JSON. Panics with a helpful message on failure.
fn parse_json(body: &str) -> serde_json::Value {
    serde_json::from_str(body).unwrap_or_else(|e| {
        panic!(
            "Failed to parse JSON: {}\nBody was: {:?}",
            e,
            &body[..body.len().min(500)]
        )
    })
}

// ===========================================================================
// Basic connectivity
// ===========================================================================

#[test]
fn test_server_starts_and_stops() {
    let mut h = SmokeHarness::new();
    let port = h.start_server();

    // Verify the port is listening by opening a TCP connection.
    let stream = TcpStream::connect(format!("127.0.0.1:{}", port));
    assert!(
        stream.is_ok(),
        "Server should be listening on port {}",
        port
    );
    drop(stream);

    h.stop_server();

    // After stop, the port should no longer be listening.
    // Give it a moment to fully shut down.
    std::thread::sleep(Duration::from_millis(200));
    let stream = TcpStream::connect_timeout(
        &format!("127.0.0.1:{}", port).parse().unwrap(),
        Duration::from_millis(500),
    );
    assert!(
        stream.is_err(),
        "Server should not be listening after stop_server()"
    );
}

#[test]
fn test_health_endpoint() {
    let mut h = SmokeHarness::new();
    let port = h.start_server();

    let (status, body) = http_request(port, "GET", "/api/v1/health", None);
    assert_eq!(status, 200, "Health endpoint should return 200");

    let json = parse_json(&body);
    assert_eq!(json["status"], "ok", "Health status should be 'ok'");
    assert!(
        json["version"].is_string(),
        "Health response should include version string"
    );
}

// ===========================================================================
// Issue CRUD via API
// ===========================================================================

#[test]
fn test_api_create_issue() {
    let mut h = SmokeHarness::new();
    let _port = h.start_server();

    let payload = r#"{"title": "Test issue via API", "priority": "high"}"#;
    let (status, body) = authed_request(&h, "POST", "/api/v1/issues", Some(payload));
    assert!(
        status == 200 || status == 201,
        "Create issue should return 200 or 201, got {}",
        status
    );

    let json = parse_json(&body);
    assert_eq!(json["title"], "Test issue via API");
    assert_eq!(json["priority"], "high");
    assert!(
        json["id"].as_i64().is_some(),
        "Response should include numeric id"
    );
    assert_eq!(json["status"], "open");
}

#[test]
fn test_api_get_issue() {
    let mut h = SmokeHarness::new();

    // Create an issue via CLI so we have something to fetch.
    h.run_ok(&["issue", "create", "CLI-created issue", "-p", "medium"]);

    let _port = h.start_server();

    let (status, body) = authed_request(&h, "GET", "/api/v1/issues/1", None);
    assert_eq!(status, 200, "GET issue should return 200");

    let json = parse_json(&body);
    assert_eq!(json["id"], 1);
    assert_eq!(json["title"], "CLI-created issue");
    assert_eq!(json["priority"], "medium");
    // IssueDetail includes labels, comments, blockers arrays.
    assert!(json["labels"].is_array(), "Should have labels array");
    assert!(json["comments"].is_array(), "Should have comments array");
    assert!(json["blockers"].is_array(), "Should have blockers array");
}

#[test]
fn test_api_list_issues() {
    let mut h = SmokeHarness::new();

    // Create 3 issues via CLI.
    h.run_ok(&["issue", "create", "Issue Alpha"]);
    h.run_ok(&["issue", "create", "Issue Beta"]);
    h.run_ok(&["issue", "create", "Issue Gamma"]);

    let _port = h.start_server();

    let (status, body) = authed_request(&h, "GET", "/api/v1/issues", None);
    assert_eq!(status, 200);

    let json = parse_json(&body);
    let items = json["items"].as_array().expect("items should be an array");
    assert_eq!(items.len(), 3, "Should have 3 issues");
    assert_eq!(json["total"], 3);

    // Verify all titles are present.
    let titles: Vec<&str> = items.iter().map(|i| i["title"].as_str().unwrap()).collect();
    assert!(titles.contains(&"Issue Alpha"));
    assert!(titles.contains(&"Issue Beta"));
    assert!(titles.contains(&"Issue Gamma"));
}

#[test]
fn test_api_update_issue() {
    let mut h = SmokeHarness::new();
    h.run_ok(&["issue", "create", "Original title"]);

    let _port = h.start_server();

    let payload = r#"{"title": "Updated title", "priority": "high"}"#;
    let (status, body) = authed_request(&h, "PATCH", "/api/v1/issues/1", Some(payload));
    assert_eq!(status, 200, "PATCH should return 200");

    let json = parse_json(&body);
    assert_eq!(json["title"], "Updated title");
    assert_eq!(json["priority"], "high");

    // Verify the update persisted by fetching the issue again.
    let (status2, body2) = authed_request(&h, "GET", "/api/v1/issues/1", None);
    assert_eq!(status2, 200);
    let json2 = parse_json(&body2);
    assert_eq!(json2["title"], "Updated title");
    assert_eq!(json2["priority"], "high");
}

#[test]
fn test_api_delete_issue() {
    let mut h = SmokeHarness::new();
    h.run_ok(&["issue", "create", "Doomed issue"]);

    let _port = h.start_server();

    // Verify it exists first.
    let (status, _) = authed_request(&h, "GET", "/api/v1/issues/1", None);
    assert_eq!(status, 200);

    // Delete it.
    let (status, body) = authed_request(&h, "DELETE", "/api/v1/issues/1", None);
    assert_eq!(status, 200, "DELETE should return 200");
    let json = parse_json(&body);
    assert_eq!(json["ok"], true);

    // Verify it's gone.
    let (status, _) = authed_request(&h, "GET", "/api/v1/issues/1", None);
    assert_eq!(status, 404, "Deleted issue should return 404");
}

#[test]
fn test_api_close_reopen() {
    let mut h = SmokeHarness::new();
    h.run_ok(&["issue", "create", "Close-reopen test"]);

    let _port = h.start_server();

    // Close the issue.
    let (status, body) = authed_request(&h, "POST", "/api/v1/issues/1/close", None);
    assert_eq!(status, 200, "Close should return 200");
    let json = parse_json(&body);
    assert_eq!(json["status"], "closed");

    // Verify via GET.
    let (_, body) = authed_request(&h, "GET", "/api/v1/issues/1", None);
    let json = parse_json(&body);
    assert_eq!(json["status"], "closed");

    // Reopen.
    let (status, body) = authed_request(&h, "POST", "/api/v1/issues/1/reopen", None);
    assert_eq!(status, 200, "Reopen should return 200");
    let json = parse_json(&body);
    assert_eq!(json["status"], "open");

    // Verify via GET again.
    let (_, body) = authed_request(&h, "GET", "/api/v1/issues/1", None);
    let json = parse_json(&body);
    assert_eq!(json["status"], "open");
}

// ===========================================================================
// Error paths
// ===========================================================================

#[test]
fn test_api_404_unknown() {
    let mut h = SmokeHarness::new();
    let _port = h.start_server();

    let (status, _) = authed_request(&h, "GET", "/api/v1/nonexistent", None);
    assert_eq!(
        status, 404,
        "Unknown API path should return 404, got {}",
        status
    );
}

#[test]
fn test_api_issue_not_found() {
    let mut h = SmokeHarness::new();
    let _port = h.start_server();

    let (status, body) = authed_request(&h, "GET", "/api/v1/issues/99999", None);
    assert_eq!(status, 404, "Non-existent issue should return 404");

    let json = parse_json(&body);
    assert_eq!(json["error"], "not found");
}

#[test]
fn test_api_invalid_json() {
    let mut h = SmokeHarness::new();
    let _port = h.start_server();

    // Send garbage JSON to the create issue endpoint.
    let (status, _) = authed_request(
        &h,
        "POST",
        "/api/v1/issues",
        Some("this is not valid json{{{"),
    );
    assert!(
        status == 400 || status == 422,
        "Invalid JSON should return 400 or 422, got {}",
        status
    );
}

// ===========================================================================
// Sessions
// ===========================================================================

#[test]
fn test_api_sessions() {
    let mut h = SmokeHarness::new();
    let _port = h.start_server();

    // Before starting a session, current session should be 404.
    let (status, _) = authed_request(&h, "GET", "/api/v1/sessions/current", None);
    assert_eq!(
        status, 404,
        "No session should exist initially, got {}",
        status
    );

    // Start a session.
    let (status, body) = authed_request(&h, "POST", "/api/v1/sessions/start", Some("{}"));
    assert_eq!(status, 200, "Start session should return 200");
    let json = parse_json(&body);
    assert!(json["id"].as_i64().is_some(), "Session should have an id");
    assert!(
        json["started_at"].is_string(),
        "Session should have started_at"
    );

    // Get current session.
    let (status, body) = authed_request(&h, "GET", "/api/v1/sessions/current", None);
    assert_eq!(status, 200, "Current session should now exist");
    let json = parse_json(&body);
    assert!(json["id"].as_i64().is_some());

    // End the session.
    let (status, body) = authed_request(
        &h,
        "POST",
        "/api/v1/sessions/end",
        Some(r#"{"notes": "smoke test done"}"#),
    );
    assert_eq!(status, 200, "End session should return 200");
    let json = parse_json(&body);
    assert_eq!(json["ok"], true);

    // After ending, current session should be 404 again.
    let (status, _) = authed_request(&h, "GET", "/api/v1/sessions/current", None);
    assert_eq!(
        status, 404,
        "After ending session, current should be 404, got {}",
        status
    );
}

// ===========================================================================
// Milestones
// ===========================================================================

#[test]
fn test_api_milestones() {
    let mut h = SmokeHarness::new();
    let _port = h.start_server();

    // List milestones (should be empty initially).
    let (status, body) = authed_request(&h, "GET", "/api/v1/milestones", None);
    assert_eq!(status, 200);
    let json = parse_json(&body);
    assert_eq!(json["total"], 0);

    // Create a milestone.
    let payload = r#"{"name": "v1.0", "description": "First release"}"#;
    let (status, body) = authed_request(&h, "POST", "/api/v1/milestones", Some(payload));
    assert_eq!(status, 200, "Create milestone should return 200");
    let created = parse_json(&body);
    assert_eq!(created["name"], "v1.0");
    assert_eq!(created["status"], "open");
    let ms_id = created["id"].as_i64().expect("Milestone should have id");

    // List milestones (should have 1).
    let (status, body) = authed_request(&h, "GET", "/api/v1/milestones", None);
    assert_eq!(status, 200);
    let json = parse_json(&body);
    assert_eq!(json["total"], 1);

    // Get by ID.
    let (status, body) = authed_request(&h, "GET", &format!("/api/v1/milestones/{}", ms_id), None);
    assert_eq!(status, 200);
    let json = parse_json(&body);
    assert_eq!(json["name"], "v1.0");
    assert_eq!(json["issue_count"], 0);
    assert_eq!(json["progress_percent"], 0.0);
}

// ===========================================================================
// Search
// ===========================================================================

#[test]
fn test_api_search() {
    let mut h = SmokeHarness::new();

    // Create issues with distinctive titles.
    h.run_ok(&["issue", "create", "Authentication bug fix"]);
    h.run_ok(&["issue", "create", "Dashboard layout update"]);
    h.run_ok(&["issue", "create", "Authentication refactor"]);

    let _port = h.start_server();

    // Search for "authentication" — should find 2 issues.
    let (status, body) = authed_request(&h, "GET", "/api/v1/search?q=authentication", None);
    assert_eq!(status, 200);
    let json = parse_json(&body);
    let total = json["total"].as_u64().unwrap_or(0);
    assert!(
        total >= 2,
        "Search for 'authentication' should find at least 2 results, got {}",
        total
    );

    let items = json["items"].as_array().expect("items should be an array");
    // All results should be issue-type.
    for item in items {
        if item["kind"] == "issue" {
            let title = item["title"].as_str().unwrap_or("");
            assert!(
                title.to_lowercase().contains("authentication"),
                "Issue result should match query: {}",
                title
            );
        }
    }
}

// ===========================================================================
// Config
// ===========================================================================

#[test]
fn test_api_config() {
    let mut h = SmokeHarness::new();
    let _port = h.start_server();

    let (status, body) = authed_request(&h, "GET", "/api/v1/config", None);
    assert_eq!(status, 200, "GET config should return 200");

    let json = parse_json(&body);
    // Config should include standard fields with defaults.
    assert!(
        json["tracking_mode"].is_string(),
        "Config should have tracking_mode"
    );
    assert!(json["remote"].is_string(), "Config should have remote");
    assert!(
        json.get("intervention_tracking").is_some(),
        "Config should have intervention_tracking"
    );
    assert!(
        json.get("auto_steal_stale_locks").is_some(),
        "Config should have auto_steal_stale_locks"
    );
    assert!(
        json.get("stale_lock_timeout_minutes").is_some(),
        "Config should have stale_lock_timeout_minutes"
    );
    assert!(
        json.get("signing_enforcement").is_some(),
        "Config should have signing_enforcement"
    );
}

// ===========================================================================
// Sync status
// ===========================================================================

#[test]
fn test_api_sync_status() {
    let mut h = SmokeHarness::new();
    let _port = h.start_server();

    let (status, body) = authed_request(&h, "GET", "/api/v1/sync/status", None);
    assert_eq!(status, 200, "GET sync/status should return 200");

    let json = parse_json(&body);
    // In a fresh test environment, hub is not initialized.
    assert!(
        json.get("hub_initialized").is_some(),
        "Should have hub_initialized field"
    );
    assert_eq!(
        json["hub_branch"], "crosslink/hub",
        "hub_branch should be crosslink/hub"
    );
    assert!(json.get("remote").is_some(), "Should have remote field");
    assert!(
        json.get("active_lock_count").is_some(),
        "Should have active_lock_count"
    );
    assert!(
        json.get("stale_lock_count").is_some(),
        "Should have stale_lock_count"
    );
}

// ===========================================================================
// WebSocket connectivity
// ===========================================================================

#[test]
fn test_ws_connects() {
    let mut h = SmokeHarness::new();
    let port = h.start_server();

    // Perform a WebSocket upgrade handshake using raw TCP.
    // We just verify the server responds with 101 Switching Protocols.
    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port))
        .expect("Failed to connect for WebSocket test");
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(5))).ok();

    // Send a WebSocket upgrade request per RFC 6455.
    let ws_request = format!(
        "GET /ws HTTP/1.1\r\n\
Host: 127.0.0.1:{port}\r\n\
Upgrade: websocket\r\n\
Connection: Upgrade\r\n\
Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
Sec-WebSocket-Version: 13\r\n\
\r\n"
    );
    stream
        .write_all(ws_request.as_bytes())
        .expect("Failed to send WebSocket upgrade request");

    // Read the response — we expect "HTTP/1.1 101 Switching Protocols".
    let mut buf = [0u8; 1024];
    let n = stream
        .read(&mut buf)
        .expect("Failed to read WebSocket upgrade response");
    let response = String::from_utf8_lossy(&buf[..n]);

    assert!(
        response.contains("101"),
        "WebSocket upgrade should return 101 Switching Protocols, got: {}",
        response.lines().next().unwrap_or("(empty)")
    );
    assert!(
        response.to_lowercase().contains("upgrade: websocket"),
        "Response should contain 'Upgrade: websocket' header"
    );
}

// ===========================================================================
// Additional issue API tests
// ===========================================================================

#[test]
fn test_api_create_issue_with_description() {
    let mut h = SmokeHarness::new();
    let _port = h.start_server();

    let payload =
        r#"{"title": "Described issue", "description": "This is the details", "priority": "low"}"#;
    let (status, body) = authed_request(&h, "POST", "/api/v1/issues", Some(payload));
    assert!(status == 200 || status == 201);

    let json = parse_json(&body);
    assert_eq!(json["title"], "Described issue");
    assert_eq!(json["description"], "This is the details");
    assert_eq!(json["priority"], "low");
}

#[test]
fn test_api_create_issue_default_priority() {
    let mut h = SmokeHarness::new();
    let _port = h.start_server();

    let payload = r#"{"title": "Default priority issue"}"#;
    let (status, body) = authed_request(&h, "POST", "/api/v1/issues", Some(payload));
    assert!(status == 200 || status == 201);

    let json = parse_json(&body);
    assert_eq!(
        json["priority"], "medium",
        "Default priority should be 'medium'"
    );
}

#[test]
fn test_api_update_nonexistent_issue() {
    let mut h = SmokeHarness::new();
    let _port = h.start_server();

    let payload = r#"{"title": "New title"}"#;
    let (status, _) = authed_request(&h, "PATCH", "/api/v1/issues/99999", Some(payload));
    assert_eq!(status, 404, "Updating non-existent issue should return 404");
}

#[test]
fn test_api_delete_nonexistent_issue() {
    let mut h = SmokeHarness::new();
    let _port = h.start_server();

    let (status, _) = authed_request(&h, "DELETE", "/api/v1/issues/99999", None);
    assert_eq!(status, 404, "Deleting non-existent issue should return 404");
}

#[test]
fn test_api_close_nonexistent_issue() {
    let mut h = SmokeHarness::new();
    let _port = h.start_server();

    let (status, _) = authed_request(&h, "POST", "/api/v1/issues/99999/close", None);
    assert_eq!(status, 404, "Closing non-existent issue should return 404");
}

#[test]
fn test_api_list_issues_empty() {
    let mut h = SmokeHarness::new();
    let _port = h.start_server();

    let (status, body) = authed_request(&h, "GET", "/api/v1/issues", None);
    assert_eq!(status, 200);

    let json = parse_json(&body);
    assert_eq!(json["total"], 0);
    assert!(json["items"].as_array().unwrap().is_empty());
}

#[test]
fn test_api_issues_blocked_and_ready() {
    let mut h = SmokeHarness::new();

    // Create two issues via CLI.
    h.run_ok(&["issue", "create", "Ready issue"]);
    h.run_ok(&["issue", "create", "Another ready issue"]);

    let _port = h.start_server();

    // Both should appear in the "ready" list (no blockers).
    let (status, body) = authed_request(&h, "GET", "/api/v1/issues/ready", None);
    assert_eq!(status, 200);
    let json = parse_json(&body);
    let total = json["total"].as_u64().unwrap_or(0);
    assert!(
        total >= 2,
        "Should have at least 2 ready issues, got {}",
        total
    );

    // Blocked list should be empty (no dependencies set).
    let (status, body) = authed_request(&h, "GET", "/api/v1/issues/blocked", None);
    assert_eq!(status, 200);
    let json = parse_json(&body);
    assert_eq!(json["total"], 0, "No issues should be blocked initially");
}

#[test]
fn test_api_milestone_not_found() {
    let mut h = SmokeHarness::new();
    let _port = h.start_server();

    let (status, body) = authed_request(&h, "GET", "/api/v1/milestones/99999", None);
    assert_eq!(status, 404);

    let json = parse_json(&body);
    assert_eq!(json["error"], "not found");
}

#[test]
fn test_api_search_empty_query() {
    let mut h = SmokeHarness::new();
    let _port = h.start_server();

    // Empty query should return 400.
    let (status, _) = authed_request(&h, "GET", "/api/v1/search?q=", None);
    assert_eq!(
        status, 400,
        "Empty search query should return 400, got {}",
        status
    );
}

#[test]
fn test_api_search_no_results() {
    let mut h = SmokeHarness::new();
    let _port = h.start_server();

    let (status, body) = authed_request(&h, "GET", "/api/v1/search?q=xyznonexistent", None);
    assert_eq!(status, 200);
    let json = parse_json(&body);
    assert_eq!(json["total"], 0);
}
