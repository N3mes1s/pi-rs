use pi_ai::{AuthStorage, ModelRegistry};
use pi_agent_core::{
    discover_context_files, default_system_prompt, ContextFile, RuntimeConfig, SessionManager,
    Settings,
};
use pi_tools::ToolRegistry;
use std::path::PathBuf;

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

    // 4. model registry.
    let registry = ModelRegistry::new(auth.clone());

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

    // 6. system prompt + addenda.
    let mut system = String::from(default_system_prompt());
    for p in system_prompt_paths() {
        if let Ok(s) = std::fs::read_to_string(&p) {
            system.push_str("\n\n");
            system.push_str(&s);
        }
    }

    // 7. context files (AGENTS.md / CLAUDE.md).
    let context_files: Vec<ContextFile> = if cli.no_context_files {
        Vec::new()
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
        SessionManager::on_disk(session_dir, cwd.clone()).unwrap_or_else(|_| SessionManager::in_memory())
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
