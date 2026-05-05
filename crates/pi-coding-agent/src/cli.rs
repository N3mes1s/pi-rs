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

    /// Thinking level: off, low, medium, high, xhigh.
    /// `xhigh` maps to OpenAI Responses-API `effort:"xhigh"` for gpt-5.x;
    /// on Anthropic/Bedrock it clamps to `high`.
    #[arg(long, value_parser = clap::builder::PossibleValuesParser::new(["off","low","medium","high","xhigh"]))]
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

    /// Enable the RFD 0022 sandbox boundary. Currently supports:
    ///   `local-process` — invokes tools through pi_sandbox::LocalProcessProvider.
    #[arg(long = "sandbox-provider", value_name = "KIND")]
    pub sandbox_provider: Option<String>,

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

    /// Resume a specific session by id, or "most recent" when no id given.
    /// Examples:
    ///   pi -r                        # resume most-recently-touched session
    ///   pi -r 72d77a8d-2dbd-...      # resume that exact session
    /// Setting this AND positional message-args is supported: pi loads
    /// the named session and then sends the message into it.
    #[arg(short = 'r', long = "resume", num_args = 0..=1, default_missing_value = "")]
    pub resume: Option<String>,

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

    /// Sandbox subcommand. Currently supports:
    ///   `doctor` — probe the host for `microvm:firecracker`
    ///   prerequisites (KVM, firecracker, virtiofsd, vsock, plus
    ///   NetworkPolicy::Allow extras: pasta, nftables, unprivileged
    ///   userns) and print a per-check report. Exit 0 if everything
    ///   the configured provider needs is available; exit 1 if a
    ///   blocker is missing.
    #[arg(
        long = "sandbox",
        value_name = "VERB",
        value_parser = clap::builder::PossibleValuesParser::new(["doctor"])
    )]
    pub sandbox_subcommand: Option<String>,

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
    #[arg(long = "evolve", value_parser = clap::builder::PossibleValuesParser::new(["status", "off", "on", "dry-run", "apply"]))]
    pub evolve: Option<String>,

    /// Render a trajectory flamegraph for a session id (or path) to
    /// HTML and exit.
    #[arg(long = "flamegraph", value_name = "SESSION_OR_PATH")]
    pub flamegraph: Option<String>,

    /// Output format for `--flamegraph`: `html` (default; the self-
    /// contained dark-mode page) or `json` (agent-readable trajectory
    /// shape with per-turn blocks). RFD 0012.
    #[arg(
        long = "flamegraph-format",
        value_name = "FORMAT",
        value_parser = clap::builder::PossibleValuesParser::new(["html", "json"])
    )]
    pub flamegraph_format: Option<String>,

    /// Render a session as a self-contained HTML transcript and write
    /// it to `~/.pi/agent/shares/<id>.html` (path printed on stdout).
    #[arg(long = "share", value_name = "SESSION_OR_PATH")]
    pub share: Option<String>,

    /// Manage the auto-approve policy file at
    /// `~/.pi/agent/auto-approve.json`. Verbs:
    ///
    /// * `list` — pretty-print current policy.
    /// * `add  bash:<regex>` — append <regex> to the bash command_allow_regex.
    /// * `deny bash:<regex>` — append <regex> to the bash command_deny_regex.
    /// * `allow <tool>:<pattern>` — set always_approve=true for <tool>.
    /// * `remove <tool>:<entry>` — remove the first matching entry.
    #[arg(long = "policy", value_name = "VERB[ ARG]")]
    pub policy: Option<String>,

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

    /// Stats subcommand: `server` (default — bind dashboard on
    /// `--stats-port`), `sync` (ingest then exit), or `json` (ingest
    /// then dump `DashboardStats` to stdout). Short-circuits the
    /// agent loop. RFD 0004.
    #[arg(long = "stats", value_name = "VERB", num_args = 0..=1,
          default_missing_value = "server")]
    pub stats: Option<String>,

    /// Port for `--stats server` (default 3847).
    #[arg(long = "stats-port", default_value_t = 3847)]
    pub stats_port: u16,

    /// Monitor subcommand (RFD 0017): `list` (show active monitors,
    /// across-session diagnostics — v1 reports per-process state only)
    /// or `stop ID` (force-stop a monitor by id). Short-circuits the
    /// agent loop.
    #[arg(long = "monitor", value_name = "VERB", num_args = 0..=2)]
    pub monitor: Vec<String>,

    /// Free-form positional args. `@file` references add attachments.
    #[arg(value_name = "MESSAGE_OR_AT_FILES")]
    pub positionals: Vec<String>,

    /// Run this invocation inside a private git worktree (RFD 0006).
    /// The parent branch is not touched; on success, changes land on
    /// `pi/task/<id>` (or as a patch artifact when
    /// `--worktree-mode=patch`).
    #[arg(long, action = ArgAction::SetTrue)]
    pub worktree: bool,

    /// Route mode override: off, static, auto, learned.
    /// When omitted, pi keeps the value from settings.json (whose default
    /// is still `static`).
    #[arg(long = "route", value_parser = clap::builder::PossibleValuesParser::new(["off","static","auto","learned"]))]
    pub route: Option<String>,

    /// Reconciliation mode for `--worktree`: `branch` (default) or
    /// `patch`.
    #[arg(long = "worktree-mode", value_name = "MODE",
          value_parser = clap::builder::PossibleValuesParser::new(["branch", "patch"]))]
    pub worktree_mode: Option<String>,

    /// Explicit task id for `--worktree`. Defaults to a random UUID.
    #[arg(long = "worktree-id", value_name = "ID")]
    pub worktree_id: Option<String>,

    /// Parse + validate a campaign TOML and print the execution plan.
    /// Zero side effects. Exit 0 on success, exit 2 on validation errors.
    #[arg(long = "orchestrate-dry-run", value_name = "PATH")]
    pub orchestrate_dry_run: Option<PathBuf>,

    /// Run a campaign TOML: walk milestones in topo order, emit
    /// transitions to `<state-root>/<campaign>/state.jsonl`.
    ///
    /// For each milestone the runner (1) checks out the milestone
    /// branch, (2) dispatches an implementer subagent (subprocess;
    /// agent definition at `<repo>/.pi/agents/<name>.md` supplies
    /// model + prompt), (3) captures a review snapshot, then
    /// (4) dispatches a reviewer subagent. On `READY_TO_MERGE` the
    /// snapshotted commit is cherry-picked onto the target branch
    /// (or blocked if the target moved). On `NEEDS_FIX` the
    /// implementer is re-dispatched with the reviewer's feedback
    /// appended (up to `fix_loop_max` iterations; exhaustion →
    /// `FAILED`). On `DO_NOT_MERGE` the milestone is marked
    /// `FAILED` immediately.
    ///
    /// Exit 0 on success, 2 on validation/IO errors.
    #[arg(long = "orchestrate", value_name = "PATH")]
    pub orchestrate: Option<PathBuf>,

    /// Override the state root directory for `--orchestrate`.
    /// Defaults to `~/.pi/orchestrate/`.
    #[arg(long = "orchestrate-state-root", value_name = "PATH")]
    pub orchestrate_state_root: Option<PathBuf>,

    /// Wrap a `--orchestrate` run in a freshly-allocated git worktree.
    /// The worktree is seeded from HEAD of the current repo and removed
    /// (best-effort) when the run completes. Useful for isolating the
    /// orchestrator's `git checkout` calls from the operator's working
    /// tree. Has no effect when `--orchestrate` is not set.
    #[arg(long = "orchestrate-isolate", action = ArgAction::SetTrue)]
    pub orchestrate_isolate: bool,

    // ---- halo flags (RFD 0025 M1) ----

    /// Read-only snapshot of halo supervisor state. Exit 0. Use with
    /// --watch or --json.
    #[arg(long = "halo-status", action = ArgAction::SetTrue)]
    pub halo_status: bool,

    /// Config path for --halo-status (default <repo>/.pi/halo.toml).
    #[arg(long = "halo-config", value_name = "PATH")]
    pub halo_config: Option<PathBuf>,

    /// With --halo-status: re-render every 5s (like `top`).
    #[arg(long = "watch", action = ArgAction::SetTrue)]
    pub watch: bool,

    /// Write bundled halo agent files (halo-proposer.md, halo-implementer.md,
    /// code-reviewer.md) to <repo>/.pi/agents/ if they don't already exist,
    /// then exit. Tests the M1 bundled-agent bootstrap.
    #[arg(long = "halo-bootstrap-agents", action = ArgAction::SetTrue)]
    pub halo_bootstrap_agents: bool,

    /// Allow target_branch = "main" (normally refused by halo validator).
    #[arg(long = "halo-allow-main", action = ArgAction::SetTrue)]
    pub halo_allow_main: bool,

    /// Run halo in long-running supervisor mode. M2 runs `--halo-max-cycles`
    /// cycles (default 1) then exits. M3+ will run forever (until paused/stopped).
    #[arg(long = "halo", action = ArgAction::SetTrue)]
    pub halo: bool,

    /// Max cycles to run before exiting (default 1; 0 = unlimited).
    #[arg(long = "halo-max-cycles", value_name = "N", default_value_t = 1)]
    pub halo_max_cycles: u64,

    /// Add a proposal directly to the backlog.
    #[arg(long = "halo-add-proposal", action = ArgAction::SetTrue)]
    pub halo_add_proposal: bool,

    /// Drop a proposal by id.
    #[arg(long = "halo-drop-proposal", value_name = "ID")]
    pub halo_drop_proposal: Option<String>,

    /// Proposal title for --halo-add-proposal.
    #[arg(long = "title", value_name = "TITLE")]
    pub halo_title: Option<String>,

    /// Proposal rationale for --halo-add-proposal.
    #[arg(long = "rationale", value_name = "RATIONALE")]
    pub halo_rationale: Option<String>,

    /// CSV file list for --halo-add-proposal.
    #[arg(long = "files", value_name = "CSV")]
    pub halo_files: Option<String>,

    /// Priority for --halo-add-proposal.
    #[arg(long = "priority", value_name = "0..1")]
    pub halo_priority: Option<f64>,

    /// Estimated cost for --halo-add-proposal.
    #[arg(long = "est-cost", value_name = "USD")]
    pub halo_est_cost: Option<f64>,

    // ---- halo operator controls (RFD 0025 M4) ----

    /// Write pause.req — supervisor finishes current cycle then pauses.
    #[arg(long = "halo-pause", action = ArgAction::SetTrue)]
    pub halo_pause: bool,

    /// Clear paused flag + append STREAK_RESET. Run `pi --halo` afterwards.
    #[arg(long = "halo-resume", action = ArgAction::SetTrue)]
    pub halo_resume: bool,

    /// Write stop.req — supervisor finishes current cycle then exits cleanly.
    #[arg(long = "halo-stop", action = ArgAction::SetTrue)]
    pub halo_stop: bool,

    /// Read the runtime system prompt from a file. Mutually exclusive
    /// with --system-prompt (if both, file wins) and overrides the
    /// default system prompt. Useful for orchestrator-driven dispatch
    /// where the per-agent prompt lives in `.pi/agents/<name>.md`.
    #[arg(long = "system-prompt-file", value_name = "PATH")]
    pub system_prompt_file: Option<PathBuf>,
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
            .filter_map(|p| p.strip_prefix('@').map(PathBuf::from))
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
