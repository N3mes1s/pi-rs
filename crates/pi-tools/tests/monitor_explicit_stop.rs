//! RFD 0017 — explicit `stop` op.
//! Start `tail -f /tmp/X`; write lines; call `op:stop`; expect the final
//! notification to be `Ended { cancelled: true, .. }`.

use pi_tools::monitor::{MonitorConfig, MonitorNotification, MonitorTool};
use pi_tools::{Tool, ToolContext};
use serde_json::json;
use std::time::Duration;

#[tokio::test]
async fn monitor_explicit_stop() {
    let path = std::env::temp_dir().join(format!("monitor_stop_{}.log", std::process::id()));
    std::fs::write(&path, "").unwrap();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let tool = MonitorTool::new(
        tx,
        MonitorConfig {
            batch_window: Duration::from_millis(50),
            ..Default::default()
        },
    );

    let ctx = ToolContext::default();
    let r = tool
        .invoke(
            &ctx,
            "call_start",
            json!({
                "op": "start",
                "command": format!("tail -f {}", path.display()),
                "description": "tailF",
            }),
        )
        .await
        .unwrap();
    let id = r.display.unwrap()["monitor"]["id"].as_str().unwrap().to_string();

    // Push a couple of lines through the file.
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(f, "alpha").unwrap();
    writeln!(f, "beta").unwrap();
    drop(f);

    // Wait for at least one Lines event.
    let mut got_lines = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if let Ok(Some(MonitorNotification::Lines { .. })) =
            tokio::time::timeout(Duration::from_millis(200), rx.recv()).await
        {
            got_lines = true;
            break;
        }
    }
    assert!(got_lines, "expected at least one Lines event");

    let r = tool
        .invoke(&ctx, "call_stop", json!({ "op": "stop", "id": id }))
        .await
        .unwrap();
    assert_eq!(r.display.unwrap()["stopped"], json!(true));

    // Drain until we see Ended.
    let mut ended = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while ended.is_none() && std::time::Instant::now() < deadline {
        if let Ok(Some(n)) = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            if let MonitorNotification::Ended { cancelled, .. } = n {
                ended = Some(cancelled);
            }
        }
    }
    assert_eq!(ended, Some(true), "expected cancelled Ended");
    let _ = std::fs::remove_file(&path);
}
