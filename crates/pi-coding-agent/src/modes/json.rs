use crate::modes::{build_session, read_stdin_if_piped};
use crate::prompts;
use crate::startup::Startup;

/// JSON event stream mode: emit one event per line, JSON encoded.
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
                return Ok(());
            }
        }
    } else {
        let stdin_text = read_stdin_if_piped();
        match (startup.cli.prompt_text(), stdin_text) {
            (Some(p), Some(s)) => format!("{p}\n\n{s}"),
            (Some(p), None) => p,
            (None, Some(s)) => s,
            (None, None) => return Ok(()),
        }
    };


    let printer = tokio::spawn(async move {
        use std::io::Write;
        while let Some(ev) = rx.recv().await {
            if let Ok(line) = serde_json::to_string(&ev) {
                let stdout = std::io::stdout();
                let mut out = stdout.lock();
                let _ = writeln!(out, "{line}");
                let _ = out.flush();
            }
            if matches!(
                ev.kind,
                pi_agent_core::AgentEventKind::TurnComplete | pi_agent_core::AgentEventKind::Aborted
            ) {
                break;
            }
        }
    });

    let prompt = crate::modes::expand_slash(&prompt, &startup);
    let handle = crate::native::task::tool::ParentHandle {
        parent_cfg: std::sync::Arc::new(startup.runtime_config.clone()),
        parent_session: session.clone(),
        current_agent: None,
    };
    let _ = crate::native::task::tool::with_runtime(handle, session.prompt(prompt)).await;
    printer.await.ok();

    let _ = crate::native::trajectory::finalize_for_runtime(
        &startup.runtime_config,
        &startup.settings,
        session.id(),
    )
    .await;

    Ok(())
}
