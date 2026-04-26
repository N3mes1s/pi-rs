use pi_ai::{AuthStorage, ModelRegistry};
use pi_agent_core::{
    discover_context_files, default_system_prompt, ContextFile, RuntimeConfig, SessionManager,
    Settings,
};
use pi_tools::ToolRegistry;
use std::path::PathBuf;

use crate::cli::Cli;
use crate::context::{
    agent_dir, auth_path, package_dir, prompts_dirs, sessions_dir, settings_paths, skills_dirs,
    system_prompt_paths, themes_dirs,
};
use crate::packages;
use crate::prompts::PromptRegistry;
use crate::skills::SkillRegistry;

/// The set of resources assembled at startup, ready to drive any of the modes.
pub struct Startup {
    pub cli: Cli,
    pub settings: Settings,
    pub runtime_config: RuntimeConfig,
    pub prompts: PromptRegistry,
    pub skills: SkillRegistry,
    pub themes: pi_tui::ThemeRegistry,
}

pub fn assemble(cli: Cli) -> anyhow::Result<Startup> {
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

    // 3. auth.
    let auth = AuthStorage::open(auth_path()).unwrap_or_else(|_| AuthStorage::in_memory());
    // overlay env keys (env wins for fresh shells).
    let env = AuthStorage::from_env();
    for p in [
        "anthropic", "openai", "azure", "google", "gemini", "groq", "cerebras", "xai",
        "openrouter", "deepseek", "mistral", "fireworks", "zai", "vercel", "huggingface",
    ] {
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
    let themes = crate::themes::load_themes(&themes_dirs());

    let runtime_config = RuntimeConfig {
        session_manager,
        auth_storage: auth,
        model_registry: registry,
        tools,
        settings: settings.clone(),
        system_prompt: system,
        context_files,
        cwd,
    };

    Ok(Startup {
        cli,
        settings,
        runtime_config,
        prompts,
        skills,
        themes,
    })
}
