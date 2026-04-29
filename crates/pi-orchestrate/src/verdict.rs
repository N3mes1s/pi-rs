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

/// Parse the reviewer's verdict text. Returns `None` if no
/// `Merge readiness:` line is found (RFD's "fallback mode" trigger:
/// caller should treat as NeedsFix to be safe — we never silently
/// promote to Ready).
///
/// Match: case-sensitive, last-line-wins (so a reviewer that mentions
/// "Merge readiness: NEEDS_FIX" in prose context but ends with the
/// real verdict still parses correctly). Whitespace tolerated around
/// the verdict word.
pub fn parse_verdict(text: &str) -> Option<MergeReadiness> {
    let mut last: Option<MergeReadiness> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("Merge readiness:") else {
            continue;
        };
        let word = rest.trim();
        let parsed = match word {
            "READY_TO_MERGE" => Some(MergeReadiness::Ready),
            "NEEDS_FIX" => Some(MergeReadiness::NeedsFix),
            "DO_NOT_MERGE" => Some(MergeReadiness::DoNotMerge),
            _ => None,
        };
        if parsed.is_some() {
            last = parsed;
        }
    }
    last
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
    fn last_line_wins_when_repeated() {
        // A reviewer that mentions the phrase in prose context, then
        // gives the real final verdict, must parse as the LAST
        // occurrence.
        let v = "earlier I said 'Merge readiness: NEEDS_FIX' but actually\n\
                 Merge readiness: READY_TO_MERGE\n";
        assert_eq!(parse_verdict(v), Some(MergeReadiness::Ready));
    }

    #[test]
    fn missing_line_returns_none() {
        assert_eq!(parse_verdict("the reviewer forgot to include the line"), None);
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
