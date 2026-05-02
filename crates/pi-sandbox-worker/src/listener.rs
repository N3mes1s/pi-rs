//! Vsock listener — accepts host connections, runs the
//! request/response loop per connection, handles SIGTERM /
//! SIGINT for graceful shutdown.
//!
//! Each connection is spawned as an independent tokio task so that
//! the signal-polling `select!` in `serve()` remains live for the
//! entire lifetime of the listener — including while one or more
//! host connections are active.  A [`tokio::sync::watch`] channel
//! carries the shutdown signal from `serve()` into every spawned
//! connection task, allowing in-flight work to drain promptly.

use crate::dispatch::dispatch_request;
use anyhow::Context;
use pi_sandbox_protocol::framing;
use std::path::PathBuf;
use std::time::Instant;
use tokio::io::BufReader;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::watch;
use tokio_vsock::{VsockAddr, VsockListener, VMADDR_CID_ANY};
use tracing::{error, info, warn};

/// Start the vsock listener loop. Runs until SIGTERM/SIGINT or accept error.
///
/// Connections are spawned as separate tasks so that signal polling is never
/// blocked by an active connection.
pub async fn serve(vsock_port: u32, work_dir: PathBuf) -> anyhow::Result<()> {
    // VMADDR_CID_ANY is the conventional "listen for any host CID".
    let addr = VsockAddr::new(VMADDR_CID_ANY, vsock_port);
    let listener = VsockListener::bind(addr)
        .with_context(|| format!("failed to bind vsock listener on port {vsock_port}"))?;
    info!(port = vsock_port, work_dir = %work_dir.display(),
          "pi-sandbox-worker listening on vsock");

    // Shutdown channel: sender stays in `serve()`; clones go to each task.
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let mut sigterm = signal(SignalKind::terminate()).context("install SIGTERM handler")?;
    let mut sigint = signal(SignalKind::interrupt()).context("install SIGINT handler")?;

    loop {
        tokio::select! {
            res = listener.accept() => {
                match res {
                    Ok((stream, peer)) => {
                        info!(?peer, "accepted host connection");
                        let wd = work_dir.clone();
                        let rx = shutdown_rx.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(stream, wd, rx).await {
                                warn!(err = %e, "connection ended with error");
                            }
                        });
                    }
                    Err(e) => {
                        error!(err = %e, "vsock accept failed; exiting");
                        // Signal all active connections to stop before we return.
                        let _ = shutdown_tx.send(true);
                        return Err(e.into());
                    }
                }
            }
            _ = sigterm.recv() => {
                info!("SIGTERM received; shutting down");
                let _ = shutdown_tx.send(true);
                return Ok(());
            }
            _ = sigint.recv() => {
                info!("SIGINT received; shutting down");
                let _ = shutdown_tx.send(true);
                return Ok(());
            }
        }
    }
}

async fn handle_connection(
    stream: tokio_vsock::VsockStream,
    work_dir: PathBuf,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let (read_half, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);

    loop {
        // Poll for the next request *or* a shutdown signal concurrently so the
        // connection loop always remains responsive to graceful-shutdown events.
        let req = tokio::select! {
            read_res = framing::read_request(&mut reader) => {
                match read_res {
                    Ok(r) => r,
                    Err(pi_sandbox_protocol::ProtocolError::Eof) => {
                        info!("host closed connection; loop ends");
                        return Ok(());
                    }
                    Err(e) => {
                        warn!(err = %e, "read_request failed; closing connection");
                        return Err(e.into());
                    }
                }
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    info!("shutdown signal received; closing connection");
                    return Ok(());
                }
                // Spurious (value unchanged) — keep looping.
                continue;
            }
        };

        let started = Instant::now();
        let response = dispatch_request(req, &work_dir).await;
        let elapsed = started.elapsed().as_millis() as u32;
        let response = pi_sandbox_protocol::ToolResponse {
            guest_duration_ms: elapsed,
            ..response
        };
        framing::write_response(&mut writer, &response).await?;
    }
}
