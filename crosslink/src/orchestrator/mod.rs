//! Orchestrator module — LLM-assisted decomposition and DAG-based execution.
//!
//! This module provides:
//! - [`models`] — domain types for plans, phases, stages, tasks
//! - [`decompose`] — LLM-assisted document decomposition via `claude` CLI
//! - [`dag`] — directed acyclic graph with topological sort and ready-node detection
//! - [`executor`] — execution lifecycle management with kickoff integration

pub mod dag;
pub mod decompose;
pub mod executor;
pub mod models;
