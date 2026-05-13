//! Guest-side bridge for the contextfs broker UDS (RFD 0023
//! §3.5 / Commit G3 step 3 — Cedar/RW phase).
//!
//! Listens on a Unix-domain socket at
//! `/run/contextfs/broker.sock`. For each accepted connection,
//! opens a vsock connection to (HOST_CID=2, port=5006) and
//! forwards bytes both directions until either side hangs up.
//!
//! Twin of `pi-cfs-vsock-bridge` (which handles the
//! `/run/cfs.sock` ↔ vsock(2,5005) flow for the remote-fs file
//! ops). This sibling bridge handles the daemon ↔ broker flow
//! that gates writes via Cedar (`Request::VerifyWrite` /
//! `VerifyWriteBatch` per contextfs RFD-0020 / RFD-0024). The
//! broker itself runs on the host, kill_on_drop alongside the
//! VM lifecycle; pi-rs's host launcher binds the matching
//! `<vsock_path>_5006` UDS at cold-boot time.
//!
//! Spawned by the rootfs init script BEFORE contextfsd, so
//! contextfsd's first connect on `/run/contextfs/broker.sock`
//! succeeds.

#[cfg(target_os = "linux")]
#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    use std::path::Path;
    use tokio::io::AsyncWriteExt;
    use tokio::net::UnixListener;
    use tokio_vsock::{VsockAddr, VsockStream};

    const SOCKET_PATH: &str = "/run/contextfs/broker.sock";
    const HOST_CID: u32 = 2;
    const VSOCK_BROKER_PORT: u32 = 5006;

    let _ = std::fs::remove_file(SOCKET_PATH);
    if let Some(parent) = Path::new(SOCKET_PATH).parent() {
        std::fs::create_dir_all(parent)?;
    }
    let listener = UnixListener::bind(SOCKET_PATH)?;
    eprintln!("pi-cfs-broker-vsock-bridge: listening on {}", SOCKET_PATH);

    loop {
        let (uds, _) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                eprintln!("pi-cfs-broker-vsock-bridge: accept failed: {e}");
                continue;
            }
        };
        tokio::spawn(async move {
            let vsock = match VsockStream::connect(VsockAddr::new(
                HOST_CID,
                VSOCK_BROKER_PORT,
            ))
            .await
            {
                Ok(s) => s,
                Err(e) => {
                    eprintln!(
                        "pi-cfs-broker-vsock-bridge: dial vsock(2,{VSOCK_BROKER_PORT}) failed: {e}"
                    );
                    return;
                }
            };
            let (mut u_r, mut u_w) = uds.into_split();
            let (mut v_r, mut v_w) = tokio::io::split(vsock);
            let u_to_v = tokio::spawn(async move {
                let _ = tokio::io::copy(&mut u_r, &mut v_w).await;
                let _ = v_w.shutdown().await;
            });
            let v_to_u = tokio::spawn(async move {
                let _ = tokio::io::copy(&mut v_r, &mut u_w).await;
                let _ = u_w.shutdown().await;
            });
            let _ = u_to_v.await;
            let _ = v_to_u.await;
        });
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("pi-cfs-broker-vsock-bridge is Linux-only (vsock).");
    std::process::exit(2);
}
