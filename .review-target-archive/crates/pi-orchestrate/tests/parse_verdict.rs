//! Integration tests for `parse_verdict` — exercises the public API from
//! outside the crate against the kinds of reviewer outputs the canary
//! actually produces (multi-section reviews, concerns blocks, etc.).

use pi_orchestrate::verdict::{parse_verdict, MergeReadiness};

// ──────────────────────────────────────────────────
// Happy-path: each of the three verdicts on the last line
// ──────────────────────────────────────────────────

#[test]
fn ready_to_merge_last_line() {
    let review = "\
## Summary\n\
Looks good overall.\n\
\n\
## Concerns\n\
None notable.\n\
\n\
Merge readiness: READY_TO_MERGE\n";
    assert_eq!(parse_verdict(review), Some(MergeReadiness::Ready));
}

#[test]
fn needs_fix_with_concerns_body() {
    let review = "\
## Summary\n\
The diff is mostly fine but has one issue.\n\
\n\
## Concerns\n\
- Missing error handling in foo::bar.\n\
- Docs are thin.\n\
\n\
Merge readiness: NEEDS_FIX\n";
    assert_eq!(parse_verdict(review), Some(MergeReadiness::NeedsFix));
}

#[test]
fn do_not_merge_verdict() {
    let review = "\
## Summary\n\
This introduces a security regression.\n\
\n\
Merge readiness: DO_NOT_MERGE\n";
    assert_eq!(parse_verdict(review), Some(MergeReadiness::DoNotMerge));
}

// ──────────────────────────────────────────────────
// Verdict NOT on the last non-empty line → None
// ──────────────────────────────────────────────────

#[test]
fn verdict_buried_in_prose_not_last_line_returns_none() {
    // Reviewer mentions the phrase mid-text but does not produce a final
    // verdict line — caller must treat as NeedsFix (fallback mode).
    let review = "\
## Summary\n\
A passing run would show 'Merge readiness: READY_TO_MERGE' but there\n\
are unresolved concerns I have yet to enumerate below.\n";
    assert_eq!(parse_verdict(review), None);
}

#[test]
fn prose_mention_then_final_line_wins() {
    // The *last* non-empty line is authoritative; an earlier prose
    // mention of a different verdict must not win.
    let review = "\
I previously indicated 'Merge readiness: NEEDS_FIX', but after the\n\
re-spin that concern is addressed.\n\
\n\
Merge readiness: READY_TO_MERGE\n";
    assert_eq!(parse_verdict(review), Some(MergeReadiness::Ready));
}

// ──────────────────────────────────────────────────
// Case sensitivity
// ──────────────────────────────────────────────────

#[test]
fn lowercase_prefix_not_accepted() {
    // `merge readiness:` (all-lowercase) is not the spec'd format.
    assert_eq!(parse_verdict("merge readiness: READY_TO_MERGE"), None);
}

#[test]
fn lowercase_verdict_keyword_not_accepted() {
    // Verdict keyword must be all-caps per spec.
    assert_eq!(
        parse_verdict("Merge readiness: ready_to_merge"),
        None
    );
}

// ──────────────────────────────────────────────────
// Edge cases
// ──────────────────────────────────────────────────

#[test]
fn trailing_blank_lines_ignored() {
    let review = "Merge readiness: DO_NOT_MERGE\n\n\n   \n\t\n";
    assert_eq!(parse_verdict(review), Some(MergeReadiness::DoNotMerge));
}

#[test]
fn extra_whitespace_around_keyword_tolerated() {
    let review = "## Concerns\nnone\n\nMerge readiness:    NEEDS_FIX   \n";
    assert_eq!(parse_verdict(review), Some(MergeReadiness::NeedsFix));
}

#[test]
fn empty_input_returns_none() {
    assert_eq!(parse_verdict(""), None);
}

#[test]
fn unknown_verdict_keyword_returns_none() {
    assert_eq!(parse_verdict("Merge readiness: SHIP_IT"), None);
}
