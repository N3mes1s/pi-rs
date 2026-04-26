use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Settings persisted to `~/.pi/agent/settings.json` (with project overrides
/// in `.pi/settings.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Default provider name.
    #[serde(default = "default_provider")]
    pub provider: String,
    /// Default model id or alias.
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default)]
    pub thinking: ThinkingSetting,
    #[serde(default = "default_steering_mode")]
    pub steering_mode: QueueMode,
    #[serde(default = "default_steering_mode")]
    pub follow_up_mode: QueueMode,
    #[serde(default = "default_transport")]
    pub transport: Transport,
    #[serde(default)]
    pub theme: String,
    /// Auto-compact when remaining context drops below this fraction.
    #[serde(default = "default_compact_threshold")]
    pub compact_threshold: f32,
    /// Allowlist of tool names. Empty = all builtins.
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub no_builtin_tools: bool,
    #[serde(default)]
    pub no_tools: bool,
    #[serde(default)]
    pub session_dir: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            model: default_model(),
            thinking: ThinkingSetting::Off,
            steering_mode: default_steering_mode(),
            follow_up_mode: default_steering_mode(),
            transport: default_transport(),
            theme: "dark".into(),
            compact_threshold: default_compact_threshold(),
            tools: Vec::new(),
            no_builtin_tools: false,
            no_tools: false,
            session_dir: None,
        }
    }
}

fn default_provider() -> String {
    "anthropic".into()
}

fn default_model() -> String {
    "sonnet".into()
}

fn default_steering_mode() -> QueueMode {
    QueueMode::OneAtATime
}

fn default_transport() -> Transport {
    Transport::Auto
}

fn default_compact_threshold() -> f32 {
    0.15
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingSetting {
    #[default]
    Off,
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QueueMode {
    OneAtATime,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    Sse,
    Websocket,
    Auto,
}

impl Settings {
    pub fn load(path: &std::path::Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn merge_project(&mut self, project_path: &std::path::Path) {
        if let Ok(s) = std::fs::read_to_string(project_path) {
            if let Ok(p) = serde_json::from_str::<serde_json::Value>(&s) {
                let merged = merge_json(serde_json::to_value(&self).unwrap_or_default(), p);
                if let Ok(s) = serde_json::from_value::<Settings>(merged) {
                    *self = s;
                }
            }
        }
    }
}

fn merge_json(mut a: serde_json::Value, b: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match (&mut a, b) {
        (Value::Object(am), Value::Object(bm)) => {
            for (k, v) in bm {
                let existing = am.remove(&k).unwrap_or(Value::Null);
                am.insert(k, merge_json(existing, v));
            }
            a
        }
        (_, b) => b,
    }
}

impl From<ThinkingSetting> for pi_ai::ThinkingLevel {
    fn from(v: ThinkingSetting) -> Self {
        match v {
            ThinkingSetting::Off => pi_ai::ThinkingLevel::Off,
            ThinkingSetting::Low => pi_ai::ThinkingLevel::Low,
            ThinkingSetting::Medium => pi_ai::ThinkingLevel::Medium,
            ThinkingSetting::High => pi_ai::ThinkingLevel::High,
        }
    }
}
