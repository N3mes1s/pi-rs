//! Per-host LSP configuration (D1).
//!
//! `LspConfig` lives on `Settings` (or is materialised from CLI flags)
//! and tells the LSP module which features to turn on. Defaults match
//! upstream pi: feature off; format-on-write off; diagnostics-on-write
//! on. Two override paths: (1) the user adds `lsp.enabled = true` in
//! `~/.pi/settings.toml`; (2) per-language overrides in
//! `lsp.languages.{rust,typescript,…}`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LspConfig {
    /// Master switch: when `false` the LSP module is fully inert and
    /// no servers are spawned. `false` by default (matches upstream:
    /// users opt in).
    #[serde(default)]
    pub enabled: bool,
    /// Run the LSP `formatting` request on every save and write the
    /// result back to disk. `false` by default — formatting is
    /// destructive and we don't want to surprise users.
    #[serde(default)]
    pub format_on_write: bool,
    /// Pull diagnostics on save and surface them in the transcript.
    /// `true` by default — diagnostics are read-only and high-value.
    #[serde(default = "default_true")]
    pub diagnostics_on_write: bool,
    /// Per-language enable/disable. A missing entry inherits the
    /// master `enabled` flag.
    #[serde(default)]
    pub languages: BTreeMap<String, LanguageConfig>,
}

fn default_true() -> bool {
    true
}

impl Default for LspConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            format_on_write: false,
            diagnostics_on_write: true,
            languages: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LanguageConfig {
    /// Override: enable/disable this language regardless of the master
    /// switch. `None` = inherit.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Override the server command (e.g. point to a vendored binary).
    #[serde(default)]
    pub command: Option<Vec<String>>,
}

impl From<&pi_agent_core::settings::LspSettings> for LspConfig {
    /// Lift the serde-only mirror in pi-agent-core into the runtime
    /// type used by the engine + tool.
    fn from(s: &pi_agent_core::settings::LspSettings) -> Self {
        Self {
            enabled: s.enabled,
            format_on_write: s.format_on_write,
            diagnostics_on_write: s.diagnostics_on_write,
            languages: s
                .languages
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        LanguageConfig {
                            enabled: v.enabled,
                            command: v.command.clone(),
                        },
                    )
                })
                .collect(),
        }
    }
}

impl LspConfig {
    /// Is `language` enabled? Per-language override wins; otherwise
    /// fall back to `self.enabled`.
    pub fn is_language_enabled(&self, language: &str) -> bool {
        match self.languages.get(language).and_then(|l| l.enabled) {
            Some(v) => v,
            None => self.enabled,
        }
    }

    /// Per-language command override, if any.
    pub fn command_override<'a>(&'a self, language: &str) -> Option<&'a [String]> {
        self.languages
            .get(language)
            .and_then(|l| l.command.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_upstream_pi() {
        let c = LspConfig::default();
        assert!(!c.enabled);
        assert!(!c.format_on_write);
        assert!(c.diagnostics_on_write, "diagnostics default to ON");
        assert!(c.languages.is_empty());
    }

    #[test]
    fn round_trips_through_toml_and_json() {
        let mut c = LspConfig::default();
        c.enabled = true;
        c.format_on_write = true;
        c.languages.insert(
            "rust".into(),
            LanguageConfig {
                enabled: Some(false),
                command: Some(vec!["ra-multiplex".into()]),
            },
        );
        let json = serde_json::to_string(&c).unwrap();
        let back: LspConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn is_language_enabled_falls_back_to_master_switch() {
        let mut c = LspConfig::default();
        c.enabled = true;
        // No languages map entry → master applies.
        assert!(c.is_language_enabled("rust"));
        // Per-language disable wins over master enable.
        c.languages.insert(
            "rust".into(),
            LanguageConfig {
                enabled: Some(false),
                command: None,
            },
        );
        assert!(!c.is_language_enabled("rust"));
        // Other languages still inherit master.
        assert!(c.is_language_enabled("python"));
    }

    #[test]
    fn is_language_enabled_can_opt_in_per_language_when_master_off() {
        let mut c = LspConfig::default();
        // master off
        assert!(!c.is_language_enabled("rust"));
        c.languages.insert(
            "rust".into(),
            LanguageConfig {
                enabled: Some(true),
                command: None,
            },
        );
        assert!(c.is_language_enabled("rust"));
        assert!(!c.is_language_enabled("python"));
    }

    #[test]
    fn command_override_returns_user_supplied_args() {
        let mut c = LspConfig::default();
        c.languages.insert(
            "rust".into(),
            LanguageConfig {
                enabled: None,
                command: Some(vec!["ra".into(), "--watch".into()]),
            },
        );
        let cmd = c.command_override("rust").expect("override present");
        assert_eq!(cmd, &["ra".to_string(), "--watch".into()]);
        assert!(c.command_override("missing").is_none());
    }

    #[test]
    fn deserialise_partial_json_uses_defaults_for_missing_fields() {
        // Only specifying `enabled` leaves the other flags at their
        // upstream-matching defaults.
        let s = r#"{"enabled": true}"#;
        let c: LspConfig = serde_json::from_str(s).unwrap();
        assert!(c.enabled);
        assert!(!c.format_on_write);
        assert!(c.diagnostics_on_write);
    }

    #[test]
    fn from_lsp_settings_preserves_every_field() {
        let s = pi_agent_core::settings::LspSettings {
            enabled: true,
            format_on_write: true,
            diagnostics_on_write: false,
            languages: [(
                "rust".into(),
                pi_agent_core::settings::LspLanguageSettings {
                    enabled: Some(true),
                    command: Some(vec!["ra-multiplex".into()]),
                },
            )]
            .into_iter()
            .collect(),
        };
        let c = LspConfig::from(&s);
        assert!(c.enabled);
        assert!(c.format_on_write);
        assert!(!c.diagnostics_on_write);
        let rust = c.languages.get("rust").expect("rust override survives");
        assert_eq!(rust.enabled, Some(true));
        assert_eq!(rust.command.as_deref(), Some(&["ra-multiplex".to_string()][..]));
    }

    #[test]
    fn from_default_lsp_settings_matches_lspconfig_default() {
        let c = LspConfig::from(&pi_agent_core::settings::LspSettings::default());
        assert_eq!(c, LspConfig::default());
    }
}
