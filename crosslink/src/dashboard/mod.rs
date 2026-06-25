//! `crosslink dashboard` — multi-project SCADA-style control panel.
//!
//! See `DESIGN-CROSSLINK-DASHBOARD.md` at the repo root for the full
//! architectural spec. This module implements the aggregator service
//! that the `crosslink dashboard` subcommand runs.
//!
//! Layers (populated incrementally across the GH #689 subissue stack):
//!
//! - `db` — per-user `SQLite` index (`~/.crosslink/dashboard.db`)
//! - (P1.2) poll loop + project index population
//! - (P1.3) REST API
//! - (P1.5) WebSocket
//! - (P1.6) alert engine
//! - (P1.8+) write surface
//!
//! This module is separate from the main crosslink `db`
//! ([`crate::db`]): the dashboard DB lives in the user's home
//! directory and tracks cross-repo state, while the main crosslink DB
//! lives inside each project's `.crosslink/` and tracks that project's
//! issues.

pub mod actions;
pub mod alerts;
pub mod alerts_db;
pub mod api;
pub mod db;
pub mod discover;
pub mod export;
pub mod github;
pub mod github_api;
pub mod poll;
pub mod projects;
pub mod pty;
pub mod pty_api;
pub mod reader;
pub mod webhook;
pub mod webhook_api;
