//! Directed acyclic graph for orchestration stage execution ordering.
//!
//! [`Dag`] holds stages as nodes and dependency edges between them. It provides:
//! - Topological sort (Kahn's algorithm)
//! - Ready-node detection (all predecessors completed)
//! - Cycle detection
//! - Progress tracking (mark nodes as running, done, failed, skipped)

use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::server::types::StageStatus;

/// A single node in the execution DAG, representing one orchestration stage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagNode {
    /// Unique stage identifier (e.g. "phase-1-agent-1a").
    pub id: String,
    /// Human-readable title.
    pub title: String,
    /// Current execution status.
    pub status: StageStatus,
    /// IDs of stages that must complete before this one can start.
    pub depends_on: Vec<String>,
    /// Crosslink issue ID created for this stage (set during execution setup).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_id: Option<i64>,
    /// Agent ID assigned to execute this stage (set when launched).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Phase this stage belongs to.
    pub phase_id: String,
}

/// Directed acyclic graph managing orchestration stage execution order.
///
/// Nodes are indexed by their string ID. Edges are stored as adjacency lists
/// in both directions (forward for dependents, reverse for dependencies).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dag {
    /// All nodes, keyed by stage ID.
    nodes: HashMap<String, DagNode>,
    /// Forward edges: `stage_id → set of stages that depend on it`.
    forward: HashMap<String, HashSet<String>>,
    /// Reverse edges: `stage_id → set of stages it depends on`.
    reverse: HashMap<String, HashSet<String>>,
    /// Cached topological sort result. Computed once after construction since
    /// the graph structure (nodes/edges) never changes — only statuses do (#485).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cached_topo_order: Option<Vec<String>>,
}

impl Dag {
    /// Create an empty DAG.
    #[must_use]
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            forward: HashMap::new(),
            reverse: HashMap::new(),
            cached_topo_order: None,
        }
    }

    /// Build a DAG from a list of nodes. Returns an error if any dependency
    /// references a node that doesn't exist, or if the graph contains a cycle.
    ///
    /// # Errors
    ///
    /// Returns an error if duplicate stage IDs exist, a dependency references
    /// a nonexistent node, or the graph contains a cycle.
    pub fn from_nodes(nodes: &[DagNode]) -> Result<Self> {
        let mut dag = Self::new();

        // Insert all nodes first so we can validate edges.
        for node in nodes {
            if dag.nodes.contains_key(&node.id) {
                bail!("Duplicate stage ID: {}", node.id);
            }
            dag.nodes.insert(node.id.clone(), node.clone());
            dag.forward.entry(node.id.clone()).or_default();
            dag.reverse.entry(node.id.clone()).or_default();
        }

        // Add edges.
        for node in nodes {
            for dep in &node.depends_on {
                if !dag.nodes.contains_key(dep) {
                    bail!(
                        "Stage '{}' depends on '{}' which does not exist",
                        node.id,
                        dep
                    );
                }
                dag.forward
                    .entry(dep.clone())
                    .or_default()
                    .insert(node.id.clone());
                dag.reverse
                    .entry(node.id.clone())
                    .or_default()
                    .insert(dep.clone());
            }
        }

        // Validate acyclicity and cache topological order (#485).
        let topo = dag.topological_sort()?;
        dag.cached_topo_order = Some(topo);

        Ok(dag)
    }

    /// Return a reference to the node with the given ID, if it exists.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&DagNode> {
        self.nodes.get(id)
    }

    /// Return a mutable reference to the node with the given ID.
    pub fn get_mut(&mut self, id: &str) -> Option<&mut DagNode> {
        self.nodes.get_mut(id)
    }

    /// Return all node IDs.
    #[must_use]
    pub fn node_ids(&self) -> Vec<String> {
        self.nodes.keys().cloned().collect()
    }

    /// Return all nodes.
    #[must_use]
    pub const fn nodes(&self) -> &HashMap<String, DagNode> {
        &self.nodes
    }

    /// Total number of stages.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the DAG is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Return stage IDs that are ready to execute: status is `Pending` and all
    /// dependencies have a terminal status (`Done` or `Skipped`).
    #[must_use]
    pub fn ready_nodes(&self) -> Vec<String> {
        self.nodes
            .iter()
            .filter(|(_, node)| node.status == StageStatus::Pending)
            .filter(|(id, _)| {
                self.reverse.get(*id).is_none_or(|deps| {
                    deps.iter().all(|dep_id| {
                        self.nodes.get(dep_id).is_some_and(|d| {
                            matches!(d.status, StageStatus::Done | StageStatus::Skipped)
                        })
                    })
                })
            })
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Return stage IDs that are currently running.
    #[must_use]
    pub fn running_nodes(&self) -> Vec<String> {
        self.nodes
            .iter()
            .filter(|(_, node)| node.status == StageStatus::Running)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Return stage IDs with the given status.
    #[must_use]
    pub fn nodes_with_status(&self, status: &StageStatus) -> Vec<String> {
        self.nodes
            .iter()
            .filter(|(_, node)| &node.status == status)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Mark a stage as running.
    ///
    /// # Errors
    ///
    /// Returns an error if the stage is not found or is not in `Pending` status.
    pub fn mark_running(&mut self, id: &str, agent_id: &str) -> Result<()> {
        let node = self
            .nodes
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("Stage '{id}' not found"))?;
        if node.status != StageStatus::Pending {
            bail!(
                "Cannot mark '{}' as running — current status is {:?}",
                id,
                node.status
            );
        }
        node.status = StageStatus::Running;
        node.agent_id = Some(agent_id.to_string());
        Ok(())
    }

    /// Mark a stage as done. Returns the list of stage IDs that are now
    /// newly unblocked (all their dependencies are done and they are pending).
    ///
    /// # Errors
    ///
    /// Returns an error if the stage is not found or is not in `Running` status.
    pub fn mark_done(&mut self, id: &str) -> Result<Vec<String>> {
        let node = self
            .nodes
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("Stage '{id}' not found"))?;
        if node.status != StageStatus::Running {
            bail!(
                "Cannot mark '{}' as done — current status is {:?}",
                id,
                node.status
            );
        }
        node.status = StageStatus::Done;

        Ok(self.find_newly_unblocked(id))
    }

    /// Find dependents of `id` that are now unblocked (all deps terminal and node pending).
    ///
    /// Shared by `mark_done` and `mark_skipped_and_unblock` to avoid duplicating
    /// the unblocking logic (#483).
    fn find_newly_unblocked(&self, id: &str) -> Vec<String> {
        let dependents = self.forward.get(id).cloned().unwrap_or_default();
        let mut newly_ready = Vec::new();
        for dep_id in dependents {
            if let Some(dep_node) = self.nodes.get(&dep_id) {
                if dep_node.status != StageStatus::Pending {
                    continue;
                }
                let all_deps_terminal = self.reverse.get(&dep_id).is_none_or(|deps| {
                    deps.iter().all(|d| {
                        self.nodes.get(d).is_some_and(|n| {
                            matches!(n.status, StageStatus::Done | StageStatus::Skipped)
                        })
                    })
                });
                if all_deps_terminal {
                    newly_ready.push(dep_id);
                }
            }
        }
        newly_ready
    }

    /// Mark a stage as skipped and return newly-unblocked dependents.
    ///
    /// Combines `mark_skipped` with the same unblocking logic used by `mark_done`
    /// so callers don't need to reimplement it (#483).
    ///
    /// # Errors
    ///
    /// Returns an error if the stage is not found or the status transition is invalid.
    pub fn mark_skipped_and_unblock(&mut self, id: &str) -> Result<Vec<String>> {
        self.mark_skipped(id)?;
        Ok(self.find_newly_unblocked(id))
    }

    /// Mark a stage as failed. Valid from `Pending` or `Running`.
    ///
    /// # Errors
    ///
    /// Returns an error if the stage is not found or is not in `Pending`/`Running` status.
    pub fn mark_failed(&mut self, id: &str) -> Result<()> {
        let node = self
            .nodes
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("Stage '{id}' not found"))?;
        if !matches!(node.status, StageStatus::Pending | StageStatus::Running) {
            bail!(
                "Cannot mark '{}' as failed — current status is {:?}, must be Pending or Running",
                id,
                node.status
            );
        }
        node.status = StageStatus::Failed;
        Ok(())
    }

    /// Mark a stage as skipped. Valid from `Pending` or `Failed`.
    ///
    /// # Errors
    ///
    /// Returns an error if the stage is not found or is not in `Pending`/`Failed` status.
    pub fn mark_skipped(&mut self, id: &str) -> Result<()> {
        let node = self
            .nodes
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("Stage '{id}' not found"))?;
        if !matches!(node.status, StageStatus::Pending | StageStatus::Failed) {
            bail!(
                "Cannot mark '{}' as skipped — current status is {:?}, must be Pending or Failed",
                id,
                node.status
            );
        }
        node.status = StageStatus::Skipped;
        Ok(())
    }

    /// Assign a crosslink issue ID to a stage.
    ///
    /// # Errors
    ///
    /// Returns an error if the stage is not found.
    pub fn set_issue_id(&mut self, stage_id: &str, issue_id: i64) -> Result<()> {
        let node = self
            .nodes
            .get_mut(stage_id)
            .ok_or_else(|| anyhow::anyhow!("Stage '{stage_id}' not found"))?;
        node.issue_id = Some(issue_id);
        Ok(())
    }

    /// Produce a topological ordering of all stages (Kahn's algorithm).
    /// Returns an error if the graph has a cycle.
    ///
    /// # Errors
    ///
    /// Returns an error if the graph contains a cycle.
    pub fn topological_sort(&self) -> Result<Vec<String>> {
        let mut in_degree: HashMap<&str, usize> = HashMap::new();
        for id in self.nodes.keys() {
            in_degree.insert(id.as_str(), 0);
        }
        // In-degree = number of dependencies (reverse edges).
        for (id, deps) in &self.reverse {
            *in_degree.entry(id.as_str()).or_insert(0) = deps.len();
        }

        let mut queue: VecDeque<String> = VecDeque::new();
        for (id, &deg) in &in_degree {
            if deg == 0 {
                queue.push_back(id.to_string());
            }
        }

        // Sort the initial queue for deterministic output.
        let mut sorted_start: Vec<String> = queue.into_iter().collect();
        sorted_start.sort();
        let mut queue: VecDeque<String> = sorted_start.into_iter().collect();

        let mut order = Vec::with_capacity(self.nodes.len());
        while let Some(id) = queue.pop_front() {
            order.push(id.clone());
            if let Some(dependents) = self.forward.get(&id) {
                let mut sorted_deps: Vec<&String> = dependents.iter().collect();
                sorted_deps.sort();
                for dep_id in sorted_deps {
                    if let Some(deg) = in_degree.get_mut(dep_id.as_str()) {
                        *deg -= 1;
                        if *deg == 0 {
                            queue.push_back(dep_id.clone());
                        }
                    }
                }
            }
        }

        if order.len() != self.nodes.len() {
            bail!(
                "Cycle detected: topological sort produced {} of {} nodes",
                order.len(),
                self.nodes.len()
            );
        }

        Ok(order)
    }

    /// Check whether the graph contains a cycle (DFS-based).
    #[cfg(test)]
    pub(crate) fn has_cycle(&self) -> bool {
        #[derive(Clone, Copy, PartialEq)]
        enum Color {
            White,
            Gray,
            Black,
        }

        let mut color: HashMap<&str, Color> = self
            .nodes
            .keys()
            .map(|id| (id.as_str(), Color::White))
            .collect();

        fn dfs<'a>(
            node: &'a str,
            forward: &'a HashMap<String, HashSet<String>>,
            color: &mut HashMap<&'a str, Color>,
        ) -> bool {
            color.insert(node, Color::Gray);
            if let Some(neighbors) = forward.get(node) {
                for neighbor in neighbors {
                    match color.get(neighbor.as_str()) {
                        Some(Color::Gray) => return true, // back edge = cycle
                        Some(Color::White) | None => {
                            if dfs(neighbor.as_str(), forward, color) {
                                return true;
                            }
                        }
                        Some(Color::Black) => {} // already fully explored
                    }
                }
            }
            color.insert(node, Color::Black);
            false
        }

        for id in self.nodes.keys() {
            if color.get(id.as_str()) == Some(&Color::White)
                && dfs(id.as_str(), &self.forward, &mut color)
            {
                return true;
            }
        }
        false
    }

    /// Return the IDs of stages that directly depend on the given stage.
    #[must_use]
    pub fn dependents(&self, id: &str) -> Vec<String> {
        self.forward
            .get(id)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Return the IDs of stages that the given stage depends on.
    #[must_use]
    pub fn dependencies(&self, id: &str) -> Vec<String> {
        self.reverse
            .get(id)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Calculate progress: fraction of nodes that are done (0.0–1.0).
    #[must_use]
    pub fn progress(&self) -> f64 {
        if self.nodes.is_empty() {
            return 1.0;
        }
        let done = self
            .nodes
            .values()
            .filter(|n| n.status == StageStatus::Done || n.status == StageStatus::Skipped)
            .count();
        let total = self.nodes.len();
        // Practical DAG sizes are well within u32 range; truncate_as avoids
        // the clippy::cast_precision_loss lint on 64-bit targets.
        let done_u32 = u32::try_from(done).unwrap_or(u32::MAX);
        let total_u32 = u32::try_from(total).unwrap_or(u32::MAX);
        f64::from(done_u32) / f64::from(total_u32)
    }

    /// Check if all stages are in a terminal state (done, failed, or skipped).
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.nodes.values().all(|n| {
            matches!(
                n.status,
                StageStatus::Done | StageStatus::Failed | StageStatus::Skipped
            )
        })
    }

    /// Check if any stage has failed.
    #[must_use]
    pub fn has_failures(&self) -> bool {
        self.nodes.values().any(|n| n.status == StageStatus::Failed)
    }

    /// Return all stages grouped by phase ID, preserving topological order within each phase.
    ///
    /// Uses the cached topological sort computed at construction time (#485).
    #[must_use]
    pub fn stages_by_phase(&self) -> HashMap<String, Vec<String>> {
        let mut by_phase: HashMap<String, Vec<String>> = HashMap::new();
        // Use cached topological order for consistent ordering without recomputation.
        let order = self
            .cached_topo_order
            .clone()
            .or_else(|| self.topological_sort().ok());
        if let Some(order) = order {
            for id in order {
                if let Some(node) = self.nodes.get(&id) {
                    by_phase.entry(node.phase_id.clone()).or_default().push(id);
                }
            }
        } else {
            // Fallback: arbitrary order (should not happen for valid DAGs).
            for (id, node) in &self.nodes {
                by_phase
                    .entry(node.phase_id.clone())
                    .or_default()
                    .push(id.clone());
            }
        }
        by_phase
    }

    /// Build a map from `stage_id` → `StageStatus` for all nodes.
    #[must_use]
    pub fn status_map(&self) -> HashMap<String, StageStatus> {
        self.nodes
            .iter()
            .map(|(id, node)| (id.clone(), node.status.clone()))
            .collect()
    }

    /// Build a map from `stage_id` → `agent_id` for all running stages.
    #[must_use]
    pub fn agent_map(&self) -> HashMap<String, String> {
        self.nodes
            .iter()
            .filter_map(|(id, node)| node.agent_id.as_ref().map(|a| (id.clone(), a.clone())))
            .collect()
    }
}

impl Default for Dag {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_node(id: &str, phase: &str, deps: &[&str]) -> DagNode {
        DagNode {
            id: id.to_string(),
            title: format!("Stage {}", id),
            status: StageStatus::Pending,
            depends_on: deps.iter().map(|d| d.to_string()).collect(),
            issue_id: None,
            agent_id: None,
            phase_id: phase.to_string(),
        }
    }

    #[test]
    fn test_empty_dag() {
        let dag = Dag::new();
        assert!(dag.is_empty());
        assert_eq!(dag.len(), 0);
        assert!(dag.is_complete());
        assert_eq!(dag.progress(), 1.0);
        assert!(dag.ready_nodes().is_empty());
    }

    #[test]
    fn test_single_node_no_deps() {
        let dag = Dag::from_nodes(&vec![make_node("a", "p1", &[])]).unwrap();
        assert_eq!(dag.len(), 1);
        assert_eq!(dag.ready_nodes(), vec!["a"]);
        assert!(!dag.is_complete());
    }

    #[test]
    fn test_linear_chain() {
        let dag = Dag::from_nodes(&vec![
            make_node("a", "p1", &[]),
            make_node("b", "p1", &["a"]),
            make_node("c", "p1", &["b"]),
        ])
        .unwrap();

        assert_eq!(dag.ready_nodes(), vec!["a"]);
        assert_eq!(dag.topological_sort().unwrap(), vec!["a", "b", "c"]);
    }

    #[test]
    fn test_diamond_dag() {
        let dag = Dag::from_nodes(&vec![
            make_node("a", "p1", &[]),
            make_node("b", "p1", &["a"]),
            make_node("c", "p1", &["a"]),
            make_node("d", "p1", &["b", "c"]),
        ])
        .unwrap();

        assert_eq!(dag.ready_nodes(), vec!["a"]);

        let topo = dag.topological_sort().unwrap();
        assert_eq!(topo[0], "a");
        assert_eq!(topo[3], "d");
        // b and c can be in either order
        assert!(topo[1] == "b" || topo[1] == "c");
        assert!(topo[2] == "b" || topo[2] == "c");
    }

    #[test]
    fn test_cycle_detection() {
        let result = Dag::from_nodes(&vec![
            make_node("a", "p1", &["b"]),
            make_node("b", "p1", &["a"]),
        ]);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string().to_lowercase();
        assert!(
            err_msg.contains("cycle"),
            "Expected 'cycle' in error: {}",
            err_msg
        );
    }

    #[test]
    fn test_missing_dependency() {
        let result = Dag::from_nodes(&vec![make_node("a", "p1", &["nonexistent"])]);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("does not exist"));
    }

    #[test]
    fn test_duplicate_id() {
        let result = Dag::from_nodes(&vec![make_node("a", "p1", &[]), make_node("a", "p1", &[])]);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Duplicate"));
    }

    #[test]
    fn test_mark_running_and_done() {
        let mut dag = Dag::from_nodes(&vec![
            make_node("a", "p1", &[]),
            make_node("b", "p1", &["a"]),
            make_node("c", "p1", &["a"]),
        ])
        .unwrap();

        // a is ready, b and c are blocked
        assert_eq!(dag.ready_nodes(), vec!["a"]);

        dag.mark_running("a", "agent-1").unwrap();
        assert_eq!(dag.running_nodes(), vec!["a"]);
        assert!(dag.ready_nodes().is_empty());

        // Complete a → b and c become ready
        let unblocked = dag.mark_done("a").unwrap();
        assert_eq!(unblocked.len(), 2);
        assert!(unblocked.contains(&"b".to_string()));
        assert!(unblocked.contains(&"c".to_string()));

        let mut ready = dag.ready_nodes();
        ready.sort();
        assert_eq!(ready, vec!["b", "c"]);
    }

    #[test]
    fn test_mark_failed() {
        let mut dag = Dag::from_nodes(&vec![
            make_node("a", "p1", &[]),
            make_node("b", "p1", &["a"]),
        ])
        .unwrap();

        dag.mark_running("a", "agent-1").unwrap();
        dag.mark_failed("a").unwrap();

        assert!(dag.has_failures());
        // b is still pending but blocked because a is not done
        assert!(dag.ready_nodes().is_empty());
    }

    #[test]
    fn test_mark_skipped() {
        let mut dag = Dag::from_nodes(&vec![make_node("a", "p1", &[])]).unwrap();
        dag.mark_skipped("a").unwrap();
        assert!(dag.is_complete());
        assert_eq!(dag.progress(), 1.0);
    }

    #[test]
    fn test_progress_tracking() {
        let mut dag = Dag::from_nodes(&vec![
            make_node("a", "p1", &[]),
            make_node("b", "p1", &[]),
            make_node("c", "p1", &[]),
            make_node("d", "p1", &[]),
        ])
        .unwrap();

        assert_eq!(dag.progress(), 0.0);

        dag.mark_running("a", "agent-1").unwrap();
        dag.mark_done("a").unwrap();
        assert!((dag.progress() - 0.25).abs() < f64::EPSILON);

        dag.mark_running("b", "agent-2").unwrap();
        dag.mark_done("b").unwrap();
        assert!((dag.progress() - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_cannot_mark_done_if_not_running() {
        let mut dag = Dag::from_nodes(&vec![make_node("a", "p1", &[])]).unwrap();
        assert!(dag.mark_done("a").is_err());
    }

    #[test]
    fn test_cannot_mark_running_if_not_pending() {
        let mut dag = Dag::from_nodes(&vec![make_node("a", "p1", &[])]).unwrap();
        dag.mark_running("a", "agent-1").unwrap();
        assert!(dag.mark_running("a", "agent-2").is_err());
    }

    #[test]
    fn test_set_issue_id() {
        let mut dag = Dag::from_nodes(&vec![make_node("a", "p1", &[])]).unwrap();
        dag.set_issue_id("a", 42).unwrap();
        assert_eq!(dag.get("a").unwrap().issue_id, Some(42));
    }

    #[test]
    fn test_dependents_and_dependencies() {
        let dag = Dag::from_nodes(&vec![
            make_node("a", "p1", &[]),
            make_node("b", "p1", &["a"]),
            make_node("c", "p1", &["a"]),
        ])
        .unwrap();

        let mut deps = dag.dependents("a");
        deps.sort();
        assert_eq!(deps, vec!["b", "c"]);
        assert_eq!(dag.dependencies("b"), vec!["a"]);
        assert!(dag.dependencies("a").is_empty());
    }

    #[test]
    fn test_stages_by_phase() {
        let dag = Dag::from_nodes(&vec![
            make_node("a", "p1", &[]),
            make_node("b", "p1", &["a"]),
            make_node("c", "p2", &[]),
        ])
        .unwrap();

        let by_phase = dag.stages_by_phase();
        assert_eq!(by_phase.get("p1").unwrap().len(), 2);
        assert_eq!(by_phase.get("p2").unwrap().len(), 1);
    }

    #[test]
    fn test_status_and_agent_maps() {
        let mut dag =
            Dag::from_nodes(&vec![make_node("a", "p1", &[]), make_node("b", "p1", &[])]).unwrap();

        dag.mark_running("a", "agent-1").unwrap();

        let status_map = dag.status_map();
        assert_eq!(status_map.get("a"), Some(&StageStatus::Running));
        assert_eq!(status_map.get("b"), Some(&StageStatus::Pending));

        let agent_map = dag.agent_map();
        assert_eq!(agent_map.get("a"), Some(&"agent-1".to_string()));
        assert!(!agent_map.contains_key("b"));
    }

    #[test]
    fn test_complex_multi_phase_dag() {
        // Simulates the web dashboard phases: 1 → (2 || 3) → 4 → 6
        let dag = Dag::from_nodes(&vec![
            make_node("1a", "p1", &[]),
            make_node("1b", "p1", &[]),
            make_node("2a", "p2", &["1a", "1b"]),
            make_node("2b", "p2", &["1a", "1b"]),
            make_node("3a", "p3", &["1a", "1b"]),
            make_node("3b", "p3", &["1a", "1b"]),
            make_node("4a", "p4", &["3a", "3b"]),
            make_node("6a", "p6", &["2a", "2b", "4a"]),
        ])
        .unwrap();

        let topo = dag.topological_sort().unwrap();

        // 1a and 1b must come first
        let pos = |id: &str| topo.iter().position(|x| x == id).unwrap();
        assert!(pos("1a") < pos("2a"));
        assert!(pos("1b") < pos("2a"));
        assert!(pos("3a") < pos("4a"));
        assert!(pos("3b") < pos("4a"));
        assert!(pos("4a") < pos("6a"));
        assert!(pos("2a") < pos("6a"));
        assert!(pos("2b") < pos("6a"));
    }

    #[test]
    fn test_serialization_round_trip() {
        let dag = Dag::from_nodes(&vec![
            make_node("a", "p1", &[]),
            make_node("b", "p1", &["a"]),
        ])
        .unwrap();

        let json = serde_json::to_string_pretty(&dag).unwrap();
        let restored: Dag = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.len(), 2);
        assert_eq!(restored.get("a").unwrap().title, "Stage a");
        assert_eq!(restored.dependencies("b"), dag.dependencies("b"));
    }

    #[test]
    fn test_three_node_cycle_detection() {
        let result = Dag::from_nodes(&vec![
            make_node("a", "p1", &["c"]),
            make_node("b", "p1", &["a"]),
            make_node("c", "p1", &["b"]),
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn test_self_loop_detection() {
        let result = Dag::from_nodes(&vec![make_node("a", "p1", &["a"])]);
        assert!(result.is_err());
    }

    #[test]
    fn test_no_false_cycle_on_diamond() {
        // Diamond is NOT a cycle
        let dag = Dag::from_nodes(&vec![
            make_node("a", "p1", &[]),
            make_node("b", "p1", &["a"]),
            make_node("c", "p1", &["a"]),
            make_node("d", "p1", &["b", "c"]),
        ]);
        assert!(dag.is_ok());
        assert!(!dag.unwrap().has_cycle());
    }

    #[test]
    fn test_many_independent_nodes_all_ready() {
        let nodes: Vec<DagNode> = (0..10)
            .map(|i| make_node(&format!("n{}", i), "p1", &[]))
            .collect();
        let dag = Dag::from_nodes(&nodes).unwrap();
        assert_eq!(dag.ready_nodes().len(), 10);
    }

    #[test]
    fn test_default_creates_empty_dag() {
        let dag = Dag::default();
        assert!(dag.is_empty());
        assert_eq!(dag.len(), 0);
    }

    #[test]
    fn test_is_empty_false_with_nodes() {
        let dag = Dag::from_nodes(&vec![make_node("a", "p1", &[])]).unwrap();
        assert!(!dag.is_empty());
    }

    #[test]
    fn test_has_failures_false_when_clean() {
        let dag = Dag::from_nodes(&vec![make_node("a", "p1", &[])]).unwrap();
        assert!(!dag.has_failures());
    }

    #[test]
    fn test_nodes_with_status() {
        let mut dag = Dag::from_nodes(&vec![
            make_node("a", "p1", &[]),
            make_node("b", "p1", &[]),
            make_node("c", "p1", &[]),
        ])
        .unwrap();

        // All start pending
        assert_eq!(dag.nodes_with_status(&StageStatus::Pending).len(), 3);
        assert_eq!(dag.nodes_with_status(&StageStatus::Running).len(), 0);
        assert_eq!(dag.nodes_with_status(&StageStatus::Done).len(), 0);

        dag.mark_running("a", "agent-1").unwrap();
        assert_eq!(dag.nodes_with_status(&StageStatus::Pending).len(), 2);
        assert_eq!(dag.nodes_with_status(&StageStatus::Running).len(), 1);

        dag.mark_done("a").unwrap();
        assert_eq!(dag.nodes_with_status(&StageStatus::Done).len(), 1);

        dag.mark_failed("b").unwrap();
        assert_eq!(dag.nodes_with_status(&StageStatus::Failed).len(), 1);

        dag.mark_skipped("c").unwrap();
        assert_eq!(dag.nodes_with_status(&StageStatus::Skipped).len(), 1);
    }

    #[test]
    fn test_mark_running_nonexistent_node() {
        let mut dag = Dag::from_nodes(&vec![make_node("a", "p1", &[])]).unwrap();
        let result = dag.mark_running("nonexistent", "agent-1");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_mark_done_nonexistent_node() {
        let mut dag = Dag::from_nodes(&vec![make_node("a", "p1", &[])]).unwrap();
        let result = dag.mark_done("nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_mark_failed_nonexistent_node() {
        let mut dag = Dag::from_nodes(&vec![make_node("a", "p1", &[])]).unwrap();
        let result = dag.mark_failed("nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_mark_skipped_nonexistent_node() {
        let mut dag = Dag::from_nodes(&vec![make_node("a", "p1", &[])]).unwrap();
        let result = dag.mark_skipped("nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_set_issue_id_nonexistent_node() {
        let mut dag = Dag::from_nodes(&vec![make_node("a", "p1", &[])]).unwrap();
        let result = dag.set_issue_id("nonexistent", 42);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_dependents_nonexistent_node() {
        let dag = Dag::from_nodes(&vec![make_node("a", "p1", &[])]).unwrap();
        assert!(dag.dependents("nonexistent").is_empty());
    }

    #[test]
    fn test_dependencies_nonexistent_node() {
        let dag = Dag::from_nodes(&vec![make_node("a", "p1", &[])]).unwrap();
        assert!(dag.dependencies("nonexistent").is_empty());
    }

    #[test]
    fn test_get_returns_none_for_missing() {
        let dag = Dag::from_nodes(&vec![make_node("a", "p1", &[])]).unwrap();
        assert!(dag.get("nonexistent").is_none());
        assert!(dag.get("a").is_some());
    }

    #[test]
    fn test_get_mut_returns_none_for_missing() {
        let mut dag = Dag::from_nodes(&vec![make_node("a", "p1", &[])]).unwrap();
        assert!(dag.get_mut("nonexistent").is_none());
        assert!(dag.get_mut("a").is_some());
    }

    #[test]
    fn test_get_mut_modifies_node() {
        let mut dag = Dag::from_nodes(&vec![make_node("a", "p1", &[])]).unwrap();
        dag.get_mut("a").unwrap().title = "Modified".to_string();
        assert_eq!(dag.get("a").unwrap().title, "Modified");
    }

    #[test]
    fn test_node_ids_returns_all_ids() {
        let dag = Dag::from_nodes(&vec![
            make_node("x", "p1", &[]),
            make_node("y", "p1", &[]),
            make_node("z", "p1", &[]),
        ])
        .unwrap();
        let mut ids = dag.node_ids();
        ids.sort();
        assert_eq!(ids, vec!["x", "y", "z"]);
    }

    #[test]
    fn test_nodes_returns_all_nodes() {
        let dag =
            Dag::from_nodes(&vec![make_node("a", "p1", &[]), make_node("b", "p1", &[])]).unwrap();
        let nodes = dag.nodes();
        assert_eq!(nodes.len(), 2);
        assert!(nodes.contains_key("a"));
        assert!(nodes.contains_key("b"));
    }

    #[test]
    fn test_running_nodes_empty_when_none_running() {
        let dag = Dag::from_nodes(&vec![make_node("a", "p1", &[])]).unwrap();
        assert!(dag.running_nodes().is_empty());
    }

    #[test]
    fn test_ready_nodes_blocked_by_running_dep() {
        let mut dag = Dag::from_nodes(&vec![
            make_node("a", "p1", &[]),
            make_node("b", "p1", &["a"]),
        ])
        .unwrap();

        dag.mark_running("a", "agent-1").unwrap();
        // b should NOT be ready since a is running, not done
        assert!(dag.ready_nodes().is_empty());
    }

    #[test]
    fn test_ready_nodes_blocked_by_failed_dep() {
        let mut dag = Dag::from_nodes(&vec![
            make_node("a", "p1", &[]),
            make_node("b", "p1", &["a"]),
        ])
        .unwrap();

        dag.mark_failed("a").unwrap();
        // b should NOT be ready since a is failed, not done
        assert!(dag.ready_nodes().is_empty());
    }

    #[test]
    fn test_mark_done_no_dependents_returns_empty() {
        let mut dag = Dag::from_nodes(&vec![make_node("a", "p1", &[])]).unwrap();
        dag.mark_running("a", "agent-1").unwrap();
        let unblocked = dag.mark_done("a").unwrap();
        assert!(unblocked.is_empty());
    }

    #[test]
    fn test_mark_done_dependent_not_pending_not_unblocked() {
        let mut dag = Dag::from_nodes(&vec![
            make_node("a", "p1", &[]),
            make_node("b", "p1", &["a"]),
        ])
        .unwrap();

        // Mark b as failed before a completes
        dag.mark_failed("b").unwrap();

        dag.mark_running("a", "agent-1").unwrap();
        let unblocked = dag.mark_done("a").unwrap();
        // b is failed, not pending, so it should NOT appear in unblocked
        assert!(unblocked.is_empty());
    }

    #[test]
    fn test_progress_with_mixed_terminal_states() {
        let mut dag = Dag::from_nodes(&vec![
            make_node("a", "p1", &[]),
            make_node("b", "p1", &[]),
            make_node("c", "p1", &[]),
            make_node("d", "p1", &[]),
        ])
        .unwrap();

        dag.mark_running("a", "agent-1").unwrap();
        dag.mark_done("a").unwrap();
        dag.mark_skipped("b").unwrap();
        // c and d still pending
        assert!((dag.progress() - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_is_complete_with_all_terminal_states() {
        let mut dag = Dag::from_nodes(&vec![
            make_node("a", "p1", &[]),
            make_node("b", "p1", &[]),
            make_node("c", "p1", &[]),
        ])
        .unwrap();

        dag.mark_running("a", "agent-1").unwrap();
        dag.mark_done("a").unwrap();
        dag.mark_failed("b").unwrap();
        dag.mark_skipped("c").unwrap();

        assert!(dag.is_complete());
    }

    #[test]
    fn test_is_complete_false_with_running() {
        let mut dag = Dag::from_nodes(&vec![make_node("a", "p1", &[])]).unwrap();
        dag.mark_running("a", "agent-1").unwrap();
        assert!(!dag.is_complete());
    }

    #[test]
    fn test_topological_sort_empty_dag() {
        let dag = Dag::new();
        let order = dag.topological_sort().unwrap();
        assert!(order.is_empty());
    }

    #[test]
    fn test_has_cycle_empty_dag() {
        let dag = Dag::new();
        assert!(!dag.has_cycle());
    }

    #[test]
    fn test_stages_by_phase_multiple_phases() {
        let dag = Dag::from_nodes(&vec![
            make_node("a", "phase-1", &[]),
            make_node("b", "phase-1", &["a"]),
            make_node("c", "phase-2", &[]),
            make_node("d", "phase-2", &["c"]),
            make_node("e", "phase-3", &[]),
        ])
        .unwrap();

        let by_phase = dag.stages_by_phase();
        assert_eq!(by_phase.len(), 3);
        assert_eq!(by_phase["phase-1"].len(), 2);
        assert_eq!(by_phase["phase-2"].len(), 2);
        assert_eq!(by_phase["phase-3"].len(), 1);
        // Within phase-1, a should come before b (topological order)
        let p1 = &by_phase["phase-1"];
        let pos_a = p1.iter().position(|x| x == "a").unwrap();
        let pos_b = p1.iter().position(|x| x == "b").unwrap();
        assert!(pos_a < pos_b);
    }

    #[test]
    fn test_agent_map_only_includes_agents() {
        let mut dag = Dag::from_nodes(&vec![
            make_node("a", "p1", &[]),
            make_node("b", "p1", &[]),
            make_node("c", "p1", &[]),
        ])
        .unwrap();

        // Only a has an agent
        dag.mark_running("a", "agent-1").unwrap();
        let map = dag.agent_map();
        assert_eq!(map.len(), 1);
        assert_eq!(map["a"], "agent-1");
    }

    #[test]
    fn test_status_map_all_nodes() {
        let mut dag =
            Dag::from_nodes(&vec![make_node("a", "p1", &[]), make_node("b", "p1", &[])]).unwrap();

        dag.mark_running("a", "agent-1").unwrap();
        dag.mark_done("a").unwrap();
        dag.mark_skipped("b").unwrap();

        let map = dag.status_map();
        assert_eq!(map["a"], StageStatus::Done);
        assert_eq!(map["b"], StageStatus::Skipped);
    }

    #[test]
    fn test_mark_done_diamond_partial_unblock() {
        // d depends on both b and c. Completing b should NOT unblock d.
        let mut dag = Dag::from_nodes(&vec![
            make_node("a", "p1", &[]),
            make_node("b", "p1", &["a"]),
            make_node("c", "p1", &["a"]),
            make_node("d", "p1", &["b", "c"]),
        ])
        .unwrap();

        dag.mark_running("a", "agent-1").unwrap();
        dag.mark_done("a").unwrap();

        dag.mark_running("b", "agent-2").unwrap();
        let unblocked = dag.mark_done("b").unwrap();
        // d should NOT be unblocked yet because c is still pending
        assert!(!unblocked.contains(&"d".to_string()));

        dag.mark_running("c", "agent-3").unwrap();
        let unblocked = dag.mark_done("c").unwrap();
        // NOW d should be unblocked
        assert!(unblocked.contains(&"d".to_string()));
    }

    #[test]
    fn test_topological_sort_cycle_error() {
        // Manually construct a DAG with a cycle (bypassing from_nodes validation)
        let mut dag = Dag::new();
        dag.nodes.insert(
            "a".to_string(),
            DagNode {
                id: "a".to_string(),
                title: "A".to_string(),
                status: StageStatus::Pending,
                depends_on: vec!["b".to_string()],
                issue_id: None,
                agent_id: None,
                phase_id: "p1".to_string(),
            },
        );
        dag.nodes.insert(
            "b".to_string(),
            DagNode {
                id: "b".to_string(),
                title: "B".to_string(),
                status: StageStatus::Pending,
                depends_on: vec!["a".to_string()],
                issue_id: None,
                agent_id: None,
                phase_id: "p1".to_string(),
            },
        );
        // Set up edges for the cycle
        dag.forward
            .entry("a".to_string())
            .or_default()
            .insert("b".to_string());
        dag.forward
            .entry("b".to_string())
            .or_default()
            .insert("a".to_string());
        dag.reverse
            .entry("a".to_string())
            .or_default()
            .insert("b".to_string());
        dag.reverse
            .entry("b".to_string())
            .or_default()
            .insert("a".to_string());

        // topological_sort should fail with cycle error
        let err = dag.topological_sort().unwrap_err();
        assert!(err.to_string().contains("Cycle"));

        // stages_by_phase should fall back to arbitrary order
        let by_phase = dag.stages_by_phase();
        assert_eq!(by_phase["p1"].len(), 2);
    }
}
