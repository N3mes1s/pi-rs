//! Vsock listener — accepts host connections, runs the
//! request/response loop per connection, handles SIGTERM /
//! SIGINT for graceful shutdown.

use crate::dispatch::dispatch_request;
use anyhow::Context;
use pi_sandbox_protocol::framing;
use std::path::PathBuf;
use std::time::Instant;
use tokio::io::BufReader;
use tokio::signal::unix::{signal, SignalKind};
use tokio_vsock::{VsockAddr, VsockListener, VMADDR_CID_ANY};
use tracing::{error, info, warn};

/// Start the vsock listener loop. Runs until SIGTERM/SIGINT or accept error.
pub async fn serve(vsock_port: u32, work_dir: PathBuf) -> anyhow::Result<()> {
    // VMADDR_CID_ANY is the conventional "listen for any host CID".
    let addr = VsockAddr::new(VMADDR_CID_ANY, vsock_port);
    let listener = VsockListener::bind(addr)
        .with_context(|| format!("failed to bind vsock listener on port {vsock_port}"))?;
    info!(port = vsock_port, work_dir = %work_dir.display(),
          "pi-sandbox-worker listening on vsock");

    let mut sigterm = signal(SignalKind::terminate()).context("install SIGTERM handler")?;
    let mut sigint = signal(SignalKind::interrupt()).context("install SIGINT handler")?;

    loop {
        tokio::select! {
            res = listener.accept() => {
                match res {
                    Ok((stream, peer)) => {
                        info!(?peer, "accepted host connection");
                        let wd = work_dir.clone();
                        if let Err(e) = handle_connection(stream, wd).await {
                            warn!(err = %e, "connection ended with error");
                        }
                    }
                    Err(e) => {
                        error!(err = %e, "vsock accept failed; exiting");
                        return Err(e.into());
                    }
                }
            }
            _ = sigterm.recv() => {
                info!("SIGTERM received; shutting down");
                return Ok(());
            }
            _ = sigint.recv() => {
                info!("SIGINT received; shutting down");
                return Ok(());
            }
        }
    }
}

async fn handle_connection(
    stream: tokio_vsock::VsockStream,
    work_dir: PathBuf,
) -> anyhow::Result<()> {
    let (read_half, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);

    loop {
        let req = match framing::read_request(&mut reader).await {
            Ok(r) => r,
            Err(pi_sandbox_protocol::ProtocolError::Eof) => {
                info!("host closed connection; loop ends");
                return Ok(());
            }
            Err(e) => {
                warn!(err = %e, "read_request failed; closing connection");
                return Err(e.into());
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
