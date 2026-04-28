//! The four operating modes: interactive, print, json, rpc.

pub mod interactive;
pub mod json;
pub mod print;
pub mod rpc;

use crate::startup::Startup;
use pi_agent_core::{create_agent_session, AgentSession, EventSender};
use tokio::sync::mpsc::UnboundedReceiver;

pub(crate) fn build_session(
    startup: &Startup,
) -> anyhow::Result<(AgentSession, UnboundedReceiver<pi_agent_core::AgentEvent>)> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<pi_agent_core::AgentEvent>();
    let cfg = startup.runtime_config.clone();
    let (_runtime, mut session) = create_agent_session(cfg, Some(tx))?;

    if startup.cli.continue_recent
        || startup.cli.resume
        || startup.cli.session.is_some()
        || startup.cli.fork.is_some()
    {
        let mgr = startup.runtime_config.session_manager.clone();
        let target: Option<String> = if let Some(s) = &startup.cli.session {
            Some(s.clone())
        } else if let Some(f) = &startup.cli.fork {
            Some(f.clone())
        } else if startup.cli.continue_recent {
            mgr.most_recent().map(|m| m.id)
        } else if startup.cli.resume {
            // crude: pick most recent. In interactive mode the picker
            // overrides this.
            mgr.most_recent().map(|m| m.id)
        } else {
            None
        };
        if let Some(id) = target {
            let runtime = pi_agent_core::AgentSessionRuntime::new(startup.runtime_config.clone());
            if let Ok(opened) = runtime.open_session(&id, Some(_sender_clone(&session))) {
                session = opened;
            }
        }
    }
    Ok((session, rx))
}

fn _sender_clone(_s: &AgentSession) -> EventSender {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    tx
}

pub(crate) fn read_stdin_if_piped() -> Option<String> {
    use std::io::Read;
    if atty_stdin() {
        return None;
    }
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_ok() && !buf.trim().is_empty() {
        Some(buf)
    } else {
        None
    }
}

fn atty_stdin() -> bool {
    // We avoid the `atty` crate. If reading would block on a tty, we treat
    // it as interactive. The simplest portable check: if stdin is a tty, we
    // skip stdin slurping. Use IsTerminal from std (1.70+).
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
}

/// Expand a leading slash command in the user prompt for non-TUI modes.
///
/// `/autoresearch <goal>` becomes a plain `autoresearch: <goal>` user
/// message; the agent picks up the autoresearch-create skill from the
/// `<available_skills>` block in the system prompt (injected at startup)
/// and reads `SKILL.md` itself.
///
/// `/skill:<name> [args]` injects the named skill's SKILL.md body
/// (looked up in `startup.skills`) followed by the trailing args.
/// Unknown skills pass through unchanged so the agent at least sees the
/// raw text.
///
/// Other slash commands pass through unchanged.
pub fn expand_slash(prompt: &str, startup: &Startup) -> String {
    expand_slash_with(prompt, &startup.skills)
}

/// Lower-level variant of [`expand_slash`] taking just a [`SkillRegistry`].
/// Exposed for unit testing without having to build a full [`Startup`].
pub fn expand_slash_with(prompt: &str, skills: &crate::skills::SkillRegistry) -> String {
    let trimmed = prompt.trim_start();
    let Some((name, args)) = crate::slash::parse(trimmed) else {
        return prompt.to_string();
    };
    if let Some(skill_name) = name.strip_prefix("skill:") {
        if let Some(skill) = skills.get(skill_name) {
            let arg = args.trim();
            let mut msg = String::new();
            msg.push_str(&format!("# Skill: {}\n\n", skill.name));
            msg.push_str(&skill.body);
            if !arg.is_empty() {
                msg.push_str("\n\n---\n\n");
                msg.push_str(arg);
            }
            return msg;
        }
        return prompt.to_string();
    }
    if name != "autoresearch" {
        return prompt.to_string();
    }
    let goal = args.trim();
    if goal.is_empty() || matches!(goal, "off" | "clear" | "export") {
        return prompt.to_string();
    }
    format!("autoresearch: {goal}")
}
