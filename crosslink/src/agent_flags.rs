//! Local control flags an agent loop consults between ticks.
//!
//! The git-native agent request protocol (design doc §9) translates
//! remote requests into local flag files under `.crosslink/agent-flags/`.
//! These are machine-local, never pushed — each agent's flag directory
//! is scoped to its own working tree.
//!
//! Flag files:
//! - `paused`   — presence = paused; absence = running
//! - `kill`     — presence = "exit after current tool use"
//! - `reprioritise.json` — presence = "try this issue next" hint
//!
//! Long-running agent loops (Claude Code sessions, kickoff children,
//! swarm supervisors) call [`is_paused`], [`should_exit`], and
//! [`read_reprioritise_hint`] to react to these flags. The poll loop
//! in [`crate::agent_requests::poll`] writes them when requests arrive.
//!
//! Flags are plain filesystem existence checks so they're cheap and
//! tolerant to concurrent writes. No schema, no locking.
//!
//! Consumers in this PR:
//! - `crosslink agent flags [--strict]` — operator visibility +
//!   `PreToolUse` hook integration (`--strict` exits 2 when paused/kill
//!   so `.claude/hooks/work-check.py` can block tool use cleanly).
//! - `crate::agent_requests::poll` — applies remote requests by
//!   writing these flags.
//!
//! `clear_reprioritise_hint` is a public utility for agent-loop
//! integrations that want to acknowledge they've consumed the hint.
//! It's currently unused inside the crate so we suppress the warning
//! at the function level rather than the whole module.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Directory under `.crosslink/` that holds per-agent flag files.
/// Kept local (never pushed) because the flags encode *this machine's*
/// response to remote requests, not state that should propagate.
pub const FLAG_DIR: &str = "agent-flags";

const PAUSED: &str = "paused";
const KILL: &str = "kill";
const REPRIORITISE: &str = "reprioritise.json";

/// Hint surfaced by a `reprioritise` request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReprioritiseHint {
    pub issue_id: i64,
    /// Ulid of the request that produced the hint — lets the agent
    /// loop dedupe if the same hint is applied twice.
    pub from_request_id: String,
}

fn flag_dir(crosslink_dir: &Path) -> PathBuf {
    crosslink_dir.join(FLAG_DIR)
}

fn ensure_flag_dir(crosslink_dir: &Path) -> Result<PathBuf> {
    let dir = flag_dir(crosslink_dir);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create agent-flags dir {}", dir.display()))?;
    Ok(dir)
}

/// True when a `pause` request has landed and not been cleared.
#[must_use]
pub fn is_paused(crosslink_dir: &Path) -> bool {
    flag_dir(crosslink_dir).join(PAUSED).exists()
}

/// True when a `kill` request has landed. Long-running loops should
/// check this between tool invocations and exit cleanly.
#[must_use]
pub fn should_exit(crosslink_dir: &Path) -> bool {
    flag_dir(crosslink_dir).join(KILL).exists()
}

/// Latest reprioritise hint, if any. Agents decide whether to honour it.
///
/// # Errors
/// Propagates filesystem / JSON errors when the hint file exists but
/// can't be read or parsed.
pub fn read_reprioritise_hint(crosslink_dir: &Path) -> Result<Option<ReprioritiseHint>> {
    let path = flag_dir(crosslink_dir).join(REPRIORITISE);
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let hint: ReprioritiseHint =
        serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
    Ok(Some(hint))
}

/// Record a pause. Idempotent.
///
/// # Errors
/// Returns an error if the flag directory cannot be created or written.
pub fn set_paused(crosslink_dir: &Path) -> Result<()> {
    let dir = ensure_flag_dir(crosslink_dir)?;
    std::fs::write(dir.join(PAUSED), b"").with_context(|| format!("write {PAUSED} flag"))?;
    Ok(())
}

/// Clear a pause. Idempotent — missing flag is not an error.
///
/// # Errors
/// Returns an error only if the flag exists and can't be removed.
pub fn clear_paused(crosslink_dir: &Path) -> Result<()> {
    let path = flag_dir(crosslink_dir).join(PAUSED);
    if path.exists() {
        std::fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
    }
    Ok(())
}

/// Record a kill request. Idempotent.
///
/// # Errors
/// Returns an error if the flag directory cannot be created or written.
pub fn set_kill(crosslink_dir: &Path) -> Result<()> {
    let dir = ensure_flag_dir(crosslink_dir)?;
    std::fs::write(dir.join(KILL), b"").with_context(|| format!("write {KILL} flag"))?;
    Ok(())
}

/// Write a reprioritise hint, replacing any existing one.
///
/// # Errors
/// Returns an error if the hint can't be serialized or written.
pub fn set_reprioritise_hint(crosslink_dir: &Path, hint: &ReprioritiseHint) -> Result<()> {
    let dir = ensure_flag_dir(crosslink_dir)?;
    let body = serde_json::to_vec_pretty(hint).context("serialize reprioritise hint")?;
    std::fs::write(dir.join(REPRIORITISE), body)
        .with_context(|| format!("write {REPRIORITISE} hint"))?;
    Ok(())
}

/// Clear a reprioritise hint (e.g., after the agent loop acts on it).
///
/// # Errors
/// Returns an error only if the hint exists and can't be removed.
#[allow(dead_code)] // Consumed by external agent-loop integrations.
pub fn clear_reprioritise_hint(crosslink_dir: &Path) -> Result<()> {
    let path = flag_dir(crosslink_dir).join(REPRIORITISE);
    if path.exists() {
        std::fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_pause_lifecycle() {
        let dir = tempdir().unwrap();
        assert!(!is_paused(dir.path()));
        set_paused(dir.path()).unwrap();
        assert!(is_paused(dir.path()));
        // Idempotent.
        set_paused(dir.path()).unwrap();
        assert!(is_paused(dir.path()));
        clear_paused(dir.path()).unwrap();
        assert!(!is_paused(dir.path()));
        // Clearing a missing flag is a no-op.
        clear_paused(dir.path()).unwrap();
    }

    #[test]
    fn test_kill_flag() {
        let dir = tempdir().unwrap();
        assert!(!should_exit(dir.path()));
        set_kill(dir.path()).unwrap();
        assert!(should_exit(dir.path()));
    }

    #[test]
    fn test_reprioritise_hint_roundtrip() {
        let dir = tempdir().unwrap();
        assert!(read_reprioritise_hint(dir.path()).unwrap().is_none());

        let hint = ReprioritiseHint {
            issue_id: 42,
            from_request_id: "01HXY000000000000000000001".into(),
        };
        set_reprioritise_hint(dir.path(), &hint).unwrap();
        let loaded = read_reprioritise_hint(dir.path()).unwrap().unwrap();
        assert_eq!(loaded, hint);

        clear_reprioritise_hint(dir.path()).unwrap();
        assert!(read_reprioritise_hint(dir.path()).unwrap().is_none());
    }
}
