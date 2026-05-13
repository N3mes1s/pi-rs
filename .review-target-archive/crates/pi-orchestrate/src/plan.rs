//! Topological ordering and human-readable plan formatting (RFD 0021).

use crate::schema::{Campaign, Milestone};
use std::collections::{HashMap, VecDeque};

/// Return milestones in topological order (roots first).
///
/// Assumes the campaign has already been validated (no cycles, no missing deps).
/// Ties within the same "level" are broken by declaration order.
pub fn topological_order(campaign: &Campaign) -> Vec<&Milestone> {
    let n = campaign.milestones.len();
    if n == 0 {
        return vec![];
    }

    let id_to_idx: HashMap<&str, usize> = campaign
        .milestones
        .iter()
        .enumerate()
        .map(|(i, m)| (m.id.as_str(), i))
        .collect();

    let mut in_degree = vec![0usize; n];
    // Adjacency: predecessor → successor.
    let mut adj: Vec<Vec<usize>> = vec![vec![]; n];

    for (i, m) in campaign.milestones.iter().enumerate() {
        for dep in &m.depends_on {
            if let Some(&j) = id_to_idx.get(dep.as_str()) {
                adj[j].push(i);
                in_degree[i] += 1;
            }
        }
    }

    // Kahn's with stable ordering: process nodes in declaration-index order.
    // Use a sorted queue (smallest index first).
    let mut ready: Vec<usize> = in_degree
        .iter()
        .enumerate()
        .filter(|(_, &d)| d == 0)
        .map(|(i, _)| i)
        .collect();
    // Keep sorted so we pop from front consistently.
    ready.sort_unstable();
    let mut queue: VecDeque<usize> = ready.into_iter().collect();

    let mut order: Vec<&Milestone> = Vec::with_capacity(n);

    while let Some(u) = queue.pop_front() {
        order.push(&campaign.milestones[u]);
        let mut next: Vec<usize> = adj[u]
            .iter()
            .filter_map(|&v| {
                in_degree[v] -= 1;
                if in_degree[v] == 0 {
                    Some(v)
                } else {
                    None
                }
            })
            .collect();
        next.sort_unstable();
        for v in next {
            queue.push_back(v);
        }
    }

    order
}

/// Format a human-readable execution plan for `--orchestrate-dry-run`.
///
/// Header: name, description (if non-empty), target branch.
/// Then a numbered list of milestones in topological order showing
/// branch, implementer, reviewer (with default applied), and fix_loop_max.
pub fn format_plan(campaign: &Campaign) -> String {
    let mut out = String::new();

    out.push_str("=== Orchestrate dry-run plan ===\n");
    out.push_str(&format!("Campaign : {}\n", campaign.name));
    if !campaign.description.is_empty() {
        out.push_str(&format!("Description: {}\n", campaign.description));
    }
    out.push_str(&format!("Target branch: {}\n", campaign.target_branch));
    out.push('\n');
    out.push_str("Execution order:\n");

    let ordered = topological_order(campaign);
    for (idx, m) in ordered.iter().enumerate() {
        let reviewer = m.effective_reviewer(&campaign.defaults);
        let flm = m.effective_fix_loop_max(&campaign.defaults);

        out.push_str(&format!("  {}. [{}]\n", idx + 1, m.id,));
        out.push_str(&format!("     branch      : {}\n", m.branch));
        out.push_str(&format!("     implementer : {}\n", m.implementer));
        out.push_str(&format!("     reviewer    : {}\n", reviewer));
        out.push_str(&format!("     fix_loop_max: {}\n", flm));
        if !m.depends_on.is_empty() {
            out.push_str(&format!("     depends_on  : {}\n", m.depends_on.join(", ")));
        }
        if !m.override_rules.is_empty() {
            out.push_str(&format!(
                "     override_rules: {} rule(s)\n",
                m.override_rules.len()
            ));
        }
    }

    out
}
