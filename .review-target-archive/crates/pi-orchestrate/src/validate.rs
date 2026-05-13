//! Campaign validation (RFD 0021 §"Campaign schema (TOML)" validation rules).

use crate::schema::{Campaign, Milestone};
use std::collections::{HashMap, HashSet, VecDeque};
use thiserror::Error;

/// A single validation error.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ValidationError {
    #[error("duplicate milestone id: {0:?}")]
    DuplicateMilestoneId(String),

    #[error("milestone {src:?} has depends_on {dep:?} which is not a defined milestone id")]
    UndefinedDependency { src: String, dep: String },

    #[error("milestone {src:?} override_rule forward_to {target:?} is not a defined milestone id")]
    UndefinedForwardTarget { src: String, target: String },

    #[error("dependency graph has a cycle involving milestone {0:?}")]
    DependencyCycle(String),

    #[error(
        "milestone {src:?} override_rule forward_to {target:?} is not a strict descendant of {src:?} in the dependency DAG"
    )]
    ForwardToNotDescendant { src: String, target: String },

    #[error("milestone {src:?} override_rule with verdict \"out-of-scope\" has no forward_to")]
    ForwardToMissing { src: String },
}

/// Validate a parsed [`Campaign`], returning all errors (not just the first).
pub fn validate(campaign: &Campaign) -> Result<(), Vec<ValidationError>> {
    let mut errors: Vec<ValidationError> = Vec::new();

    // --- (a) milestone ids must be unique ---------------------------------
    let mut seen_ids: HashSet<&str> = HashSet::new();
    for m in &campaign.milestones {
        if !seen_ids.insert(m.id.as_str()) {
            errors.push(ValidationError::DuplicateMilestoneId(m.id.clone()));
        }
    }

    // Build id → milestone index for subsequent checks.
    let id_set: HashSet<&str> = campaign.milestones.iter().map(|m| m.id.as_str()).collect();

    // --- (b) every depends_on refers to a defined id ----------------------
    for m in &campaign.milestones {
        for dep in &m.depends_on {
            if !id_set.contains(dep.as_str()) {
                errors.push(ValidationError::UndefinedDependency {
                    src: m.id.clone(),
                    dep: dep.clone(),
                });
            }
        }
    }

    // --- (c) no cycles in the dependency DAG ------------------------------
    if errors.is_empty() {
        // Kahn's algorithm.
        let n = campaign.milestones.len();
        let id_to_idx: HashMap<&str, usize> = campaign
            .milestones
            .iter()
            .enumerate()
            .map(|(i, m)| (m.id.as_str(), i))
            .collect();

        let mut in_degree = vec![0usize; n];
        // adjacency: dep → dependents
        let mut adj: Vec<Vec<usize>> = vec![vec![]; n];

        for (i, m) in campaign.milestones.iter().enumerate() {
            for dep in &m.depends_on {
                if let Some(&j) = id_to_idx.get(dep.as_str()) {
                    adj[j].push(i);
                    in_degree[i] += 1;
                }
            }
        }

        let mut queue: VecDeque<usize> = in_degree
            .iter()
            .enumerate()
            .filter(|(_, &d)| d == 0)
            .map(|(i, _)| i)
            .collect();

        let mut processed = 0usize;
        while let Some(u) = queue.pop_front() {
            processed += 1;
            for &v in &adj[u] {
                in_degree[v] -= 1;
                if in_degree[v] == 0 {
                    queue.push_back(v);
                }
            }
        }

        if processed < n {
            // Find a node still in a cycle (in_degree > 0).
            for (i, &d) in in_degree.iter().enumerate() {
                if d > 0 {
                    errors.push(ValidationError::DependencyCycle(
                        campaign.milestones[i].id.clone(),
                    ));
                    break;
                }
            }
        }
    }

    // --- (f) forward_to must be a strict descendant in the DAG -----------
    // Only checked when there are no structural errors so far (need a valid DAG).
    if errors.is_empty() {
        let id_to_idx: HashMap<&str, usize> = campaign
            .milestones
            .iter()
            .enumerate()
            .map(|(i, m)| (m.id.as_str(), i))
            .collect();

        // Pre-compute descendants for each node via BFS on the forward edges.
        let n = campaign.milestones.len();
        // forward_adj: node → nodes that depend on it (direct successors)
        let mut forward_adj: Vec<Vec<usize>> = vec![vec![]; n];
        for (i, m) in campaign.milestones.iter().enumerate() {
            for dep in &m.depends_on {
                if let Some(&j) = id_to_idx.get(dep.as_str()) {
                    // j is a predecessor of i; i is a successor of j
                    forward_adj[j].push(i);
                }
            }
        }

        // descendants(u) = all nodes reachable from u via forward_adj (BFS).
        let descendants_of = |start: usize| -> HashSet<usize> {
            let mut visited = HashSet::new();
            let mut q: VecDeque<usize> = VecDeque::new();
            for &succ in &forward_adj[start] {
                if visited.insert(succ) {
                    q.push_back(succ);
                }
            }
            while let Some(u) = q.pop_front() {
                for &succ in &forward_adj[u] {
                    if visited.insert(succ) {
                        q.push_back(succ);
                    }
                }
            }
            visited
        };

        for m in &campaign.milestones {
            for rule in &m.override_rules {
                if rule.verdict == "out-of-scope" && rule.forward_to.is_none() {
                    errors.push(ValidationError::ForwardToMissing { src: m.id.clone() });
                    continue;
                }

                if let Some(target) = &rule.forward_to {
                    let Some(&src_idx) = id_to_idx.get(m.id.as_str()) else {
                        continue;
                    };
                    let Some(&tgt_idx) = id_to_idx.get(target.as_str()) else {
                        errors.push(ValidationError::UndefinedForwardTarget {
                            src: m.id.clone(),
                            target: target.clone(),
                        });
                        continue;
                    };

                    let descs = descendants_of(src_idx);
                    if !descs.contains(&tgt_idx) {
                        errors.push(ValidationError::ForwardToNotDescendant {
                            src: m.id.clone(),
                            target: target.clone(),
                        });
                    }
                }
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Effective milestones slice after applying defaults (for convenience).
pub fn milestones_with_defaults(campaign: &Campaign) -> Vec<(&Milestone, String, u32)> {
    campaign
        .milestones
        .iter()
        .map(|m| {
            let reviewer = m.effective_reviewer(&campaign.defaults).to_string();
            let flm = m.effective_fix_loop_max(&campaign.defaults);
            (m, reviewer, flm)
        })
        .collect()
}
