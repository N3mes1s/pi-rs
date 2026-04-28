//! RFD 0017 — session-drop kills children.
//! Drop the `MonitorTool` (after explicit stop_all): poll `kill -0` on
//! the recorded pid. Should be gone within ~1 second.

use pi_tools::monitor::{MonitorConfig, MonitorTool};
use pi_tools::{Tool, ToolContext};
use serde_json::json;
use std::time::Duration;

fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    unsafe {
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        kill(pid as i32, 0) == 0
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

#[tokio::test]
async fn monitor_session_drop_kills_children() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let tool = MonitorTool::new(tx, MonitorConfig::default());

    let ctx = ToolContext::default();
    let r = tool
        .invoke(
            &ctx,
            "c",
            json!({
                "op": "start",
                "command": "sleep 60",
                "description": "long-sleeper",
                "persistent": true,
            }),
        )
        .await
        .unwrap();
    let pid = r.display.unwrap()["monitor"]["pid"].as_u64().unwrap() as u32;
    assert!(pid_alive(pid));

    // Simulate session drop: stop_all then drop.
    tool.stop_all();
    drop(tool);

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline && pid_alive(pid) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!pid_alive(pid), "child pid {pid} still alive after stop_all + drop");
}
