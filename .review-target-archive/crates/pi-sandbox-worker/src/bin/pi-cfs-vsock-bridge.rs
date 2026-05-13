//! Guest-side bridge for the contextfs `/work` mount (RFD 0023
//! §3.5 / Commit G3 step 2b).
//!
//! Listens on a Unix-domain socket at `/run/cfs.sock`. For each
//! accepted connection, opens a vsock connection to
//! (HOST_CID=2, port=5005) and forwards bytes both directions
//! until either side hangs up.
//!
//! Mirrors the pasta+nft topology: contextfs's daemons (in
//! particular the `remote-fs` backend) only know how to speak
//! over UDS; vsock is pi-rs's transport choice. This bridge
//! makes them composable without contextfs needing to know about
//! vsock.
//!
//! Spawned by the rootfs init script BEFORE contextfsd, so
//! contextfsd's first connect on /run/cfs.sock succeeds.

#[cfg(target_os = "linux")]
#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    use std::path::Path;
    use tokio::io::AsyncWriteExt;
    use tokio::net::UnixListener;
    use tokio_vsock::{VsockAddr, VsockStream};

    const SOCKET_PATH: &str = "/run/cfs.sock";
    const HOST_CID: u32 = 2;
    const VSOCK_CFS_PORT: u32 = 5005;

    // Stale socket from a prior run — remove before bind.
    let _ = std::fs::remove_file(SOCKET_PATH);
    if let Some(parent) = Path::new(SOCKET_PATH).parent() {
        std::fs::create_dir_all(parent)?;
    }
    let listener = UnixListener::bind(SOCKET_PATH)?;
    eprintln!("pi-cfs-vsock-bridge: listening on {}", SOCKET_PATH);

    loop {
        let (uds, _) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                eprintln!("pi-cfs-vsock-bridge: accept failed: {e}");
                continue;
            }
        };
        tokio::spawn(async move {
            let vsock = match VsockStream::connect(VsockAddr::new(HOST_CID, VSOCK_CFS_PORT)).await
            {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("pi-cfs-vsock-bridge: dial vsock(2,5005) failed: {e}");
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
    eprintln!("pi-cfs-vsock-bridge is Linux-only (vsock).");
    std::process::exit(2);
}
