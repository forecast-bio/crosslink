//! Write-side operations for multi-agent shared issue tracking.
//!
//! `SharedWriter` wraps a `SyncManager` and `AgentConfig` to provide
//! write operations that persist issue data as JSON on the coordination
//! branch and then update local SQLite. In single-agent mode (no
//! `agent.json`), `SharedWriter::new()` returns `None` and all commands
//! fall back to direct `Database` writes.

pub(crate) mod core;
mod locks;
mod milestones;
mod mutations;
mod offline;

#[cfg(test)]
mod tests;

// Re-export public API at the module level so external callers
// continue to use `crate::shared_writer::SharedWriter`, etc.
pub use self::core::SharedWriter;
pub use locks::LockClaimResult;

#[allow(unused_imports)]
pub use offline::RewriteStats;
