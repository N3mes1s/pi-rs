use pi_orchestrate::{parse_campaign, topological_order, validate};

/// Diamond DAG: m1 → m2, m1 → m3; m2 → m4, m3 → m4.
const DIAMOND_TOML: &str = r#"
name          = "diamond dag test"
target_branch = "main"

[[milestones]]
id          = "m1"
branch      = "pi/m1"
depends_on  = []
assignment  = "..."
implementer = "agent"

[[milestones]]
id          = "m2"
branch      = "pi/m2"
depends_on  = ["m1"]
assignment  = "..."
implementer = "agent"

[[milestones]]
id          = "m3"
branch      = "pi/m3"
depends_on  = ["m1"]
assignment  = "..."
implementer = "agent"

[[milestones]]
id          = "m4"
branch      = "pi/m4"
depends_on  = ["m2", "m3"]
assignment  = "..."
implementer = "agent"
"#;

#[test]
fn topological_order_diamond() {
    let campaign = parse_campaign(DIAMOND_TOML).expect("should parse");
    validate(&campaign).expect("should validate");

    let order = topological_order(&campaign);
    assert_eq!(order.len(), 4, "all 4 milestones must appear");

    let ids: Vec<&str> = order.iter().map(|m| m.id.as_str()).collect();

    // m1 must be first.
    assert_eq!(ids[0], "m1", "m1 (root) must come first; got {:?}", ids);

    // m4 must be last.
    assert_eq!(ids[3], "m4", "m4 (leaf) must come last; got {:?}", ids);

    // m2 and m3 must appear in between (either order).
    let mid: std::collections::HashSet<&str> = ids[1..3].iter().copied().collect();
    assert!(
        mid.contains("m2") && mid.contains("m3"),
        "m2 and m3 must both appear between m1 and m4; got {:?}",
        ids
    );
}

#[test]
fn topological_order_linear_chain() {
    const LINEAR: &str = r#"
name          = "linear"
target_branch = "main"

[[milestones]]
id          = "a"
branch      = "pi/a"
depends_on  = []
assignment  = "..."
implementer = "agent"

[[milestones]]
id          = "b"
branch      = "pi/b"
depends_on  = ["a"]
assignment  = "..."
implementer = "agent"

[[milestones]]
id          = "c"
branch      = "pi/c"
depends_on  = ["b"]
assignment  = "..."
implementer = "agent"
"#;

    let campaign = parse_campaign(LINEAR).expect("should parse");
    validate(&campaign).expect("should validate");
    let order = topological_order(&campaign);
    let ids: Vec<&str> = order.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["a", "b", "c"]);
}
