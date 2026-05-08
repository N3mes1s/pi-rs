use serde::{Deserialize, Serialize};
use std::io::Write;
use tokio::io::AsyncBufReadExt;

use crate::modes::build_session;
use crate::startup::Startup;

/// Bidirectional RPC mode. Reads JSONL commands from stdin, writes JSONL
/// events to stdout. One message per line.
pub async fn run(startup: Startup) -> anyhow::Result<()> {
    // Clone the sandbox_provider Arc early so we can call cleanup() at exit
    // even after `startup` is partially consumed. (RFD 0026 §"Session lifecycle")
    let sandbox_provider = startup.runtime_config.sandbox_provider.clone();

    let (session, mut rx) = build_session(&startup)?;

    let stdout = std::io::stdout();
    let stdout = std::sync::Arc::new(std::sync::Mutex::new(stdout));
    let writer = stdout.clone();
    let printer = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            if let Ok(line) = serde_json::to_string(&ev) {
                if let Ok(mut g) = writer.lock() {
                    let _ = writeln!(g, "{}", line);
                    let _ = g.flush();
                }
            }
        }
    });

    let stdin = tokio::io::stdin();
    let mut reader = tokio::io::BufReader::new(stdin).lines();
    while let Some(line) = reader.next_line().await? {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        let cmd: RpcCommand = match serde_json::from_str(&line) {
            Ok(c) => c,
            Err(e) => {
                let resp = RpcEvent::Error {
                    message: format!("bad command: {}", e),
                };
                if let Ok(mut g) = stdout.lock() {
                    let _ = writeln!(g, "{}", serde_json::to_string(&resp).unwrap_or_default());
                    let _ = g.flush();
                }
                continue;
            }
        };
        match cmd {
            RpcCommand::Prompt { text } => {
                let session = session.clone();
                tokio::spawn(async move {
                    let _ = session.prompt(text).await;
                });
            }
            RpcCommand::Abort => {
                session.abort().await;
            }
            RpcCommand::SetModel { provider, model } => {
                session.set_model(provider, model).await;
            }
            RpcCommand::Compact { instructions } => {
                session.compact(instructions).await;
            }
            RpcCommand::Quit => {
                break;
            }
        }
    }
    printer.abort();

    // Abort any in-flight prompt task before cleaning up the sandbox.
    // Per RFD 0026 §"Concurrent prompt draining before cleanup": abort sets
    // the aborted flag at the next loop boundary; cleanup races the last
    // in-flight tool call (acceptable; E2B timeout backstop handles the rest).
    session.abort().await;

    let _ = crate::native::trajectory::finalize_for_runtime(
        &startup.runtime_config,
        &startup.settings,
        session.id(),
    )
    .await;

    // Cleanup remote sandbox (e.g. E2B) at mode exit. Best-effort: errors are
    // logged as warnings and do not fail the mode. (RFD 0026 §"Session lifecycle")
    if let Some(sp) = sandbox_provider {
        if let Err(e) = sp.cleanup().await {
            tracing::warn!(err = %e, "sandbox cleanup failed at rpc-mode exit");
        }
    }

    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RpcCommand {
    Prompt { text: String },
    Abort,
    SetModel { provider: String, model: String },
    Compact { instructions: Option<String> },
    Quit,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RpcEvent {
    Error { message: String },
}
