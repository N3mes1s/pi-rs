use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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
    /// When true, model changes via `Ctrl+L` apply to just the *next* user
    /// message and revert afterward. When false (default), changes persist
    /// for the rest of the session.
    #[serde(default)]
    pub scoped_models: bool,
    /// Cheap-model routing. Each role is a model id (a plain `"haiku"`
    /// alias or a fully-qualified `"provider/model"`). When unset, the
    /// caller falls back to [`Settings::model`].
    #[serde(default)]
    pub roles: ModelRoles,
    /// Autonomous AGENTS.md evolution daemon settings.
    #[serde(default)]
    pub evolve: EvolveSettings,
    /// Native LSP integration settings (D1 / H5). Mirror of
    /// `pi_coding_agent::native::lsp::LspConfig` — kept in this crate to
    /// avoid a dependency cycle. The coding-agent crate converts via
    /// `From<&LspSettings>` at startup.
    #[serde(default)]
    pub lsp: LspSettings,
    /// Subagent (`task` tool) settings — fan-out cap and per-agent
    /// model overrides. See `pi_coding_agent::native::task` (RFD 0005).
    #[serde(default)]
    pub task: TaskSettings,
}

/// Configuration for the `task` tool / subagent system. Lives here so
/// `Settings` is self-contained; consumed by
/// `pi_coding_agent::native::task`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskSettings {
    /// Default subagent fan-out cap (matches oh-my-pi).
    #[serde(default = "default_max_concurrency")]
    pub max_concurrency: usize,
    /// Per-agent overrides — agent name → model id/alias.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub agent_models: BTreeMap<String, String>,
}

impl Default for TaskSettings {
    fn default() -> Self {
        Self {
            max_concurrency: default_max_concurrency(),
            agent_models: BTreeMap::new(),
        }
    }
}

fn default_max_concurrency() -> usize {
    5
}

/// Configuration for the autonomous evolution loop (G8).
///
/// Defaults are conservative: enabled by default, modest daily cost
/// cap, large minimum-sample threshold so the loop only runs after
/// the user has accumulated enough trajectory data for meaningful
/// reflection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvolveSettings {
    /// Master switch. When `false`, no tick runs; trajectory recording
    /// continues as normal.
    #[serde(default = "default_evolve_enabled")]
    pub enabled: bool,
    /// Hard $ cap per cwd per UTC day. Tick refuses to spend more.
    #[serde(default = "default_daily_cost_cap")]
    pub daily_cost_cap_usd: f32,
    /// Minimum number of outcome-labelled (non-Replay) trajectories
    /// before the first tick fires.
    #[serde(default = "default_min_samples")]
    pub min_samples: u32,
    /// Number of generations per tick. Each generation mutates one
    /// section of one candidate.
    #[serde(default = "default_generations_per_tick")]
    pub generations_per_tick: u32,
    /// Cap on benchmark cases per generation. More = better signal,
    /// linearly more cost.
    #[serde(default = "default_benchmark_size")]
    pub benchmark_size: u32,
    /// Minimum hours between successful ticks for a given cwd.
    #[serde(default = "default_min_hours_between_ticks")]
    pub min_hours_between_ticks: u32,
    /// New outcome-labelled trajectories required to re-fire the tick
    /// before the time threshold.
    #[serde(default = "default_min_new_outcomes")]
    pub min_new_outcomes_to_retick: u32,
}

impl Default for EvolveSettings {
    fn default() -> Self {
        Self {
            enabled: default_evolve_enabled(),
            daily_cost_cap_usd: default_daily_cost_cap(),
            min_samples: default_min_samples(),
            generations_per_tick: default_generations_per_tick(),
            benchmark_size: default_benchmark_size(),
            min_hours_between_ticks: default_min_hours_between_ticks(),
            min_new_outcomes_to_retick: default_min_new_outcomes(),
        }
    }
}

fn default_evolve_enabled() -> bool {
    true
}
fn default_daily_cost_cap() -> f32 {
    0.50
}
fn default_min_samples() -> u32 {
    30
}
fn default_generations_per_tick() -> u32 {
    3
}
fn default_benchmark_size() -> u32 {
    10
}
fn default_min_hours_between_ticks() -> u32 {
    24
}
fn default_min_new_outcomes() -> u32 {
    5
}

/// Role-based model routing. Lets the user pick a different cheap model
/// for short / structured / planning tasks without changing the default.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelRoles {
    /// Optional override of `Settings::model`. Most callers ignore this
    /// field — `Settings::model` is the canonical default — but we keep
    /// it so a `roles` block can be self-contained in JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// Tiny model for cheap structured calls (auto-approve judge,
    /// summary generation, classification).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smol: Option<String>,
    /// Slow / large-context model used when reasoning depth matters more
    /// than latency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slow: Option<String>,
    /// Planning model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// Commit-message model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

/// Named cheap-model role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Default,
    Smol,
    Slow,
    Plan,
    Commit,
}

impl Role {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "default" | "" => Some(Role::Default),
            "smol" => Some(Role::Smol),
            "slow" => Some(Role::Slow),
            "plan" => Some(Role::Plan),
            "commit" => Some(Role::Commit),
            _ => None,
        }
    }
}

impl ModelRoles {
    /// Resolve `role` to a model id, falling back to `default_model` when
    /// the role-specific override is not set.
    pub fn resolve<'a>(&'a self, role: Role, default_model: &'a str) -> &'a str {
        let opt = match role {
            Role::Default => self.default.as_deref(),
            Role::Smol => self.smol.as_deref(),
            Role::Slow => self.slow.as_deref(),
            Role::Plan => self.plan.as_deref(),
            Role::Commit => self.commit.as_deref(),
        };
        opt.unwrap_or(default_model)
    }
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
            scoped_models: false,
            roles: ModelRoles::default(),
            evolve: EvolveSettings::default(),
            lsp: LspSettings::default(),
            task: TaskSettings::default(),
        }
    }
}

fn default_provider() -> String {
    "anthropic".into()
}

/// Mirror of `pi_coding_agent::native::lsp::LspConfig`. Lives in
/// pi-agent-core so `Settings` can hold it without taking a dependency
/// on the coding-agent crate. The defaults here must stay in sync with
/// `LspConfig::default()` (master switch off, format-on-write off,
/// diagnostics-on-write on).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LspSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub format_on_write: bool,
    #[serde(default = "default_lsp_diagnostics_on_write")]
    pub diagnostics_on_write: bool,
    #[serde(default)]
    pub languages: std::collections::BTreeMap<String, LspLanguageSettings>,
}

impl Default for LspSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            format_on_write: false,
            diagnostics_on_write: default_lsp_diagnostics_on_write(),
            languages: std::collections::BTreeMap::new(),
        }
    }
}

fn default_lsp_diagnostics_on_write() -> bool {
    true
}

/// Per-language override block for `LspSettings`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct LspLanguageSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    /// Override the `FormattingOptions` block sent with
    /// `textDocument/formatting`. Missing fields inherit the hardcoded
    /// engine defaults (tab_size=4, insert_spaces=true, trim/newline=true).
    /// RFD 0007.
    #[serde(default, skip_serializing_if = "FormattingOptions::is_empty")]
    pub format_options: FormattingOptions,
}

/// Per-language override of LSP `FormattingOptions` (LSP 3.17 §3.17.13).
/// Each `None` field falls back to the engine default. RFD 0007.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct FormattingOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_size: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insert_spaces: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trim_trailing_whitespace: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insert_final_newline: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trim_final_newlines: Option<bool>,
}

impl FormattingOptions {
    pub fn is_empty(&self) -> bool {
        self.tab_size.is_none()
            && self.insert_spaces.is_none()
            && self.trim_trailing_whitespace.is_none()
            && self.insert_final_newline.is_none()
            && self.trim_final_newlines.is_none()
    }
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

    /// Persist `self` as pretty JSON, creating parent directories if needed.
    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let txt = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, txt)
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
