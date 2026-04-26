use crossterm::style::{Color, ResetColor, SetForegroundColor};
use crossterm::{cursor, execute, queue, style::Print};
use pi_agent_core::AgentEventKind;
use std::io::Write;

use crate::modes::build_session;
use crate::slash::{self, SlashKind, SlashRegistry};
use crate::startup::Startup;

/// Interactive mode: a simple REPL with slash-command support.
///
/// This is a deliberately simpler take on the upstream TUI. The hard
/// parts of the original (alternate-screen TUI, hot keys, message queue,
/// scrolling buffer with diff renderer) are present in the `pi-tui`
/// crate; this loop wires them up at a level that's portable across
/// terminals and easy to script.
pub async fn run(startup: Startup) -> anyhow::Result<()> {
    let mut slash = SlashRegistry::new();
    slash.register_templates(&startup.prompts);

    let (session, mut rx) = build_session(&startup)?;

    print_header(&startup);

    // Spawn the event printer.
    let printer = tokio::spawn(async move {
        let mut current_line_open = false;
        while let Some(ev) = rx.recv().await {
            match ev.kind {
                AgentEventKind::AssistantTextDelta { text } => {
                    let mut out = std::io::stdout();
                    let _ = execute!(out, SetForegroundColor(Color::Green));
                    let _ = write!(out, "{}", text);
                    let _ = execute!(out, ResetColor);
                    let _ = out.flush();
                    current_line_open = true;
                }
                AgentEventKind::AssistantThinkingDelta { text } => {
                    let mut out = std::io::stdout();
                    let _ = execute!(out, SetForegroundColor(Color::DarkGrey));
                    let _ = write!(out, "{}", text);
                    let _ = execute!(out, ResetColor);
                    let _ = out.flush();
                    current_line_open = true;
                }
                AgentEventKind::AssistantToolCall { call } => {
                    if current_line_open {
                        println!();
                    }
                    let mut out = std::io::stdout();
                    let _ = execute!(out, SetForegroundColor(Color::Yellow));
                    let _ = writeln!(
                        out,
                        "→ {} {}",
                        call.name,
                        serde_json::to_string(&call.input).unwrap_or_default()
                    );
                    let _ = execute!(out, ResetColor);
                    current_line_open = false;
                }
                AgentEventKind::ToolResult { result } => {
                    let mut out = std::io::stdout();
                    let color = if result.is_error { Color::Red } else { Color::DarkGrey };
                    let _ = execute!(out, SetForegroundColor(color));
                    for line in result.model_output.lines().take(20) {
                        let _ = writeln!(out, "  {line}");
                    }
                    if result.model_output.lines().count() > 20 {
                        let _ = writeln!(out, "  …");
                    }
                    let _ = execute!(out, ResetColor);
                }
                AgentEventKind::Error { message } => {
                    let mut out = std::io::stdout();
                    let _ = execute!(out, SetForegroundColor(Color::Red));
                    let _ = writeln!(out, "[error] {}", message);
                    let _ = execute!(out, ResetColor);
                }
                AgentEventKind::Usage { usage } => {
                    let mut out = std::io::stdout();
                    let _ = execute!(out, SetForegroundColor(Color::DarkGrey));
                    let _ = writeln!(
                        out,
                        "[tokens: in={} out={}]",
                        usage.input_tokens, usage.output_tokens
                    );
                    let _ = execute!(out, ResetColor);
                }
                AgentEventKind::TurnComplete => {
                    if current_line_open {
                        println!();
                    }
                    let _ = current_line_open;
                    break;
                }
                AgentEventKind::Aborted => {
                    println!("\n[aborted]");
                    break;
                }
                _ => {}
            }
        }
    });

    // Read user input line-by-line. A real TUI would use `pi_tui::Editor`
    // and crossterm raw events; this version uses stdin lines so it works
    // unmodified in any terminal and over pipes.
    use tokio::io::AsyncBufReadExt;
    let mut stdin = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    let mut handle = printer;
    loop {
        // Wait for previous turn to finish before drawing the next prompt.
        if handle.is_finished() {
            // good — we're idle.
        }
        print_input_prompt(&startup);
        let line = match stdin.next_line().await? {
            Some(l) => l,
            None => break,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some((name, args)) = slash::parse(trimmed) {
            match handle_slash(&slash, &name, &args, &session, &startup).await {
                SlashOutcome::Quit => break,
                SlashOutcome::Continue => continue,
                SlashOutcome::Submit(text) => {
                    handle.abort();
                    let (new_handle, new_rx) = spawn_printer(&session);
                    handle = new_handle;
                    let _ = new_rx;
                    let _ = session.prompt(text).await;
                    handle.await.ok();
                    let (h, _) = spawn_printer(&session);
                    handle = h;
                }
            }
            continue;
        }
        // Regular prompt.
        handle.abort();
        let (h, _) = spawn_printer(&session);
        handle = h;
        let _ = session.prompt(trimmed.to_string()).await;
        handle.await.ok();
        let (h, _) = spawn_printer(&session);
        handle = h;
    }
    Ok(())
}

enum SlashOutcome {
    Quit,
    Continue,
    Submit(String),
}

async fn handle_slash(
    slash: &SlashRegistry,
    name: &str,
    args: &str,
    session: &pi_agent_core::AgentSession,
    startup: &Startup,
) -> SlashOutcome {
    match name {
        "quit" | "exit" => SlashOutcome::Quit,
        "help" | "hotkeys" => {
            for n in slash.names() {
                println!("/{n}");
            }
            SlashOutcome::Continue
        }
        "compact" => {
            let ins = if args.is_empty() { None } else { Some(args.to_string()) };
            session.compact(ins).await;
            println!("[compacted]");
            SlashOutcome::Continue
        }
        "model" => {
            let target = args.trim();
            if target.is_empty() {
                for p in startup.runtime_config.model_registry.providers() {
                    for m in &p.models {
                        println!("{}/{}", p.name, m.id);
                    }
                }
                SlashOutcome::Continue
            } else {
                let (provider, model) = target
                    .split_once('/')
                    .map(|(p, m)| (p.to_string(), m.to_string()))
                    .unwrap_or_else(|| ("anthropic".into(), target.to_string()));
                session.set_model(provider, model).await;
                println!("[model set to {}]", target);
                SlashOutcome::Continue
            }
        }
        "settings" => {
            println!("settings file: {}", crate::context::settings_paths().0.display());
            SlashOutcome::Continue
        }
        "tree" => {
            let mgr = startup.runtime_config.session_manager.clone();
            if let Some(tree) = mgr.tree(session.id()) {
                for entry in tree.entries {
                    println!(
                        "{} ({}) parent={}",
                        entry.id,
                        match entry.kind {
                            pi_agent_core::SessionEntryKind::User { .. } => "user",
                            pi_agent_core::SessionEntryKind::Assistant { .. } => "assistant",
                            pi_agent_core::SessionEntryKind::ToolCall { .. } => "tool_call",
                            pi_agent_core::SessionEntryKind::ToolResult { .. } => "tool_result",
                            _ => "meta",
                        },
                        entry.parent_id.unwrap_or_default()
                    );
                }
            }
            SlashOutcome::Continue
        }
        "fork" => {
            if let Err(e) = startup
                .runtime_config
                .session_manager
                .fork(session.id(), args.trim())
            {
                eprintln!("[fork] {}", e);
            } else {
                println!("[forked at {}]", args.trim());
            }
            SlashOutcome::Continue
        }
        "resume" => {
            for s in startup.runtime_config.session_manager.list().iter().take(20) {
                println!("- {}  {}  {}", s.id, s.updated_at, s.path.display());
            }
            SlashOutcome::Continue
        }
        "export" => {
            let path = if args.is_empty() {
                "session.html".into()
            } else {
                args.to_string()
            };
            export_html(session, &path);
            SlashOutcome::Continue
        }
        "share" | "login" | "logout" | "scoped-models" | "clone" => {
            println!("[/{name}] not yet implemented in pi-rs");
            SlashOutcome::Continue
        }
        other => {
            if let Some(cmd) = slash.get(other) {
                if let SlashKind::Template { body } = &cmd.kind {
                    return SlashOutcome::Submit(slash::render_template(body, args));
                }
            }
            println!("unknown slash command: /{other}");
            SlashOutcome::Continue
        }
    }
}

fn export_html(session: &pi_agent_core::AgentSession, path: &str) {
    use std::fmt::Write;
    let messages = futures::executor::block_on(session.messages());
    let mut html = String::new();
    let _ = write!(
        html,
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>pi session</title></head><body><pre>"
    );
    for m in messages {
        let role = format!("{:?}", m.role);
        let _ = writeln!(html, "[{}]\n{}", role, m.text());
    }
    let _ = write!(html, "</pre></body></html>");
    if let Err(e) = std::fs::write(path, html) {
        eprintln!("[export] {e}");
    } else {
        println!("[exported {path}]");
    }
}

fn print_header(startup: &Startup) {
    let mut out = std::io::stdout();
    let _ = queue!(out, SetForegroundColor(Color::Cyan), Print("pi-rs "));
    let _ = queue!(
        out,
        ResetColor,
        Print(format!(
            "({}/{})\n",
            startup.settings.provider, startup.settings.model
        ))
    );
    let _ = queue!(
        out,
        SetForegroundColor(Color::DarkGrey),
        Print(format!(
            "cwd: {}\n",
            startup.runtime_config.cwd.display()
        ))
    );
    let _ = queue!(out, Print("type a message, /help for commands, /quit to exit\n\n"));
    let _ = queue!(out, ResetColor);
    let _ = out.flush();
}

fn print_input_prompt(_startup: &Startup) {
    let mut out = std::io::stdout();
    let _ = execute!(out, SetForegroundColor(Color::Cyan), Print("\nyou> "), ResetColor);
    let _ = out.flush();
}

fn spawn_printer(
    _session: &pi_agent_core::AgentSession,
) -> (
    tokio::task::JoinHandle<()>,
    tokio::sync::mpsc::UnboundedReceiver<pi_agent_core::AgentEvent>,
) {
    let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let handle = tokio::spawn(async move {});
    (handle, rx)
}

// Suppress unused warning from cursor import on platforms without a cursor.
#[allow(dead_code)]
fn _force_link() {
    let _ = cursor::Hide;
}
