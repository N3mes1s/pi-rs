use clap::{ArgAction, Parser};
use std::path::PathBuf;

/// `pi` — minimal terminal coding agent harness (Rust port).
#[derive(Parser, Debug, Clone)]
#[command(name = "pi", version, about, long_about = None)]
pub struct Cli {
    /// Provider to use (anthropic, openai, deepseek, groq, …).
    #[arg(long, env = "PI_PROVIDER")]
    pub provider: Option<String>,

    /// Model id or alias (e.g. `sonnet`, `gpt-4o`, `anthropic/claude-opus-4-7`).
    #[arg(long, short = 'm')]
    pub model: Option<String>,

    /// Comma-separated list of models to make cyclable in interactive mode.
    #[arg(long)]
    pub models: Option<String>,

    /// Thinking level: off, low, medium, high.
    #[arg(long, value_parser = clap::builder::PossibleValuesParser::new(["off","low","medium","high"]))]
    pub thinking: Option<String>,

    /// Allowlist of tool names. Comma-separated.
    #[arg(long)]
    pub tools: Option<String>,

    /// Disable built-in tools (extensions still register).
    #[arg(long, action = ArgAction::SetTrue)]
    pub no_builtin_tools: bool,

    /// Disable all tools.
    #[arg(long, action = ArgAction::SetTrue)]
    pub no_tools: bool,

    /// Print mode — non-interactive. Reads stdin if piped, prints final reply.
    #[arg(long, short = 'p', action = ArgAction::SetTrue)]
    pub print: bool,

    /// JSON event stream (implies print).
    #[arg(long, action = ArgAction::SetTrue)]
    pub json: bool,

    /// RPC mode — bidirectional JSONL on stdin/stdout.
    #[arg(long, action = ArgAction::SetTrue)]
    pub rpc: bool,

    /// Continue most recent session.
    #[arg(short = 'c', action = ArgAction::SetTrue)]
    pub continue_recent: bool,

    /// Resume with a session selector.
    #[arg(short = 'r', action = ArgAction::SetTrue)]
    pub resume: bool,

    /// Use a specific session id or path.
    #[arg(long)]
    pub session: Option<String>,

    /// Fork a session into a new one.
    #[arg(long)]
    pub fork: Option<String>,

    /// Disable session persistence (ephemeral).
    #[arg(long, action = ArgAction::SetTrue)]
    pub no_session: bool,

    /// Override session directory.
    #[arg(long)]
    pub session_dir: Option<PathBuf>,

    /// Disable AGENTS.md / CLAUDE.md auto-loading.
    #[arg(long = "no-context-files", short = 'n', action = ArgAction::SetTrue)]
    pub no_context_files: bool,

    /// Override AGENTS.md content with the contents of `<path>`. Skips
    /// the normal cwd/ancestors/global discovery — only this file is
    /// fed to the model. Used by the evolution daemon to benchmark
    /// candidate AGENTS.md files in-place without touching the live one.
    #[arg(long = "agents-md", value_name = "PATH")]
    pub agents_md: Option<PathBuf>,

    /// Disable extension loading.
    #[arg(long, action = ArgAction::SetTrue)]
    pub no_extensions: bool,

    /// Add an extension at a specific path.
    #[arg(long = "extension", short = 'e', value_name = "PATH")]
    pub extensions: Vec<PathBuf>,

    /// Add a skill at a specific path.
    #[arg(long = "skill", value_name = "PATH")]
    pub skills: Vec<PathBuf>,

    /// Use a prompt template by name or `@path`.
    #[arg(long)]
    pub prompt_template: Option<String>,

    /// Theme name (`dark`, `light`, or any installed theme).
    #[arg(long)]
    pub theme: Option<String>,

    /// Subcommand: install / list / config / update.
    #[arg(long)]
    pub install: Option<String>,

    #[arg(long, action = ArgAction::SetTrue)]
    pub list: bool,

    #[arg(long = "config", action = ArgAction::SetTrue)]
    pub config_subcommand: bool,

    #[arg(long, action = ArgAction::SetTrue)]
    pub update: bool,

    /// Run live model discovery against every provider with credentials,
    /// merge results into `~/.pi/agent/discovered-models.json`, and exit.
    #[arg(long = "refresh-models", action = ArgAction::SetTrue)]
    pub refresh_models: bool,

    /// AGENTS.md auto-evolution control: `status` (print state),
    /// `off` (disable for cwd), `on` (re-enable for cwd).
    #[arg(long = "evolve", value_parser = clap::builder::PossibleValuesParser::new(["status", "off", "on"]))]
    pub evolve: Option<String>,

    /// Render a trajectory flamegraph for a session id (or path) to
    /// HTML and exit.
    #[arg(long = "flamegraph", value_name = "SESSION_OR_PATH")]
    pub flamegraph: Option<String>,

    /// Internal: run one autonomous evolve tick for the cwd and exit.
    /// Spawned by the modes/ exit hooks; not meant for direct user
    /// invocation. Hidden from help.
    #[arg(long = "internal-evolve-tick", action = ArgAction::SetTrue, hide = true)]
    pub internal_evolve_tick: bool,

    /// Auto-approval mode: `ask` (default), `auto-policy`, `auto-judge`,
    /// or `yolo`. Policy file at `~/.pi/agent/auto-approve.json` is
    /// always consulted first.
    #[arg(long = "auto-approve")]
    pub auto_approve: Option<String>,

    /// Override the judge model (only effective with `auto-judge`).
    #[arg(long = "auto-approve-model")]
    pub auto_approve_model: Option<String>,

    /// Cheap "smol" role model id (e.g. `haiku`, `gpt-4o-mini`,
    /// `provider/model`). Overrides `settings.roles.smol`.
    #[arg(long, env = "PI_SMOL_MODEL")]
    pub smol: Option<String>,

    /// Slow / heavyweight reasoning role model id.
    /// Overrides `settings.roles.slow`.
    #[arg(long, env = "PI_SLOW_MODEL")]
    pub slow: Option<String>,

    /// Planning role model id. Overrides `settings.roles.plan`.
    #[arg(long, env = "PI_PLAN_MODEL")]
    pub plan: Option<String>,

    /// Free-form positional args. `@file` references add attachments.
    #[arg(value_name = "MESSAGE_OR_AT_FILES")]
    pub positionals: Vec<String>,
}

impl Cli {
    pub fn prompt_text(&self) -> Option<String> {
        let parts: Vec<String> = self
            .positionals
            .iter()
            .filter(|p| !p.starts_with('@'))
            .cloned()
            .collect();
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" "))
        }
    }

    pub fn at_files(&self) -> Vec<PathBuf> {
        self.positionals
            .iter()
            .filter_map(|p| p.strip_prefix('@').map(|s| PathBuf::from(s)))
            .collect()
    }

    pub fn effective_mode(&self) -> Mode {
        if self.rpc {
            Mode::Rpc
        } else if self.json {
            Mode::Json
        } else if self.print {
            Mode::Print
        } else {
            Mode::Interactive
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Interactive,
    Print,
    Json,
    Rpc,
}
