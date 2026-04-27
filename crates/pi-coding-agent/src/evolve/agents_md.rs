//! Parse AGENTS.md (or any H2-delimited markdown) into mutable modules.
//!
//! The evolver mutates one [`Section`] at a time. Sections are split by
//! ATX `## ` H2 headers — H1 (`#`) and deeper (`###+`) are preserved as
//! body text. Anything before the first H2 becomes the document
//! `preamble` and is never mutated.
//!
//! `<!-- pi:keep -->` markers protect a section from mutation:
//!
//! ```markdown
//! ## House rules
//! <!-- pi:keep -->
//! Never use --no-verify. Never push to main.
//! <!-- /pi:keep -->
//! ```
//!
//! When the marker pair appears anywhere in a section's body, the whole
//! section is treated as immutable. The marker text is preserved on
//! render. (Partial protection within a section is intentionally not
//! supported — keeps the mutation surface clean.)
//!
//! Round-trip guarantee: `AgentsMd::render(AgentsMd::parse(s)) == s` for
//! any input `s` whose H2 headers begin at column 0 and whose line
//! endings are LF. CRLF-normalised input round-trips after one pass.

use serde::{Deserialize, Serialize};

const KEEP_OPEN: &str = "<!-- pi:keep -->";
const KEEP_CLOSE: &str = "<!-- /pi:keep -->";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentsMd {
    /// Text before the first H2. Preserved verbatim; never mutated.
    pub preamble: String,
    pub sections: Vec<Section>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Section {
    /// Full heading line, including `##` and trailing newline. Empty for
    /// the synthetic "trailing" section that holds text after the last
    /// real H2 if there were no further headers — but that's just the
    /// last section's body, so heading is always non-empty for a real
    /// parse.
    pub heading: String,
    /// Body of the section: everything between this heading and the
    /// next H2 (or end of file). Includes the trailing newline that
    /// preceded the next H2, so concatenation is exact.
    pub body: String,
    /// True iff this section is eligible for mutation. False when the
    /// body contains a `<!-- pi:keep -->...<!-- /pi:keep -->` pair.
    pub mutable: bool,
}

impl AgentsMd {
    /// Parse a markdown document.
    pub fn parse(text: &str) -> Self {
        let mut preamble = String::new();
        let mut sections: Vec<Section> = Vec::new();
        let mut current: Option<(String, String)> = None;

        for line in line_iter(text) {
            if is_h2_header(&line) {
                if let Some((heading, body)) = current.take() {
                    sections.push(Section::build(heading, body));
                }
                current = Some((line, String::new()));
            } else if let Some((_, body)) = current.as_mut() {
                body.push_str(&line);
            } else {
                preamble.push_str(&line);
            }
        }
        if let Some((heading, body)) = current {
            sections.push(Section::build(heading, body));
        }

        Self { preamble, sections }
    }

    pub fn render(&self) -> String {
        let mut out = String::with_capacity(
            self.preamble.len() + self.sections.iter().map(|s| s.heading.len() + s.body.len()).sum::<usize>(),
        );
        out.push_str(&self.preamble);
        for s in &self.sections {
            out.push_str(&s.heading);
            out.push_str(&s.body);
        }
        out
    }

    /// Iterator over (index, section) pairs eligible for mutation.
    pub fn mutable_sections(&self) -> impl Iterator<Item = (usize, &Section)> {
        self.sections
            .iter()
            .enumerate()
            .filter(|(_, s)| s.mutable)
    }

    /// Replace section `idx`'s body. Heading is left untouched. Mutable
    /// flag is recomputed from the new body so a mutation that adds a
    /// `pi:keep` marker takes effect immediately.
    pub fn replace_section(&mut self, idx: usize, new_body: String) -> Result<(), ReplaceError> {
        let s = self
            .sections
            .get_mut(idx)
            .ok_or(ReplaceError::IndexOutOfRange(idx))?;
        if !s.mutable {
            return Err(ReplaceError::Immutable(idx));
        }
        s.body = new_body;
        s.mutable = !contains_keep_pair(&s.body);
        Ok(())
    }

    /// Stable digest of the rendered content. Used for the
    /// EvolveMarker.agents_md_hash field.
    pub fn hash(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(self.render().as_bytes());
        format!("{:x}", hasher.finalize())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReplaceError {
    #[error("section index {0} out of range")]
    IndexOutOfRange(usize),
    #[error("section {0} is marked pi:keep and cannot be mutated")]
    Immutable(usize),
}

impl Section {
    fn build(heading: String, body: String) -> Self {
        let mutable = !contains_keep_pair(&body);
        Self {
            heading,
            body,
            mutable,
        }
    }
}

fn contains_keep_pair(body: &str) -> bool {
    let Some(open) = body.find(KEEP_OPEN) else {
        return false;
    };
    body[open + KEEP_OPEN.len()..].contains(KEEP_CLOSE)
}

fn is_h2_header(line: &str) -> bool {
    // ATX H2: line starts with `## ` followed by something. Must NOT be
    // `### ` (deeper).
    if !line.starts_with("## ") {
        return false;
    }
    if line.starts_with("### ") {
        return false;
    }
    true
}

/// Iterate over `text` in chunks that include the line terminator. Empty
/// chunks at end-of-text are preserved so render() round-trips
/// trailing-newline state exactly.
fn line_iter(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    for ch in text.chars() {
        buf.push(ch);
        if ch == '\n' {
            out.push(std::mem::take(&mut buf));
        }
    }
    if !buf.is_empty() {
        out.push(buf);
    }
    out
}
