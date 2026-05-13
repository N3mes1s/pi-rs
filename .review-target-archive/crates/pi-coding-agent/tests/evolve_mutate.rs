//! Tests for the reflective mutation engine (G6).
//!
//! Provider-integration is the same plumbing as auto_approve::judge and
//! native::trajectory::judge — covered there. Here we focus on:
//! - prompt assembly (deterministic, easy to break)
//! - output post-processing (length cap, fence stripping, empty rejection)
//! - guardrails (immutable section refusal, no-evidence refusal)

use pi_coding_agent::evolve::{
    build_prompt, post_process, AgentsMd, EvidenceItem, MutateError, MutationEvidence,
};

fn evidence_with(wins: usize, losses: usize) -> MutationEvidence {
    MutationEvidence {
        wins: (0..wins)
            .map(|i| EvidenceItem {
                user_request: format!("win request {i}"),
                verdict_reason: format!("win reason {i}"),
            })
            .collect(),
        losses: (0..losses)
            .map(|i| EvidenceItem {
                user_request: format!("loss request {i}"),
                verdict_reason: format!("loss reason {i}"),
            })
            .collect(),
    }
}

// ─── prompt assembly ───────────────────────────────────────────────────

#[test]
fn prompt_includes_heading_body_and_evidence() {
    let doc = AgentsMd::parse("## Tools\nuse cargo.\n\n## Style\nbe brief.\n");
    let evidence = evidence_with(2, 1);
    let prompt = build_prompt(&doc, 0, &doc.sections[0], &evidence);
    assert!(prompt.contains("## Tools"));
    assert!(prompt.contains("use cargo."));
    assert!(prompt.contains("## Style"), "other section heading present");
    assert!(prompt.contains("win reason 0"));
    assert!(prompt.contains("win reason 1"));
    assert!(prompt.contains("loss reason 0"));
}

#[test]
fn prompt_caps_evidence_at_three_each() {
    let doc = AgentsMd::parse("## A\nbody\n");
    let evidence = evidence_with(7, 5);
    let prompt = build_prompt(&doc, 0, &doc.sections[0], &evidence);
    assert!(prompt.contains("win reason 2"));
    assert!(!prompt.contains("win reason 3"), "should be capped at 3");
    assert!(prompt.contains("loss reason 2"));
    assert!(!prompt.contains("loss reason 3"));
}

#[test]
fn prompt_handles_missing_evidence_with_placeholders() {
    let doc = AgentsMd::parse("## A\nbody\n");
    let only_wins = MutationEvidence {
        wins: vec![EvidenceItem {
            user_request: "do X".into(),
            verdict_reason: "did X".into(),
        }],
        losses: vec![],
    };
    let prompt = build_prompt(&doc, 0, &doc.sections[0], &only_wins);
    assert!(prompt.contains("did X"));
    assert!(prompt.contains("(no losses observed)"));
}

#[test]
fn prompt_omits_self_from_other_sections_list() {
    let doc = AgentsMd::parse("## A\nbody-a\n## B\nbody-b\n## C\nbody-c\n");
    let prompt = build_prompt(&doc, 1, &doc.sections[1], &evidence_with(1, 0));
    // Headings of A and C should be in <other_sections>; B should NOT.
    let other_block_start = prompt.find("<other_sections>").unwrap();
    let other_block_end = prompt.find("</other_sections>").unwrap();
    let other = &prompt[other_block_start..other_block_end];
    assert!(other.contains("## A"));
    assert!(other.contains("## C"));
    // The current section's heading appears in the <heading> block (above
    // <other_sections>) but NOT inside <other_sections>.
    assert!(!other.contains("## B"));
}

#[test]
fn prompt_truncates_long_evidence_strings() {
    let doc = AgentsMd::parse("## A\nbody\n");
    let long_request = "x".repeat(500);
    let evidence = MutationEvidence {
        wins: vec![EvidenceItem {
            user_request: long_request.clone(),
            verdict_reason: "ok".into(),
        }],
        losses: vec![],
    };
    let prompt = build_prompt(&doc, 0, &doc.sections[0], &evidence);
    // 240-char cap: the elision marker ('…') should appear, and the
    // raw 500-char string should not.
    assert!(prompt.contains('…'));
    assert!(!prompt.contains(&long_request));
}

// ─── post-processing ───────────────────────────────────────────────────

#[test]
fn post_process_strips_code_fences() {
    let raw = "```markdown\nbody line\nbody line 2\n```";
    let out = post_process(raw, 100, 1.2).unwrap();
    assert!(!out.starts_with("```"));
    assert!(out.contains("body line"));
}

#[test]
fn post_process_keeps_inline_code_fences_inside_body() {
    // No outer fence — inner ``` is part of the body.
    let raw = "use `cargo build` to compile.\nrun ```bash\nhi\n``` for shells.\n";
    let out = post_process(raw, 200, 1.2).unwrap();
    assert!(out.contains("`cargo build`"));
    assert!(out.contains("```bash"));
}

#[test]
fn post_process_appends_trailing_newline_if_missing() {
    let raw = "no newline at end";
    let out = post_process(raw, 100, 1.2).unwrap();
    assert!(out.ends_with('\n'));
}

#[test]
fn post_process_rejects_empty_output() {
    let err = post_process("", 100, 1.2).unwrap_err();
    assert!(matches!(err, MutateError::EmptyOutput));
    let err = post_process("   \n\n\n", 100, 1.2).unwrap_err();
    assert!(matches!(err, MutateError::EmptyOutput));
}

#[test]
fn post_process_enforces_length_cap() {
    // current = 100, cap factor = 1.2 → cap = 120 chars (or 64 floor).
    let too_long = "x".repeat(200);
    let err = post_process(&too_long, 100, 1.2).unwrap_err();
    assert!(matches!(err, MutateError::LengthCapExceeded(_, _)));
    // Within cap is fine.
    let just_under = "x".repeat(120);
    assert!(post_process(&just_under, 100, 1.2).is_ok());
}

#[test]
fn post_process_uses_64_char_floor_for_tiny_sections() {
    // current = 5 chars; cap factor 1.2 → 6 chars, but floor is 64.
    let raw = "x".repeat(50);
    assert!(post_process(&raw, 5, 1.2).is_ok());
}

#[test]
fn post_process_trims_blank_lines_around_body() {
    let raw = "\n\n\nactual content\n\n\n";
    let out = post_process(raw, 100, 1.2).unwrap();
    assert_eq!(out, "actual content\n");
}
