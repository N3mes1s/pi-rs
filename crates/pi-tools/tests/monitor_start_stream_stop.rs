//! RFD 0017 — happy-path test.
//! Start a short bash loop, collect notifications, assert exactly 3
//! `Lines` events (each one line) plus one `Ended { exit_code: Some(0),
//! cancelled: false }`.

use pi_tools::monitor::{MonitorConfig, MonitorNotification, MonitorTool};
use pi_tools::{Tool, ToolContext};
use serde_json::json;
use std::time::Duration;

#[tokio::test]
async fn monitor_start_stream_stop_happy_path() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    // Lower batch window so each `sleep 0.2` line lands in its own batch.
    let cfg = MonitorConfig {
        batch_window: Duration::from_millis(50),
        ..Default::default()
    };
    let tool = MonitorTool::new(tx, cfg);

    let ctx = ToolContext::default();
    let result = tool
        .invoke(
            &ctx,
            "call_1",
            json!({
                "op": "start",
                "command": "for i in 1 2 3; do echo line $i; sleep 0.2; done",
                "description": "loop3"
            }),
        )
        .await
        .expect("start ok");
    assert!(!result.is_error);

    let mut line_events = Vec::new();
    let mut ended = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while ended.is_none() && std::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
            Ok(Some(MonitorNotification::Lines { lines, .. })) => line_events.push(lines),
            Ok(Some(MonitorNotification::Ended {
                exit_code,
                cancelled,
                ..
            })) => ended = Some((exit_code, cancelled)),
            _ => {}
        }
    }

    let (exit, cancelled) = ended.expect("Ended notification");
    assert_eq!(exit, Some(0));
    assert!(!cancelled);
    let total_lines: usize = line_events.iter().map(|s| s.lines().count()).sum();
    assert_eq!(total_lines, 3, "got {:?}", line_events);
}
