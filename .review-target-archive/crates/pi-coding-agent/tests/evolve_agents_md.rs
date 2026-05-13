//! Tests for the AGENTS.md parser (G5).

use pi_coding_agent::evolve::AgentsMd;

#[test]
fn empty_input_yields_empty_doc() {
    let doc = AgentsMd::parse("");
    assert!(doc.preamble.is_empty());
    assert!(doc.sections.is_empty());
    assert_eq!(doc.render(), "");
}

#[test]
fn document_without_h2_is_all_preamble() {
    let src = "# Title\n\nNo H2 in here.\nJust prose.\n";
    let doc = AgentsMd::parse(src);
    assert_eq!(doc.preamble, src);
    assert!(doc.sections.is_empty());
    assert_eq!(doc.render(), src);
}

#[test]
fn splits_on_h2_and_round_trips() {
    let src = "# Title\n\nIntro line.\n\n## A\nbody-a line 1\nbody-a line 2\n\n## B\nbody-b\n";
    let doc = AgentsMd::parse(src);
    assert_eq!(doc.preamble, "# Title\n\nIntro line.\n\n");
    assert_eq!(doc.sections.len(), 2);
    assert_eq!(doc.sections[0].heading, "## A\n");
    assert_eq!(doc.sections[0].body, "body-a line 1\nbody-a line 2\n\n");
    assert_eq!(doc.sections[1].heading, "## B\n");
    assert_eq!(doc.sections[1].body, "body-b\n");
    assert_eq!(doc.render(), src);
}

#[test]
fn no_preamble_when_doc_starts_with_h2() {
    let src = "## Only\nbody\n";
    let doc = AgentsMd::parse(src);
    assert!(doc.preamble.is_empty());
    assert_eq!(doc.sections.len(), 1);
    assert_eq!(doc.render(), src);
}

#[test]
fn h1_does_not_trigger_split() {
    let src = "## A\nbefore\n# Big heading\nstill in A\n## B\nbody-b\n";
    let doc = AgentsMd::parse(src);
    assert_eq!(doc.sections.len(), 2);
    assert!(doc.sections[0].body.contains("# Big heading"));
}

#[test]
fn h3_does_not_trigger_split() {
    let src = "## A\n### sub\nstill in A\n## B\nbody-b\n";
    let doc = AgentsMd::parse(src);
    assert_eq!(doc.sections.len(), 2);
    assert!(doc.sections[0].body.contains("### sub"));
}

#[test]
fn pi_keep_marker_makes_section_immutable() {
    let src = "## House rules\n<!-- pi:keep -->\nNever push to main.\n<!-- /pi:keep -->\n## Tools\nuse cargo\n";
    let doc = AgentsMd::parse(src);
    assert_eq!(doc.sections.len(), 2);
    assert!(!doc.sections[0].mutable);
    assert!(doc.sections[1].mutable);
    let mutables: Vec<_> = doc.mutable_sections().collect();
    assert_eq!(mutables.len(), 1);
    assert_eq!(mutables[0].0, 1);
}

#[test]
fn pi_keep_open_without_close_does_not_protect() {
    // Mismatched marker — section stays mutable.
    let src = "## A\n<!-- pi:keep -->\noops, no close marker.\n";
    let doc = AgentsMd::parse(src);
    assert!(doc.sections[0].mutable);
}

#[test]
fn replace_mutates_only_body_and_preserves_others() {
    let src = "## A\nold-a\n## B\nold-b\n";
    let mut doc = AgentsMd::parse(src);
    doc.replace_section(0, "new-a body\n".into()).unwrap();
    assert_eq!(doc.sections[0].body, "new-a body\n");
    assert_eq!(doc.sections[0].heading, "## A\n", "heading preserved");
    assert_eq!(doc.sections[1].body, "old-b\n", "B unchanged");
    assert_eq!(doc.render(), "## A\nnew-a body\n## B\nold-b\n");
}

#[test]
fn replace_immutable_section_errors() {
    let src = "## A\n<!-- pi:keep -->\nlocked\n<!-- /pi:keep -->\n## B\nbody\n";
    let mut doc = AgentsMd::parse(src);
    let err = doc.replace_section(0, "new".into());
    assert!(err.is_err());
    // Original body still there.
    assert!(doc.sections[0].body.contains("locked"));
}

#[test]
fn replace_out_of_range_errors() {
    let mut doc = AgentsMd::parse("## A\nbody\n");
    let err = doc.replace_section(5, "x".into());
    assert!(err.is_err());
}

#[test]
fn replace_can_introduce_keep_marker_to_freeze_section() {
    let src = "## A\nbody\n## B\nother\n";
    let mut doc = AgentsMd::parse(src);
    doc.replace_section(
        0,
        "<!-- pi:keep -->\nfrozen now\n<!-- /pi:keep -->\n".into(),
    )
    .unwrap();
    assert!(!doc.sections[0].mutable);
    let err = doc.replace_section(0, "back".into());
    assert!(err.is_err(), "newly-frozen section refuses replacement");
}

#[test]
fn hash_changes_when_body_changes() {
    let mut doc = AgentsMd::parse("## A\nv1\n");
    let h1 = doc.hash();
    doc.replace_section(0, "v2\n".into()).unwrap();
    let h2 = doc.hash();
    assert_ne!(h1, h2);
    // Hash is deterministic for the same content.
    let doc2 = AgentsMd::parse("## A\nv1\n");
    assert_eq!(doc2.hash(), h1);
}

#[test]
fn round_trip_real_world_agents_md() {
    // Sample resembling a real AGENTS.md.
    let src = r#"# pi-rs

This repo's coding agent guidance.

## Setup

Run `cargo build --workspace`.
Run `cargo test --workspace --no-fail-fast`.

## House rules

<!-- pi:keep -->
- Never use `--no-verify` on commits.
- Never force-push to main.
<!-- /pi:keep -->

## Tools

Prefer dedicated tools (Read, Edit, Grep) over the bash shell.

## Style

Default to no comments. Only write a comment when the *why* isn't obvious.
"#;
    let doc = AgentsMd::parse(src);
    assert_eq!(doc.sections.len(), 4);
    assert_eq!(doc.sections[0].heading.trim(), "## Setup");
    assert!(!doc.sections[1].mutable, "House rules is pi:keep");
    assert!(doc.sections[2].mutable, "Tools is mutable");
    assert!(doc.sections[3].mutable, "Style is mutable");
    assert_eq!(doc.render(), src);
}

#[test]
fn doc_without_trailing_newline_round_trips() {
    let src = "## A\nbody";
    let doc = AgentsMd::parse(src);
    assert_eq!(doc.render(), src);
}
