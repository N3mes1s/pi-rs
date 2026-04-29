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
    // Clone the sender so we can also hand it to `open_session` if a
    // resume path is taken below. The original `tx` goes to the
    // `create_agent_session` so the channel lives even if no resume
    // happens.
    let resume_tx = tx.clone();
    let (_runtime, mut session) = create_agent_session(cfg, Some(tx))?;

    if startup.cli.continue_recent
        || startup.cli.resume.is_some()
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
        } else if let Some(r) = &startup.cli.resume {
            // `-r` / `--resume` accepts an optional id. Empty string =
            // user typed bare `-r`, so fall back to most-recent. Any
            // non-empty value names the session id (or path) directly
            // and gets passed verbatim to `open_session`, which
            // accepts both forms.
            if r.trim().is_empty() {
                mgr.most_recent().map(|m| m.id)
            } else {
                Some(r.clone())
            }
        } else {
            None
        };
        if let Some(id) = target {
            let runtime = pi_agent_core::AgentSessionRuntime::new(startup.runtime_config.clone());
            // Hand the LIVE sender (cloned above) to the resumed
            // session so its events flow back into the same `rx` the
            // caller will consume. The previous code used a fresh
            // dead channel here, so events from the resumed session
            // went nowhere — that's why `pi -r <id> -p` exited
            // silently with no output.
            match runtime.open_session(&id, Some(resume_tx.clone())) {
                Ok(opened) => session = opened,
                Err(e) => {
                    // Surface the failure instead of silently continuing
                    // with a fresh session — that swallowed-error path is
                    // why "pi -r <bad id>" used to succeed-with-no-output
                    // and look identical to "no resume happened".
                    return Err(anyhow::anyhow!(
                        "failed to resume session '{id}': {e}"
                    ));
                }
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
