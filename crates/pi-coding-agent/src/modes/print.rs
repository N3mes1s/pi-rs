use pi_agent_core::AgentEventKind;

use crate::modes::{build_session, read_stdin_if_piped};
use crate::prompts;
use crate::startup::Startup;

/// Print mode: run a single prompt, stream tokens to stdout, exit when done.
pub async fn run(startup: Startup) -> anyhow::Result<()> {
    let (session, mut rx) = build_session(&startup)?;

    // If a prompt template was specified, resolve it and use it as the sole
    // prompt, ignoring positional args + stdin.
    let prompt = if let Some(spec) = &startup.cli.prompt_template {
        let joined = startup.cli.prompt_text().unwrap_or_default();
        match prompts::resolve(spec, &startup.prompts, &joined) {
            Ok(resolved) => resolved,
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(2);
            }
        }
    } else {
        let stdin_text = read_stdin_if_piped();
        let prompt_text = match (startup.cli.prompt_text(), stdin_text) {
            (Some(p), Some(stdin)) => format!("{p}\n\n{stdin}"),
            (Some(p), None) => p,
            (None, Some(stdin)) => stdin,
            (None, None) => {
                eprintln!("error: no prompt provided");
                std::process::exit(2);
            }
        };
        expand_at_files(&startup.cli.at_files(), &prompt_text)
    };


    let printer = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            match ev.kind {
                AgentEventKind::AssistantTextDelta { text } => {
                    print!("{}", text);
                    use std::io::Write;
                    let _ = std::io::stdout().flush();
                }
                AgentEventKind::ToolResult { result } => {
                    if result.is_error {
                        eprintln!("\n[tool error] {}", result.model_output);
                    }
                }
                AgentEventKind::AssistantToolCall { call } => {
                    eprintln!("\n[tool {}] {}", call.name, serde_json::to_string(&call.input).unwrap_or_default());
                }
                AgentEventKind::Error { message } => {
                    eprintln!("\n[error] {message}");
                }
                AgentEventKind::TurnComplete => {
                    println!();
                    break;
                }
                _ => {}
            }
        }
    });

    let prompt = crate::modes::expand_slash(&prompt, &startup);
    let _ = session.prompt(prompt).await;
    printer.await.ok();
    Ok(())
}

fn expand_at_files(files: &[std::path::PathBuf], prompt: &str) -> String {
    if files.is_empty() {
        return prompt.into();
    }
    let mut out = String::from(prompt);
    out.push_str("\n\n");
    for f in files {
        match std::fs::read_to_string(f) {
            Ok(content) => {
                out.push_str(&format!(
                    "<file path=\"{}\">\n{}\n</file>\n\n",
                    f.display(),
                    content
                ));
            }
            Err(e) => out.push_str(&format!("[failed to read {}: {}]\n", f.display(), e)),
        }
    }
    out
}
