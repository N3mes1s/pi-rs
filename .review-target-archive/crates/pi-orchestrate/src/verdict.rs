//! Reviewer verdict parser (v1 — verdict-line-only).
//!
//! RFD 0021 §"Reviewer parser contract" specs both a structured mode
//! (concerns extracted from `## Concerns` bullets, fed to override-rule
//! evaluation) and a fallback mode (no `## Concerns` → whole verdict
//! text appended to next implementer turn). v1 implements the
//! verdict-line extraction only — concerns extraction lands in v2 with
//! the override-rule machinery.
//!
//! What v1 needs from the verdict text:
//!   * the final `Merge readiness:` line, parsed into one of three
//!     enum values
//!   * everything else (concerns body) is preserved verbatim and, on
//!     NEEDS_FIX, appended to the implementer's next turn — same
//!     effect as RFD's "fallback mode" but unconditional in v1.

/// The three possible reviewer verdicts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeReadiness {
    Ready,
    NeedsFix,
    DoNotMerge,
}

impl MergeReadiness {
    pub fn as_str(self) -> &'static str {
        match self {
            MergeReadiness::Ready => "READY_TO_MERGE",
            MergeReadiness::NeedsFix => "NEEDS_FIX",
            MergeReadiness::DoNotMerge => "DO_NOT_MERGE",
        }
    }
}

/// Parse the reviewer's verdict text per RFD 0021 §"Reviewer parser
/// contract": **the final non-empty line must match
/// `^Merge readiness:\s*(READY_TO_MERGE|NEEDS_FIX|DO_NOT_MERGE)\s*$`**.
/// Anything else is fallback mode, returning `None`. Callers MUST
/// treat fallback as NeedsFix — we never silently promote to Ready.
///
/// Why "final line" not "last matching line": a reviewer whose actual
/// verdict line is missing or unparseable but whose prose mentions
/// the phrase earlier (e.g. "the spec says the reviewer ends with
/// `Merge readiness: READY_TO_MERGE`") would have promoted under the
/// previous "last matching line wins" parser. That's the RFD-spec
/// violation flagged in the orchestrator v1 review (B4).
pub fn parse_verdict(text: &str) -> Option<MergeReadiness> {
    // Find the final non-empty trimmed line.
    let final_line = text.lines().rev().find_map(|l| {
        let t = l.trim();
        if t.is_empty() {
            None
        } else {
            Some(t)
        }
    })?;
    let rest = final_line.strip_prefix("Merge readiness:")?;
    match rest.trim() {
        "READY_TO_MERGE" => Some(MergeReadiness::Ready),
        "NEEDS_FIX" => Some(MergeReadiness::NeedsFix),
        "DO_NOT_MERGE" => Some(MergeReadiness::DoNotMerge),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ready() {
        assert_eq!(
            parse_verdict("ok\n\nMerge readiness: READY_TO_MERGE"),
            Some(MergeReadiness::Ready)
        );
    }

    #[test]
    fn parses_needs_fix() {
        assert_eq!(
            parse_verdict("Merge readiness: NEEDS_FIX\n"),
            Some(MergeReadiness::NeedsFix)
        );
    }

    #[test]
    fn parses_do_not_merge() {
        assert_eq!(
            parse_verdict("## Summary\nstuff\nMerge readiness: DO_NOT_MERGE\n"),
            Some(MergeReadiness::DoNotMerge)
        );
    }

    #[test]
    fn final_line_is_authoritative_even_with_earlier_prose_mention() {
        // A reviewer that mentions the phrase in prose context, then
        // gives the real final verdict, parses to the FINAL line.
        let v = "earlier I said 'Merge readiness: NEEDS_FIX' but actually\n\
                 Merge readiness: READY_TO_MERGE\n";
        assert_eq!(parse_verdict(v), Some(MergeReadiness::Ready));
    }

    #[test]
    fn prose_mention_without_final_verdict_returns_none() {
        // Regression for B4 in the v1 review: the previous parser
        // ("last matching line wins") would have wrongly promoted
        // this to Ready because it found the phrase mid-text. The
        // RFD says the FINAL non-empty line must be the verdict —
        // anything else is fallback mode. Returning None here forces
        // the caller (runner) to treat as NeedsFix and bump the
        // fix-loop counter, which is the correct behaviour.
        let v = "I'd normally say 'Merge readiness: READY_TO_MERGE' here, but\n\
                 there are concerns I haven't finished writing up below.\n";
        assert_eq!(parse_verdict(v), None);
    }

    #[test]
    fn trailing_blank_lines_dont_confuse_parser() {
        let v = "Merge readiness: READY_TO_MERGE\n\n\n   \n";
        assert_eq!(parse_verdict(v), Some(MergeReadiness::Ready));
    }

    #[test]
    fn missing_line_returns_none() {
        assert_eq!(
            parse_verdict("the reviewer forgot to include the line"),
            None
        );
    }

    #[test]
    fn unknown_verdict_word_returns_none() {
        // RFD: a verdict word outside the three legal values fails the
        // line — fallback path. Don't guess.
        assert_eq!(parse_verdict("Merge readiness: PROBABLY_OK"), None);
    }

    #[test]
    fn whitespace_tolerated() {
        assert_eq!(
            parse_verdict("Merge readiness:    READY_TO_MERGE   "),
            Some(MergeReadiness::Ready)
        );
    }

    #[test]
    fn case_sensitive_keyword() {
        // `merge readiness:` (lowercase) is not the spec'd format and
        // we don't guess.
        assert_eq!(parse_verdict("merge readiness: READY_TO_MERGE"), None);
    }
}
