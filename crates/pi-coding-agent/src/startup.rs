use pi_agent_core::{
    default_system_prompt, discover_context_files, ContextFile, RuntimeConfig, SessionManager,
    Settings,
};
use pi_ai::{AuthStorage, ModelRegistry};
use pi_tools::ToolRegistry;
use std::path::PathBuf;
use std::sync::Arc;

use crate::cli::Cli;
use crate::context::{
    agent_dir, auth_path, keybindings_path, package_dir, prompts_dirs, sessions_dir,
    settings_paths, skills_dirs, system_prompt_paths, themes_dirs,
};
use crate::extensions::{self, LoadedExtension};
use crate::keymap::Keymap;
use crate::packages;
use crate::prompts::PromptRegistry;
use crate::skills::SkillRegistry;
use crate::slash::SlashRegistry;

/// The set of resources assembled at startup, ready to drive any of the modes.
pub struct Startup {
    pub cli: Cli,
    pub settings: Settings,
    pub runtime_config: RuntimeConfig,
    pub prompts: PromptRegistry,
    pub skills: SkillRegistry,
    pub themes: pi_tui::ThemeRegistry,
    /// Hot-reload handle for themes — kept alive so the watcher fires.
    pub themes_handle: Option<crate::themes::HotThemes>,
    pub keymap: Keymap,
    pub extensions: Vec<LoadedExtension>,
    pub slash_registry: SlashRegistry,
}

pub async fn assemble(cli: Cli) -> anyhow::Result<Startup> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // 1. settings (global + project).
    let (global_settings, project_settings) = settings_paths();
    let mut settings = Settings::load(&global_settings);
    settings.merge_project(&project_settings);

    // 2. CLI overrides.
    if let Some(p) = &cli.provider {
        settings.provider = p.clone();
    }
    if let Some(m) = &cli.model {
        settings.model = m.clone();
    }
    if let Some(t) = &cli.thinking {
        settings.thinking = match t.as_str() {
            "low" => pi_agent_core::settings::ThinkingSetting::Low,
            "medium" => pi_agent_core::settings::ThinkingSetting::Medium,
            "high" => pi_agent_core::settings::ThinkingSetting::High,
            _ => pi_agent_core::settings::ThinkingSetting::Off,
        };
    }
    if let Some(theme) = &cli.theme {
        settings.theme = theme.clone();
    }
    if cli.no_builtin_tools {
        settings.no_builtin_tools = true;
    }
    if cli.no_tools {
        settings.no_tools = true;
    }
    if let Some(t) = &cli.tools {
        settings.tools = t.split(',').map(|s| s.trim().to_string()).collect();
    }
    // CLI / env overrides for model role routing (B1).
    if let Some(s) = &cli.smol {
        settings.roles.smol = Some(s.clone());
    }
    if let Some(s) = &cli.slow {
        settings.roles.slow = Some(s.clone());
    }
    if let Some(s) = &cli.plan {
        settings.roles.plan = Some(s.clone());
    }

    // (autoresearch tools are registered after the base ToolRegistry is built; see below)

    // 3. auth.
    let auth = AuthStorage::open(auth_path()).unwrap_or_else(|_| AuthStorage::in_memory());
    // overlay env keys (env wins for fresh shells).
    let env = AuthStorage::from_env();
    for (p, _) in AuthStorage::ENV_KEYS {
        if let Some(m) = env.get(p) {
            auth.set(p, m);
        }
    }

    // 4. model registry — start with the static catalogue, then merge
    // anything cached by a previous `pi --refresh-models` run.
    let mut registry = ModelRegistry::new(auth.clone());
    let cache = pi_ai::DiscoveredCache::load(&pi_ai::discovered_cache_path(&agent_dir()));
    if !cache.providers.is_empty() {
        registry.merge_discovered(cache.flatten());
    }

    // 5. tools.
    let mut tools = if settings.no_tools {
        ToolRegistry::new()
    } else if settings.no_builtin_tools {
        ToolRegistry::new()
    } else {
        ToolRegistry::with_extras()
    };
    if !settings.tools.is_empty() {
        tools.keep_only(&settings.tools);
    }

    // 5b. RFD 0017 — register the `monitor` tool. The tool emits
    // notifications onto `monitor_rx`; we drain them into the
    // `MonitorPump` so the next assistant turn sees them. (Mode
    // handlers may additionally bridge into their AgentEvent stream
    // via `spawn_event_bridge`, but the pump path is enough to
    // surface the events to the model.)
    let monitor_pump = std::sync::Arc::new(crate::native::monitor::MonitorPump::new());
    let (monitor_tx, monitor_rx) = tokio::sync::mpsc::unbounded_channel();
    if !settings.no_tools
        && (settings.tools.is_empty() || settings.tools.iter().any(|t| t == "monitor"))
    {
        let mcfg = pi_tools::monitor::MonitorConfig {
            max_concurrent: settings.monitor.max_concurrent,
            batch_window: std::time::Duration::from_millis(settings.monitor.batch_window_ms),
            volume_cap_lines: settings.monitor.volume_cap_lines,
            volume_cap_window: std::time::Duration::from_millis(
                settings.monitor.volume_cap_window_ms,
            ),
            default_timeout: std::time::Duration::from_millis(settings.monitor.default_timeout_ms),
            max_timeout: std::time::Duration::from_millis(settings.monitor.max_timeout_ms),
        };
        tools.register(Arc::new(pi_tools::monitor::MonitorTool::new(
            monitor_tx.clone(),
            mcfg,
        )));
        // Drain notifications into the pump (host-mode AgentEvent
        // forwarding is optional and lives in the per-mode glue).
        crate::native::monitor::spawn_event_bridge(
            String::new(),
            monitor_rx,
            None,
            monitor_pump.clone(),
        );
    } else {
        // monitor tool disabled — drop the receiver so the channel
        // closes cleanly.
        drop(monitor_rx);
    }
    drop(monitor_tx);

    // 6. system prompt + addenda.
    let mut system = String::from(default_system_prompt());
    for p in system_prompt_paths() {
        if let Ok(s) = std::fs::read_to_string(&p) {
            system.push_str("\n\n");
            system.push_str(&s);
        }
    }
    // (skill manifest gets appended after skills are loaded — see step 10)

    // 7. context files (AGENTS.md / CLAUDE.md).
    let context_files: Vec<ContextFile> = if cli.no_context_files {
        Vec::new()
    } else if let Some(override_path) = &cli.agents_md {
        // Evolution-daemon path: benchmark candidate AGENTS.md without
        // touching the live one. Skip discovery, use only the override.
        match std::fs::read_to_string(override_path) {
            Ok(content) => vec![ContextFile {
                path: override_path.clone(),
                content,
            }],
            Err(_) => Vec::new(),
        }
    } else {
        discover_context_files(&cwd, &agent_dir(), &["AGENTS.md", "CLAUDE.md"])
    };

    // 8. session manager.
    let session_dir = cli
        .session_dir
        .clone()
        .or(settings.session_dir.clone())
        .unwrap_or_else(sessions_dir);
    let session_manager = if cli.no_session {
        SessionManager::in_memory()
    } else {
        SessionManager::on_disk(session_dir, cwd.clone())
            .unwrap_or_else(|_| SessionManager::in_memory())
    };

    // 9. prompts (global + project + packages).
    let mut prompts = PromptRegistry::new();
    prompts.load_all(&prompts_dirs());

    // 10. skills.
    let mut skills = SkillRegistry::new();
    skills.load_all(&skills_dirs());
    // Built-in skills shipped with the binary (autoresearch-create, etc.).
    // We embed them as bytes at compile time and materialise into a temp
    // directory at runtime so the existing on-disk skill loader works
    // unchanged.
    if let Ok(builtin_dir) = crate::skills::ensure_builtin_skills_dir() {
        skills.load_dir(&builtin_dir);
    }

    // 10b. Append the available-skills block to the system prompt so the
    // model knows which skills exist and can read their SKILL.md when a
    // task matches the description (faithful port of upstream's
    // formatSkillsForPrompt).
    {
        let names = skills.names();
        let resolved: Vec<&crate::skills::Skill> =
            names.iter().filter_map(|n| skills.get(n)).collect();
        system.push_str(&crate::skills::format_skills_for_prompt(&resolved));
    }

    // 11. packages.
    let pkgs = packages::discover(&package_dir());
    for pkg in &pkgs {
        let dirs = packages::package_dirs(pkg);
        prompts.load_all(&dirs.prompts);
        skills.load_all(&dirs.skills);
    }

    // 12. extra skills/prompts from CLI.
    for sk in &cli.skills {
        skills.load_dir(sk.parent().unwrap_or(sk));
    }

    // 13. themes.
    let theme_dirs = themes_dirs();
    let themes = crate::themes::load_themes(&theme_dirs);
    let themes_handle = Some(crate::themes::HotThemes::new(theme_dirs));

    // 14. keybindings (defaults + JSON overrides).
    let mut keymap = Keymap::defaults();
    if !cli.no_extensions {
        if let Ok(overrides) = Keymap::load_overrides(&keybindings_path()) {
            keymap.merge_overrides(&overrides);
        }
    }

    // 15. extensions: project + user + CLI + package-provided.
    let mut ext_roots: Vec<PathBuf> = if cli.no_extensions {
        Vec::new()
    } else {
        vec![
            agent_dir().join("extensions"),
            PathBuf::from(".pi").join("extensions"),
        ]
    };
    if !cli.no_extensions {
        for pkg in &pkgs {
            let dirs = packages::package_dirs(pkg);
            ext_roots.extend(dirs.extensions);
        }
        for e in &cli.extensions {
            ext_roots.push(e.clone());
        }
    }
    // Register autoresearch tools (init_experiment, run_experiment, log_experiment).
    if !settings.no_tools {
        use std::sync::Arc;
        tools.register(Arc::new(crate::autoresearch::tools::InitExperimentTool));
        tools.register(Arc::new(crate::autoresearch::tools::RunExperimentTool));
        tools.register(Arc::new(crate::autoresearch::tools::LogExperimentTool));
        // Native todo tool (B2). Persists to <cwd>/.pi/todo.json.
        tools.register(Arc::new(crate::native::todo::TodoTool));
        // Native ask tool (B3). Only register in interactive mode — in
        // print/json/rpc the tool can't pop a picker and would always
        // return `is_error: true` ("ASK requires interactive mode"),
        // wasting the agent's tokens on an unrecoverable call. Mirrors
        // how `approve` / `judge` are wired by mode (validate bug #4).
        if cli.effective_mode() == crate::cli::Mode::Interactive {
            tools.register(Arc::new(crate::native::ask::AskTool));
        }
        // RFD 0005: subagents + `task` tool. Always registered when
        // tools are enabled — discovery (project / user / bundled)
        // happens lazily on the first invocation. The host must wrap
        // its `session.prompt(...)` call in
        // `crate::native::task::tool::with_runtime(handle, …)` for
        // the tool to find a parent handle; otherwise the call returns
        // a clean `is_error: true` result.
        tools.register(Arc::new(crate::native::task::TaskTool::new()));
        // Native lsp tool (D1 + H5). Wired through `Settings::lsp` so
        // users opt in via the `lsp` block in settings.json. Defaults
        // keep the master switch off; the tool stays registered so the
        // agent can call the `status` op and see "no servers running"
        // when LSP is disabled.
        let lsp_cfg = crate::native::lsp::LspConfig::from(&settings.lsp);
        tools.register(Arc::new(crate::native::lsp::LspTool::new(lsp_cfg.clone())));
        // RFD 0001: when LSP is enabled, swap the bare `write` tool
        // for the wrapper that fires `format_on_write` and
        // `diagnostics_on_write` after every successful write. The
        // wrapper registers under the same name so the registry's
        // BTreeMap insert overrides the entry left by `with_extras()`.
        if lsp_cfg.enabled {
            tools.register(Arc::new(crate::native::lsp::LspWriteTool::new(lsp_cfg)));
        }
    }

    let loaded_exts = extensions::discover(&ext_roots);
    if !loaded_exts.is_empty() {
        // Strip any builtins that extensions declare they replace, *before*
        // registering the extension tools so there are no duplicates.
        extensions::apply_replacements(&mut tools, &loaded_exts);
        for t in extensions::extension_tools(&loaded_exts) {
            tools.register(t);
        }
        // Fire-and-forget startup hooks (errors are only warned, never fatal).
        extensions::run_startup_hooks(&loaded_exts).await;
    }

    // Register extension keybindings.
    for (ext_idx, ext) in loaded_exts.iter().enumerate() {
        for kb in &ext.manifest.keybindings {
            keymap.bind_extension(&kb.chord, ext_idx, kb.command.clone());
        }
    }

    // Build slash registry with extension commands registered.
    let mut slash_registry = SlashRegistry::new();
    slash_registry.register_templates(&prompts);
    {
        let ext_cmds: Vec<(usize, &crate::extensions::ExtensionCommandManifest)> = loaded_exts
            .iter()
            .enumerate()
            .flat_map(|(i, ext)| ext.manifest.commands.iter().map(move |c| (i, c)))
            .collect();
        slash_registry.register_extension_commands(&ext_cmds);
    }

    // Auto-approval: load policy from disk (or fall back to safe defaults)
    // and decide whether to instantiate a judge. The mode comes from
    // settings; CLI override comes through later.
    let auto_policy_path = agent_dir().join("auto-approve.json");
    let mut auto_policy = if auto_policy_path.exists() {
        crate::auto_approve::Policy::load(&auto_policy_path)
            .unwrap_or_else(|_| crate::auto_approve::Policy::default_safe())
    } else {
        crate::auto_approve::Policy::default_safe()
    };
    auto_policy.resolve_inheritance();
    // Default mode per surface:
    // - interactive  → Ask (every call confirmed in UI)
    // - print/json/rpc → AutoPolicy (no UI to ask in, so default to
    //   policy-only — denies dangerous calls but doesn't block on
    //   benign ones the policy allows). Previously defaulted to Ask
    //   here too which made non-interactive bash unusable without
    //   `--auto-approve yolo`.
    let auto_mode = cli
        .auto_approve
        .as_deref()
        .and_then(crate::auto_approve::Mode::parse)
        .unwrap_or_else(|| match cli.effective_mode() {
            crate::cli::Mode::Interactive => crate::auto_approve::Mode::Ask,
            _ => crate::auto_approve::Mode::AutoPolicy,
        });
    let judge = if matches!(auto_mode, crate::auto_approve::Mode::AutoJudge) {
        let mut jc = crate::auto_approve::JudgeConfig::default();
        // Default the judge to settings.roles.smol when present (B1):
        // the smol role is *the* place to put a cheap structured-output
        // model, so the judge picks it up automatically.
        if let Some(smol) = &settings.roles.smol {
            if let Some((p, m)) = smol.split_once('/') {
                jc.provider = p.to_string();
                jc.model = m.to_string();
            } else {
                jc.model = smol.clone();
            }
        }
        // CLI flag still wins if explicitly given.
        if let Some(m) = &cli.auto_approve_model {
            jc.model = m.clone();
        }
        crate::auto_approve::Judge::build(&registry, &auth, jc).ok()
    } else {
        None
    };
    let auto_gate = std::sync::Arc::new(crate::auto_approve::AutoApproveGate::new(
        auto_mode,
        auto_policy,
        judge,
    ));
    let gate_ask_is_approve = matches!(cli.effective_mode(), crate::cli::Mode::Interactive);

    // Load TTSR rules from `~/.pi/agent/ttsr/` (best-effort: missing
    // dir, malformed frontmatter, or invalid regex are silently
    // skipped). Wrapped in an `Arc<RuleSet>` so the interceptor and
    // any debug surface can share it.
    let stream_interceptor: Option<std::sync::Arc<dyn pi_agent_core::StreamInterceptor>> = {
        let dir = crate::native::ttsr::default_dir();
        let rs = match dir {
            Some(d) if d.is_dir() => crate::native::ttsr::RuleSet::load_dir(&d),
            _ => crate::native::ttsr::RuleSet::new(),
        };
        let ttsr: Option<std::sync::Arc<dyn pi_agent_core::StreamInterceptor>> = if rs.is_empty() {
            None
        } else {
            let arc_rs = std::sync::Arc::new(rs);
            Some(
                std::sync::Arc::new(crate::native::ttsr::TtsrInterceptor::new(arc_rs))
                    as std::sync::Arc<dyn pi_agent_core::StreamInterceptor>,
            )
        };
        // Chain TTSR + monitor pump. If both are present, the chained
        // interceptor calls them in order on each delta and returns
        // the first non-Continue action.
        match ttsr {
            None => {
                Some(monitor_pump.clone() as std::sync::Arc<dyn pi_agent_core::StreamInterceptor>)
            }
            Some(t) => Some(std::sync::Arc::new(ChainedInterceptor {
                a: t,
                b: monitor_pump.clone(),
            })
                as std::sync::Arc<dyn pi_agent_core::StreamInterceptor>),
        }
    };

    let runtime_config = RuntimeConfig {
        session_manager,
        auth_storage: auth,
        model_registry: registry,
        tools,
        settings: settings.clone(),
        system_prompt: system,
        context_files,
        cwd,
        provider_factory: None,
        tool_gate: Some(auto_gate as std::sync::Arc<dyn pi_agent_core::ToolGate>),
        gate_ask_is_approve,
        stream_interceptor,
    };

    Ok(Startup {
        cli,
        settings,
        runtime_config,
        prompts,
        skills,
        themes,
        themes_handle,
        keymap,
        extensions: loaded_exts,
        slash_registry,
    })
}

/// Compose two [`StreamInterceptor`]s in order. RFD 0017 wires this so
/// TTSR rules and the monitor pump can both fire on the same stream.
struct ChainedInterceptor {
    a: std::sync::Arc<dyn pi_agent_core::StreamInterceptor>,
    b: std::sync::Arc<dyn pi_agent_core::StreamInterceptor>,
}

#[async_trait::async_trait]
impl pi_agent_core::StreamInterceptor for ChainedInterceptor {
    async fn turn_start(&self) {
        self.a.turn_start().await;
        self.b.turn_start().await;
    }
    async fn on_text_delta(&self, text: &str) -> pi_agent_core::InterceptAction {
        if let pi_agent_core::InterceptAction::AbortAndInject(s) = self.a.on_text_delta(text).await
        {
            return pi_agent_core::InterceptAction::AbortAndInject(s);
        }
        self.b.on_text_delta(text).await
    }
}
