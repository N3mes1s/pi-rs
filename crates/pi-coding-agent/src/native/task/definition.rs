//! Subagent definition: a Markdown file with YAML frontmatter, parsed
//! into [`AgentDefinition`]. Mirrors `oh-my-pi`'s shape exactly so users
//! can copy-paste agents between the two implementations.
//!
//! The body of the markdown file (everything after the closing `---`) is
//! stored in [`AgentDefinition::system_prompt`]; the frontmatter goes
//! through `serde_yaml` into the typed fields.

use serde::Deserialize;
use std::path::PathBuf;

/// Where a definition came from. Discovery merges the three sources
/// with `Project > User > Bundled` precedence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum AgentSource {
    #[default]
    Bundled,
    User,
    Project,
}

/// Recursive spawn restriction. `None` (= field omitted) means *no*
/// nested `task` calls; `All` means unrestricted; `Named` allowlists
/// a set of agent names by exact match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnsRule {
    All,
    Named(Vec<String>),
}

impl SpawnsRule {
    pub fn allows(&self, name: &str) -> bool {
        match self {
            SpawnsRule::All => true,
            SpawnsRule::Named(v) => v.iter().any(|n| n == name),
        }
    }
}

/// Custom (de)serialisation: accepts either a single string `"*"` /
/// `"explore"` or a list of strings. Mirrors oh-my-pi.
impl<'de> Deserialize<'de> for SpawnsRule {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;
        let v = serde_yaml::Value::deserialize(d)?;
        match v {
            serde_yaml::Value::String(s) => {
                let s = s.trim().to_string();
                if s == "*" {
                    Ok(SpawnsRule::All)
                } else if s.is_empty() {
                    Ok(SpawnsRule::Named(Vec::new()))
                } else {
                    Ok(SpawnsRule::Named(vec![s]))
                }
            }
            serde_yaml::Value::Sequence(seq) => {
                let mut out = Vec::with_capacity(seq.len());
                for item in seq {
                    match item {
                        serde_yaml::Value::String(s) => out.push(s),
                        other => {
                            return Err(D::Error::custom(format!(
                                "spawns: expected string, got {other:?}"
                            )))
                        }
                    }
                }
                Ok(SpawnsRule::Named(out))
            }
            other => Err(D::Error::custom(format!(
                "spawns: expected string or list, got {other:?}"
            ))),
        }
    }
}

/// Subagent definition (parsed frontmatter + body).
///
/// `deny_unknown_fields` matches the RFD: typo'd keys must surface as a
/// parse error, not silently degrade to a misconfigured agent. The body
/// of the markdown file is injected into [`AgentDefinition::system_prompt`]
/// after frontmatter parsing — that's why it's `#[serde(skip)]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentDefinition {
    pub name: String,
    pub description: String,
    /// Body of the markdown file after the frontmatter delimiter.
    #[serde(skip)]
    pub system_prompt: String,
    /// Allowlist of tool names. Empty/omitted = inherit parent registry.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Recursive spawn restriction. `None` = field omitted entirely;
    /// `Some(All)` = `"*"`; `Some(Named(_))` = explicit list/single name.
    #[serde(default)]
    pub spawns: Option<SpawnsRule>,
    /// Settings-style model spec (role alias or `provider/model`).
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub thinking: Option<String>,
    /// Optional output schema (forwarded only — v1 does not validate).
    #[serde(default)]
    pub output: Option<serde_json::Value>,

    /// Discovery metadata.
    #[serde(skip)]
    pub source: AgentSource,
    #[serde(skip)]
    pub file_path: Option<PathBuf>,
}

/// Errors produced while parsing a Markdown-with-frontmatter file.
#[derive(Debug, thiserror::Error)]
pub enum DefinitionParseError {
    #[error("missing frontmatter delimiter `---`")]
    MissingFrontmatter,
    #[error("yaml: {0}")]
    Yaml(#[from] serde_yaml::Error),
}

impl AgentDefinition {
    /// Parse a `name.md` file into an `AgentDefinition`. The file MUST
    /// open with `---\n` and contain a closing `---` line; everything
    /// after the closing delimiter becomes [`system_prompt`].
    pub fn parse(text: &str) -> Result<Self, DefinitionParseError> {
        let (front, body) = split_frontmatter(text).ok_or(DefinitionParseError::MissingFrontmatter)?;
        let mut def: AgentDefinition = serde_yaml::from_str(front)?;
        def.system_prompt = body.trim().to_string();
        Ok(def)
    }
}

/// Split `---\n…\n---\n…body…` into (frontmatter, body). Returns
/// `None` if the leading or trailing delimiter is missing.
fn split_frontmatter(text: &str) -> Option<(&str, &str)> {
    // Accept either `---\n` or `---\r\n` openings; tolerate a leading BOM.
    let stripped = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rest = stripped
        .strip_prefix("---\n")
        .or_else(|| stripped.strip_prefix("---\r\n"))?;
    // Find the closing delimiter on its own line.
    for (idx, line) in rest.match_indices('\n') {
        // `idx` is the position of '\n'; the line before starts after the
        // previous '\n'. Walk by lines instead.
        let _ = (idx, line);
        break;
    }
    // Simpler implementation: split on lines and find a line equal to "---".
    let mut byte = 0usize;
    for line in rest.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed == "---" {
            let front = &rest[..byte];
            let body_start = byte + line.len();
            let body = &rest[body_start..];
            return Some((front, body));
        }
        byte += line.len();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_works() {
        let s = "---\nname: x\n---\nbody\nmore\n";
        let (f, b) = split_frontmatter(s).unwrap();
        assert_eq!(f.trim(), "name: x");
        assert!(b.starts_with("body"));
    }
}
