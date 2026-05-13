use crate::router::RouteMode;
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
    /// `monitor` tool settings (RFD 0017).
    #[serde(default)]
    pub monitor: MonitorSettings,
    #[serde(default)]
    pub route: RouteMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_provider_override: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_model_override: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_thinking_override: Option<crate::settings::ThinkingSetting>,
}

/// `monitor` tool configuration (RFD 0017). Caps + batching window for
/// long-running background-watch processes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MonitorSettings {
    /// Hard cap on concurrently-active monitors per session.
    #[serde(default = "default_monitor_max_concurrent")]
    pub max_concurrent: usize,
    /// Stdout-line batching window in milliseconds. Lines emitted within
    /// the same window become one notification.
    #[serde(default = "default_monitor_batch_window_ms")]
    pub batch_window_ms: u64,
    /// Volume guardrail: if a monitor emits more than `volume_cap_lines`
    /// lines within `volume_cap_window_ms` ms, the runtime auto-stops it.
    #[serde(default = "default_monitor_volume_cap_lines")]
    pub volume_cap_lines: usize,
    #[serde(default = "default_monitor_volume_cap_window_ms")]
    pub volume_cap_window_ms: u64,
    /// Default `timeout_ms` when not persistent.
    #[serde(default = "default_monitor_timeout_ms")]
    pub default_timeout_ms: u64,
    /// Hard ceiling on `timeout_ms`.
    #[serde(default = "default_monitor_max_timeout_ms")]
    pub max_timeout_ms: u64,
}

impl Default for MonitorSettings {
    fn default() -> Self {
        Self {
            max_concurrent: default_monitor_max_concurrent(),
            batch_window_ms: default_monitor_batch_window_ms(),
            volume_cap_lines: default_monitor_volume_cap_lines(),
            volume_cap_window_ms: default_monitor_volume_cap_window_ms(),
            default_timeout_ms: default_monitor_timeout_ms(),
            max_timeout_ms: default_monitor_max_timeout_ms(),
        }
    }
}

fn default_monitor_max_concurrent() -> usize {
    8
}
fn default_monitor_batch_window_ms() -> u64 {
    200
}
fn default_monitor_volume_cap_lines() -> usize {
    100
}
fn default_monitor_volume_cap_window_ms() -> u64 {
    5_000
}
fn default_monitor_timeout_ms() -> u64 {
    300_000
}
fn default_monitor_max_timeout_ms() -> u64 {
    3_600_000
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
            monitor: MonitorSettings::default(),
            route: RouteMode::Auto,
            route_provider_override: None,
            route_model_override: None,
            route_thinking_override: None,
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
    /// Maximum reasoning effort. On OpenAI Responses-API models
    /// (gpt-5.x, o-series) this maps to `effort: "xhigh"`. On
    /// Anthropic and Bedrock providers it clamps to `High`.
    XHigh,
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
    /// Begin constructing a `Settings` via the fluent builder. Per
    /// RFD 0027 §3 + pass-1 #8 polish track: `Settings` is on the
    /// list of POD types that should eventually be marked
    /// `#[non_exhaustive]` (forcing struct-literal callers off the
    /// "spread defaults" pattern). Adding the builder is the
    /// additive prerequisite; the `#[non_exhaustive]` mark itself
    /// lands at 1.0 (per MIGRATION.md).
    ///
    /// The builder covers the **commonly-set** fields directly:
    /// `provider`, `model`, `thinking`, `compact_threshold`,
    /// `theme`, `route`. For the long tail of less-common fields
    /// (LSP, monitor, evolve, task settings, etc.) the builder
    /// exposes [`SettingsBuilder::with`] — an escape hatch that
    /// gives the caller a `&mut Settings` to mutate any field.
    /// The result of `build()` is a `Settings` materialised from
    /// `Settings::default()` with the chosen overrides applied.
    pub fn builder() -> SettingsBuilder {
        SettingsBuilder::default()
    }

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
            ThinkingSetting::XHigh => pi_ai::ThinkingLevel::XHigh,
        }
    }
}

// ─── SettingsBuilder (RFD 0027 §3 + pass-1 #8 polish) ────────────

/// Fluent builder for [`Settings`]. See [`Settings::builder`] for
/// the rationale.
///
/// All setters are optional; `build()` materialises a `Settings`
/// from `Settings::default()` with the chosen overrides applied.
/// Setters return `Self` (consume-and-return) for chain ergonomics.
///
/// For the long tail of fields not surfaced as named setters, use
/// [`with`](Self::with) — the escape hatch that hands the caller a
/// `&mut Settings` to mutate any field.
// SettingsBuilder is one-shot (consumed by `build`); not Clone
// because the `with` mutator closures are FnOnce, which is not
// itself Clone.
#[derive(Default)]
pub struct SettingsBuilder {
    provider: Option<String>,
    model: Option<String>,
    thinking: Option<ThinkingSetting>,
    compact_threshold: Option<f32>,
    theme: Option<String>,
    route: Option<crate::router::RouteMode>,
    no_tools: Option<bool>,
    /// Closures collected via `with(...)`. Applied in order at
    /// `build()` time after the named-setter overrides land.
    #[allow(clippy::type_complexity)]
    mutators: Vec<Box<dyn FnOnce(&mut Settings) + 'static>>,
}

impl SettingsBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the default provider (e.g. `"anthropic"`).
    pub fn provider<S: Into<String>>(mut self, p: S) -> Self {
        self.provider = Some(p.into());
        self
    }

    /// Set the default model id or alias.
    pub fn model<S: Into<String>>(mut self, m: S) -> Self {
        self.model = Some(m.into());
        self
    }

    /// Set the default thinking level.
    pub fn thinking(mut self, t: ThinkingSetting) -> Self {
        self.thinking = Some(t);
        self
    }

    /// Set the auto-compact threshold (fraction of context window
    /// remaining; auto-compact triggers when below this value).
    /// Default 0.15.
    pub fn compact_threshold(mut self, t: f32) -> Self {
        self.compact_threshold = Some(t);
        self
    }

    /// Set the TUI theme name. Empty = pi default.
    pub fn theme<S: Into<String>>(mut self, name: S) -> Self {
        self.theme = Some(name.into());
        self
    }

    /// Set the routing mode (RFD 0020).
    ///
    /// **Per-route overrides** (`route_provider_override`,
    /// `route_model_override`, `route_thinking_override`) are not
    /// surfaced as named setters per code-review pass-10 NIT #4 —
    /// embedders setting them go through the [`with`](Self::with)
    /// escape hatch:
    ///
    /// ```ignore
    /// let s = Settings::builder()
    ///     .route(RouteMode::Static)
    ///     .with(|s| {
    ///         s.route_provider_override = Some("anthropic".into());
    ///         s.route_model_override = Some("claude-haiku-4-5-20251001".into());
    ///     })
    ///     .build();
    /// ```
    pub fn route(mut self, r: crate::router::RouteMode) -> Self {
        self.route = Some(r);
        self
    }

    /// Disable all tool registration.
    pub fn no_tools(mut self, v: bool) -> Self {
        self.no_tools = Some(v);
        self
    }

    /// Escape hatch for fields that don't have named setters
    /// (LSP, monitor, evolve, task, role overrides, etc.). The
    /// closure is applied to a mutable `Settings` after the
    /// named-setter overrides land. Multiple `with(...)` calls
    /// run in registration order.
    ///
    /// Example:
    /// ```ignore
    /// let s = Settings::builder()
    ///     .provider("anthropic")
    ///     .with(|s| s.evolve.enabled = false)
    ///     .with(|s| s.task.max_concurrency = 10)
    ///     .build();
    /// ```
    pub fn with<F: FnOnce(&mut Settings) + 'static>(mut self, f: F) -> Self {
        self.mutators.push(Box::new(f));
        self
    }

    /// Materialise the final `Settings`. Always succeeds — every
    /// field has a sensible default.
    pub fn build(self) -> Settings {
        let mut s = Settings::default();
        if let Some(v) = self.provider { s.provider = v; }
        if let Some(v) = self.model { s.model = v; }
        if let Some(v) = self.thinking { s.thinking = v; }
        if let Some(v) = self.compact_threshold { s.compact_threshold = v; }
        if let Some(v) = self.theme { s.theme = v; }
        if let Some(v) = self.route { s.route = v; }
        if let Some(v) = self.no_tools { s.no_tools = v; }
        for f in self.mutators {
            f(&mut s);
        }
        s
    }
}

#[cfg(test)]
mod settings_builder_tests {
    use super::*;

    #[test]
    fn builder_round_trips_named_fields() {
        let s = Settings::builder()
            .provider("anthropic")
            .model("claude-haiku-4-5-20251001")
            .thinking(ThinkingSetting::Medium)
            .compact_threshold(0.25)
            .theme("solarized")
            .no_tools(true)
            .build();
        assert_eq!(s.provider, "anthropic");
        assert_eq!(s.model, "claude-haiku-4-5-20251001");
        assert_eq!(s.thinking, ThinkingSetting::Medium);
        assert!((s.compact_threshold - 0.25).abs() < f32::EPSILON);
        assert_eq!(s.theme, "solarized");
        assert!(s.no_tools);
    }

    #[test]
    fn builder_defaults_match_settings_default() {
        let s = Settings::builder().build();
        let d = Settings::default();
        assert_eq!(s.provider, d.provider);
        assert_eq!(s.model, d.model);
        assert_eq!(s.thinking, d.thinking);
        assert_eq!(s.compact_threshold, d.compact_threshold);
    }

    #[test]
    fn with_closures_run_in_order() {
        let s = Settings::builder()
            .provider("openai")
            .with(|s| s.task.max_concurrency = 7)
            .with(|s| s.evolve.enabled = false)
            .build();
        assert_eq!(s.provider, "openai");
        assert_eq!(s.task.max_concurrency, 7);
        assert!(!s.evolve.enabled);
    }

    #[test]
    fn named_setter_then_with_closure_applies_both() {
        let s = Settings::builder()
            .compact_threshold(0.5)
            .with(|s| s.compact_threshold = 0.75)
            .build();
        // `with` runs after named-setters, so 0.75 wins.
        assert!((s.compact_threshold - 0.75).abs() < f32::EPSILON);
    }
}
