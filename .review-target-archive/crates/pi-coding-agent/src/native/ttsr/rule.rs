//! TTSR rule data model + filesystem loader.
//!
//! A rule lives in a `.md` file with YAML frontmatter:
//! ```text
//! ---
//! ttsrTrigger: '\bplan\b'
//! ---
//!
//! Stop and run the planning checklist.
//! ```
//!
//! The frontmatter format is intentionally minimal: it must start at
//! byte 0 with `---\n`, end at the next `---\n`, and contain a single
//! `ttsrTrigger:` key. We don't pull in serde_yaml for this — a
//! line-by-line parser is enough and avoids the dep.

use std::path::{Path, PathBuf};

/// One TTSR rule, parsed from a `.md` file with frontmatter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// Stable identifier — derived from the file's stem.
    pub name: String,
    /// Source regex (un-compiled). Stored so callers can show diagnostics.
    pub trigger_pattern: String,
    /// Body (Markdown), trimmed of leading/trailing whitespace.
    pub body: String,
    /// Path on disk, used in error messages.
    pub path: PathBuf,
}

/// Collection of [`Rule`]s with their pre-compiled regexes.
#[derive(Debug, Default)]
pub struct RuleSet {
    rules: Vec<(Rule, regex::Regex)>,
}

impl RuleSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn rules(&self) -> &[(Rule, regex::Regex)] {
        &self.rules
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Push a rule by compiling its trigger.
    pub fn push(&mut self, rule: Rule) -> Result<(), regex::Error> {
        let r = regex::Regex::new(&rule.trigger_pattern)?;
        self.rules.push((rule, r));
        Ok(())
    }

    /// Load every `*.md` rule out of `dir`. Files with bad frontmatter
    /// or invalid regex are *skipped silently* — TTSR rules are
    /// best-effort, we don't want one broken rule to break the agent.
    /// Returns the resulting [`RuleSet`].
    pub fn load_dir(dir: &Path) -> Self {
        let mut rs = RuleSet::new();
        let Ok(rd) = std::fs::read_dir(dir) else {
            return rs;
        };
        for ent in rd.flatten() {
            let p = ent.path();
            if p.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let Ok(body) = std::fs::read_to_string(&p) else {
                continue;
            };
            let name = p
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unnamed")
                .to_string();
            if let Some(rule) = parse_rule(name, body, &p) {
                let _ = rs.push(rule);
            }
        }
        rs
    }
}

/// Parse the frontmatter + body out of a single `.md` file.
pub fn parse_rule(name: String, raw: String, path: &Path) -> Option<Rule> {
    let mut lines = raw.lines();
    let first = lines.next()?;
    if first.trim() != "---" {
        return None;
    }
    let mut trigger: Option<String> = None;
    for line in lines.by_ref() {
        let trimmed = line.trim();
        if trimmed == "---" {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("ttsrTrigger:") {
            // Remove leading/trailing whitespace and surrounding quotes.
            let v = rest.trim();
            let v = v
                .strip_prefix('\'')
                .and_then(|x| x.strip_suffix('\''))
                .or_else(|| v.strip_prefix('"').and_then(|x| x.strip_suffix('"')))
                .unwrap_or(v);
            trigger = Some(v.to_string());
        }
    }
    let body: String = lines.collect::<Vec<&str>>().join("\n").trim().to_string();
    Some(Rule {
        name,
        trigger_pattern: trigger?,
        body,
        path: path.to_path_buf(),
    })
}

/// Default rule directory: `~/.pi/agent/ttsr/`.
pub fn default_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".pi").join("agent").join("ttsr"))
}

/// Format the rule body as a `<system_reminder>` user message that gets
/// re-injected into the conversation so the next assistant turn sees it.
pub fn render_reminder(rule: &Rule) -> String {
    format!(
        "<system_reminder name=\"{}\">\n{}\n</system_reminder>",
        rule.name, rule.body
    )
}
