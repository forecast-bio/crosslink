// Swarm merge orchestration: discover worktrees, detect conflicts,
// compute merge order, and apply diffs.

use anyhow::{bail, Context, Result};
use std::path::Path;

use super::io::*;
use super::types::*;
use crate::sync::SyncManager;

/// Discover agent worktrees that have commits beyond the base branch (develop).
fn discover_worktrees(repo_root: &Path) -> Result<Vec<MergeSource>> {
    let worktrees_dir = repo_root.join(".worktrees");
    if !worktrees_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut sources = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(&worktrees_dir)
        .context("Failed to read .worktrees")?
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let wt_path = entry.path();
        if !wt_path.is_dir() {
            continue;
        }

        let slug = entry.file_name().to_string_lossy().to_string();

        // Get changed files relative to the base branch.
        // Try multiple base refs since worktrees may have been created from
        // develop, main, or their remote counterparts (#392).
        let base_refs = ["develop", "main", "origin/develop", "origin/main"];
        let mut changed_files = Vec::new();
        for base in &base_refs {
            let diff_output = std::process::Command::new("git")
                .current_dir(&wt_path)
                .args(["diff", "--name-only", &format!("{}...HEAD", base)])
                .output();

            if let Ok(output) = diff_output {
                if output.status.success() {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    changed_files = stdout
                        .lines()
                        .filter(|l| !l.is_empty())
                        .map(|l| l.to_string())
                        .collect::<Vec<_>>();
                    if !changed_files.is_empty() {
                        break;
                    }
                }
            }
        }

        if changed_files.is_empty() {
            continue;
        }

        // Count commits beyond base branch
        let mut commit_count = 0;
        for base in &base_refs {
            let log_output = std::process::Command::new("git")
                .current_dir(&wt_path)
                .args(["log", "--oneline", &format!("{}..HEAD", base)])
                .output();

            if let Ok(output) = log_output {
                if output.status.success() {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let count = stdout.lines().count();
                    if count > 0 {
                        commit_count = count;
                        break;
                    }
                }
            }
        }

        sources.push(MergeSource {
            agent_slug: slug,
            worktree_path: wt_path,
            changed_files,
            commit_count,
        });
    }

    Ok(sources)
}

/// Extract line ranges modified by a diff for a specific file in a worktree.
fn extract_diff_ranges(worktree: &Path, file: &str) -> Result<Vec<(usize, usize)>> {
    let output = std::process::Command::new("git")
        .current_dir(worktree)
        .args(["diff", "develop...HEAD", "--", file])
        .output()
        .context("Failed to run git diff")?;

    if !output.status.success() {
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut ranges = Vec::new();

    for line in stdout.lines() {
        // Parse unified diff hunk headers: @@ -start,count +start,count @@
        if let Some(rest) = line.strip_prefix("@@ ") {
            // Extract the +start,count part (new file ranges)
            if let Some(plus_part) = rest.split(' ').find(|s| s.starts_with('+')) {
                let nums = plus_part.trim_start_matches('+');
                let parts: Vec<&str> = nums.split(',').collect();
                if let Ok(start) = parts[0].parse::<usize>() {
                    let count = if parts.len() > 1 {
                        parts[1]
                            .split_whitespace()
                            .next()
                            .and_then(|s| s.parse::<usize>().ok())
                            .unwrap_or(1)
                    } else {
                        1
                    };
                    if count > 0 {
                        ranges.push((start, start + count - 1));
                    }
                }
            }
        }
    }

    Ok(ranges)
}

/// Check if two sets of line ranges overlap.
pub(super) fn ranges_overlap(a: &[(usize, usize)], b: &[(usize, usize)]) -> bool {
    for &(a_start, a_end) in a {
        for &(b_start, b_end) in b {
            if a_start <= b_end && b_start <= a_end {
                return true;
            }
        }
    }
    false
}

/// Detect file conflicts between multiple merge sources.
pub(super) fn detect_file_conflicts(sources: &[MergeSource]) -> Vec<FileConflict> {
    // Build map: file -> list of agent slugs that modified it
    let mut file_agents: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();

    for source in sources {
        for file in &source.changed_files {
            file_agents
                .entry(file.clone())
                .or_default()
                .push(source.agent_slug.clone());
        }
    }

    let mut conflicts = Vec::new();

    for (file, agents) in &file_agents {
        if agents.len() < 2 {
            continue;
        }

        // Build a lookup for worktree paths by agent slug
        let slug_to_source: std::collections::HashMap<&str, &MergeSource> =
            sources.iter().map(|s| (s.agent_slug.as_str(), s)).collect();

        // Check if we can determine overlap by inspecting diff ranges
        let mut all_ranges: Vec<(&str, Vec<(usize, usize)>)> = Vec::new();
        let mut range_extraction_ok = true;

        for agent_slug in agents {
            if let Some(source) = slug_to_source.get(agent_slug.as_str()) {
                match extract_diff_ranges(&source.worktree_path, file) {
                    Ok(ranges) if !ranges.is_empty() => {
                        all_ranges.push((agent_slug.as_str(), ranges));
                    }
                    Ok(_) => {
                        // Empty ranges could mean the file was created or binary
                        range_extraction_ok = false;
                        break;
                    }
                    Err(_) => {
                        range_extraction_ok = false;
                        break;
                    }
                }
            }
        }

        let conflict_type = if !range_extraction_ok {
            // If we can't extract ranges, check if file is new in any worktree
            ConflictType::CreateModify
        } else {
            // Check pairwise for overlapping ranges
            let mut has_overlap = false;
            'outer: for i in 0..all_ranges.len() {
                for j in (i + 1)..all_ranges.len() {
                    if ranges_overlap(&all_ranges[i].1, &all_ranges[j].1) {
                        has_overlap = true;
                        break 'outer;
                    }
                }
            }
            if has_overlap {
                ConflictType::Overlapping
            } else {
                ConflictType::NonOverlapping
            }
        };

        conflicts.push(FileConflict {
            file: file.clone(),
            agents: agents.clone(),
            conflict_type,
        });
    }

    conflicts
}

/// Compute merge order: non-conflicting agents first, then non-overlapping, then overlapping.
pub(super) fn compute_merge_order(
    sources: &[MergeSource],
    conflicts: &[FileConflict],
) -> Vec<String> {
    // Classify each agent's worst conflict level
    let mut agent_worst: std::collections::BTreeMap<&str, u8> = std::collections::BTreeMap::new();

    // Start all agents at level 0 (no conflicts)
    for source in sources {
        agent_worst.insert(&source.agent_slug, 0);
    }

    for conflict in conflicts {
        let level = match conflict.conflict_type {
            ConflictType::NonOverlapping => 1,
            ConflictType::CreateModify => 2,
            ConflictType::Overlapping => 3,
        };
        for agent in &conflict.agents {
            if let Some(current) = agent_worst.get_mut(agent.as_str()) {
                if level > *current {
                    *current = level;
                }
            }
        }
    }

    // Sort: lowest conflict level first, then alphabetically for stability
    let mut order: Vec<(&str, u8)> = agent_worst.into_iter().collect();
    order.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(b.0)));

    order.iter().map(|(slug, _)| slug.to_string()).collect()
}

/// Orchestrate merging agent worktree changes into a single branch.
pub fn merge(
    crosslink_dir: &Path,
    branch: &str,
    dry_run: bool,
    agents_filter: Option<&str>,
) -> Result<()> {
    let repo_root = crosslink_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine repo root"))?;

    // Discover agent worktrees with changes
    let mut sources = discover_worktrees(repo_root)?;

    if sources.is_empty() {
        println!("No agent worktrees with changes found.");
        return Ok(());
    }

    // Filter by agent slugs if --agents provided
    if let Some(filter) = agents_filter {
        let slugs: std::collections::HashSet<&str> = filter.split(',').map(|s| s.trim()).collect();
        sources.retain(|s| slugs.contains(s.agent_slug.as_str()));
        if sources.is_empty() {
            bail!("No matching agent worktrees found for filter: {}", filter);
        }
    }

    // Detect file conflicts
    let conflicts = detect_file_conflicts(&sources);

    // Compute merge order
    let merge_order = compute_merge_order(&sources, &conflicts);

    // Build the merge plan
    let plan = MergePlan {
        target_branch: branch.to_string(),
        agents: sources.clone(),
        conflicts: conflicts.clone(),
        merge_order: merge_order.clone(),
    };

    // Print summary
    println!("Merge Plan");
    println!("==========");
    println!("Target branch: {}", branch);
    println!(
        "Agents:        {} ({} total commits)",
        sources.len(),
        sources.iter().map(|s| s.commit_count).sum::<usize>()
    );
    println!();

    // Agent details table
    println!("Agent Worktrees:");
    for source in &sources {
        println!(
            "  {} — {} file{}, {} commit{}",
            source.agent_slug,
            source.changed_files.len(),
            if source.changed_files.len() == 1 {
                ""
            } else {
                "s"
            },
            source.commit_count,
            if source.commit_count == 1 { "" } else { "s" },
        );
    }
    println!();

    // Conflict analysis
    if conflicts.is_empty() {
        println!("Conflicts:     none detected");
    } else {
        println!(
            "Conflicts:     {} file{}",
            conflicts.len(),
            if conflicts.len() == 1 { "" } else { "s" }
        );
        for conflict in &conflicts {
            let type_label = match conflict.conflict_type {
                ConflictType::NonOverlapping => "non-overlapping",
                ConflictType::Overlapping => "OVERLAPPING",
                ConflictType::CreateModify => "create/modify",
            };
            println!(
                "  {} [{}] — agents: {}",
                conflict.file,
                type_label,
                conflict.agents.join(", ")
            );
        }

        let overlapping_count = conflicts
            .iter()
            .filter(|c| c.conflict_type == ConflictType::Overlapping)
            .count();
        if overlapping_count > 0 {
            println!();
            println!(
                "WARNING: {} file{} with overlapping changes will need manual resolution.",
                overlapping_count,
                if overlapping_count == 1 { "" } else { "s" }
            );
        }
    }
    println!();

    // Merge order
    println!("Merge order:");
    for (i, slug) in merge_order.iter().enumerate() {
        println!("  {}. {}", i + 1, slug);
    }
    println!();

    // Persist the plan to hub branch
    let sync = SyncManager::new(crosslink_dir)?;
    if sync.is_initialized() {
        sync.fetch()?;
        write_hub_json(&sync, "swarm/merge-plan.json", &plan)?;
        commit_hub_files(
            &sync,
            &["swarm/merge-plan.json"],
            &format!(
                "swarm: merge plan for {} agents → {}",
                sources.len(),
                branch
            ),
        )?;
        println!("Plan saved to hub branch (swarm/merge-plan.json).");
    }

    if dry_run {
        println!("Dry run — no changes applied.");
        return Ok(());
    }

    // Create the target branch from develop
    let create_branch = std::process::Command::new("git")
        .current_dir(repo_root)
        .args(["checkout", "-b", branch, "develop"])
        .output()
        .context("Failed to create target branch")?;

    if !create_branch.status.success() {
        let stderr = String::from_utf8_lossy(&create_branch.stderr);
        // If branch already exists, try to check it out
        if stderr.contains("already exists") {
            let checkout = std::process::Command::new("git")
                .current_dir(repo_root)
                .args(["checkout", branch])
                .output()
                .context("Failed to checkout existing target branch")?;
            if !checkout.status.success() {
                bail!(
                    "Failed to checkout branch '{}': {}",
                    branch,
                    String::from_utf8_lossy(&checkout.stderr)
                );
            }
            println!("Checked out existing branch '{}'.", branch);
        } else {
            bail!("Failed to create branch '{}': {}", branch, stderr);
        }
    } else {
        println!("Created branch '{}' from develop.", branch);
    }

    // Apply each agent's diff in merge order
    let slug_to_source: std::collections::HashMap<&str, &MergeSource> =
        sources.iter().map(|s| (s.agent_slug.as_str(), s)).collect();

    let mut applied = 0usize;
    let mut failed = Vec::new();

    for slug in &merge_order {
        let source = match slug_to_source.get(slug.as_str()) {
            Some(s) => s,
            None => continue,
        };

        println!("Applying changes from '{}'...", slug);

        // Generate the diff from the agent's worktree
        let diff_output = std::process::Command::new("git")
            .current_dir(&source.worktree_path)
            .args(["diff", "develop...HEAD"])
            .output()
            .context("Failed to generate diff")?;

        if !diff_output.status.success() {
            tracing::error!(
                "Failed to generate diff for '{}': {}",
                slug,
                String::from_utf8_lossy(&diff_output.stderr)
            );
            failed.push(slug.clone());
            continue;
        }

        let diff_content = diff_output.stdout;
        if diff_content.is_empty() {
            println!("  No diff to apply for '{}'.", slug);
            continue;
        }

        // Apply the diff using git apply
        let mut apply_cmd = std::process::Command::new("git")
            .current_dir(repo_root)
            .args(["apply", "--3way", "--stat", "-"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .context("Failed to start git apply")?;

        if let Some(mut stdin) = apply_cmd.stdin.take() {
            use std::io::Write;
            stdin.write_all(&diff_content)?;
        }

        let apply_result = apply_cmd.wait_with_output()?;

        if !apply_result.status.success() {
            let stderr = String::from_utf8_lossy(&apply_result.stderr);
            tracing::error!(
                "Failed to apply diff for '{}': {} — manual resolution required.",
                slug,
                stderr
            );
            failed.push(slug.clone());

            // INTENTIONAL: checkout to abort partial apply is best-effort — next agent's diff will be applied fresh
            let _ = std::process::Command::new("git")
                .current_dir(repo_root)
                .args(["checkout", "."])
                .output();
            continue;
        }

        // INTENTIONAL: staging is best-effort — commit below will capture whatever was staged
        let _ = std::process::Command::new("git")
            .current_dir(repo_root)
            .args(["add", "-A"])
            .output()?;

        let commit_msg = format!("merge: apply changes from agent '{}'", slug);
        let commit_output = std::process::Command::new("git")
            .current_dir(repo_root)
            .args([
                "commit",
                "-m",
                &commit_msg,
                "--no-gpg-sign",
                "--allow-empty",
            ])
            .output()?;

        if commit_output.status.success() {
            println!("  Applied and committed changes from '{}'.", slug);
            applied += 1;
        } else {
            let stderr = String::from_utf8_lossy(&commit_output.stderr);
            if stderr.contains("nothing to commit") {
                println!("  No new changes from '{}' (already applied).", slug);
            } else {
                tracing::error!("Commit failed for '{}': {}", slug, stderr);
                failed.push(slug.clone());
            }
        }
    }

    println!();
    println!(
        "Merge complete: {} applied, {} failed.",
        applied,
        failed.len()
    );
    if !failed.is_empty() {
        println!("Failed agents: {}", failed.join(", "));
        println!("These agents' changes need manual resolution.");
    }

    Ok(())
}
