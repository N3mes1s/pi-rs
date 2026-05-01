//! Unit tests for `plan::format_plan` (RFD 0021).
//!
//! Covers:
//!  1. Empty milestone list — headers still emitted.
//!  2. Linear DAG (a → b → c) — execution order reflected in output.
//!  3. Diamond DAG (m1 → {m2, m3} → m4) — m1 first, m4 last.
//!  4. Cyclic campaign — format_plan produces *some* output without panicking
//!     (topological_order silently drops nodes involved in cycles; that is an
//!     accepted behaviour documented for already-validated campaigns).

use pi_orchestrate::{format_plan, parse_campaign};

// ── helpers ──────────────────────────────────────────────────────────────────

fn parse(toml: &str) -> pi_orchestrate::Campaign {
    parse_campaign(toml).expect("TOML should parse")
}

// ── 1. Empty campaign ─────────────────────────────────────────────────────────

const EMPTY_TOML: &str = r#"
name          = "empty campaign"
description   = "no milestones at all"
target_branch = "main"
"#;

#[test]
fn format_plan_empty_campaign() {
    let campaign = parse(EMPTY_TOML);
    let out = format_plan(&campaign);

    assert!(
        out.contains("=== Orchestrate dry-run plan ==="),
        "must have header; got:\n{out}"
    );
    assert!(
        out.contains("Campaign : empty campaign"),
        "must have campaign name; got:\n{out}"
    );
    assert!(
        out.contains("Description: no milestones at all"),
        "must have description when non-empty; got:\n{out}"
    );
    assert!(
        out.contains("Target branch: main"),
        "must have target branch; got:\n{out}"
    );
    assert!(
        out.contains("Execution order:"),
        "must have execution-order section; got:\n{out}"
    );
    // No numbered milestone lines expected.
    assert!(
        !out.contains("  1."),
        "empty campaign must have no numbered entries; got:\n{out}"
    );
}

#[test]
fn format_plan_omits_description_when_empty() {
    const TOML: &str = r#"
name          = "no-desc campaign"
target_branch = "main"
"#;
    let campaign = parse(TOML);
    let out = format_plan(&campaign);
    assert!(
        !out.contains("Description:"),
        "description line must be absent when description is empty; got:\n{out}"
    );
}

// ── 2. Linear DAG ─────────────────────────────────────────────────────────────

const LINEAR_TOML: &str = r#"
name          = "linear chain"
target_branch = "main"

[defaults]
reviewer      = "default-reviewer"
fix_loop_max  = 3

[[milestones]]
id          = "alpha"
branch      = "pi/alpha"
depends_on  = []
assignment  = "do alpha"
implementer = "impl-agent"

[[milestones]]
id          = "beta"
branch      = "pi/beta"
depends_on  = ["alpha"]
assignment  = "do beta"
implementer = "impl-agent"

[[milestones]]
id          = "gamma"
branch      = "pi/gamma"
depends_on  = ["beta"]
assignment  = "do gamma"
implementer = "impl-agent"
reviewer    = "custom-reviewer"
fix_loop_max = 1
"#;

#[test]
fn format_plan_linear_dag_order() {
    let campaign = parse(LINEAR_TOML);
    let out = format_plan(&campaign);

    // The three milestones must be present as numbered items in order.
    let pos_alpha = out.find("1. [alpha]").expect("alpha must be item 1");
    let pos_beta = out.find("2. [beta]").expect("beta must be item 2");
    let pos_gamma = out.find("3. [gamma]").expect("gamma must be item 3");

    assert!(
        pos_alpha < pos_beta && pos_beta < pos_gamma,
        "items must appear in topological order; positions: {pos_alpha}, {pos_beta}, {pos_gamma}"
    );
}

#[test]
fn format_plan_linear_dag_fields() {
    let campaign = parse(LINEAR_TOML);
    let out = format_plan(&campaign);

    // Default reviewer / fix_loop_max should be visible for alpha and beta.
    assert!(
        out.contains("reviewer    : default-reviewer"),
        "default reviewer must appear; got:\n{out}"
    );
    assert!(
        out.contains("fix_loop_max: 3"),
        "default fix_loop_max (3) must appear; got:\n{out}"
    );

    // Per-milestone overrides on gamma.
    assert!(
        out.contains("reviewer    : custom-reviewer"),
        "gamma's custom reviewer must appear; got:\n{out}"
    );
    assert!(
        out.contains("fix_loop_max: 1"),
        "gamma's fix_loop_max override (1) must appear; got:\n{out}"
    );

    // depends_on line should appear for beta and gamma but not alpha.
    // We check gamma's depends_on is printed correctly.
    assert!(
        out.contains("depends_on  : beta"),
        "gamma's depends_on must mention 'beta'; got:\n{out}"
    );
}

// ── 3. Diamond DAG ────────────────────────────────────────────────────────────

const DIAMOND_TOML: &str = r#"
name          = "diamond dag"
target_branch = "feature"

[[milestones]]
id          = "m1"
branch      = "pi/m1"
depends_on  = []
assignment  = "..."
implementer = "agent-a"

[[milestones]]
id          = "m2"
branch      = "pi/m2"
depends_on  = ["m1"]
assignment  = "..."
implementer = "agent-b"

[[milestones]]
id          = "m3"
branch      = "pi/m3"
depends_on  = ["m1"]
assignment  = "..."
implementer = "agent-c"

[[milestones]]
id          = "m4"
branch      = "pi/m4"
depends_on  = ["m2", "m3"]
assignment  = "..."
implementer = "agent-d"
"#;

#[test]
fn format_plan_diamond_dag_order() {
    let campaign = parse(DIAMOND_TOML);
    let out = format_plan(&campaign);

    let pos_m1 = out.find("1. [m1]").expect("m1 must be item 1");
    let pos_m4 = out.find("4. [m4]").expect("m4 must be item 4");
    // m2 and m3 are items 2 and 3 (either order).
    let pos_m2 = out.find("[m2]").expect("m2 must appear");
    let pos_m3 = out.find("[m3]").expect("m3 must appear");

    assert!(
        pos_m1 < pos_m2 && pos_m1 < pos_m3,
        "m1 (root) must come before m2 and m3"
    );
    assert!(
        pos_m2 < pos_m4 && pos_m3 < pos_m4,
        "m2 and m3 must both come before m4 (leaf)"
    );
}

#[test]
fn format_plan_diamond_dag_depends_on_lines() {
    let campaign = parse(DIAMOND_TOML);
    let out = format_plan(&campaign);

    // m1 has no depends_on → no depends_on line immediately after its entry.
    // We verify by checking that the line does NOT appear right after "1. [m1]".
    // Simple approach: count occurrences of "depends_on" lines.
    let dep_count = out.matches("depends_on  :").count();
    // m2, m3, m4 each have depends_on, m1 does not → expect exactly 3.
    assert_eq!(
        dep_count, 3,
        "exactly 3 milestones should have a depends_on line; got {dep_count}"
    );

    // m4's depends_on mentions both m2 and m3.
    assert!(
        out.contains("depends_on  : m2, m3"),
        "m4 depends_on must list both m2 and m3; got:\n{out}"
    );
}

// ── 4. Cyclic campaign ────────────────────────────────────────────────────────

const CYCLE_TOML: &str = r#"
name          = "cycle campaign"
target_branch = "main"

[[milestones]]
id          = "x"
branch      = "pi/x"
depends_on  = ["y"]
assignment  = "..."
implementer = "agent"

[[milestones]]
id          = "y"
branch      = "pi/y"
depends_on  = ["x"]
assignment  = "..."
implementer = "agent"
"#;

#[test]
fn format_plan_cycle_does_not_panic() {
    // validate() would reject this, but format_plan is not required to call
    // validate(). It should handle cycles gracefully (Kahn's drops all nodes in
    // cycles) and return a non-empty string without panicking.
    let campaign = parse(CYCLE_TOML);
    let out = format_plan(&campaign);

    assert!(
        !out.is_empty(),
        "format_plan must return non-empty output even for a cyclic campaign"
    );
    assert!(
        out.contains("=== Orchestrate dry-run plan ==="),
        "header must always be emitted; got:\n{out}"
    );
    // Both cyclic nodes are silently dropped by Kahn's algorithm.
    assert!(
        !out.contains("[x]") && !out.contains("[y]"),
        "cyclic nodes should not appear in the ordered output; got:\n{out}"
    );
}
