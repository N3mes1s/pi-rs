//! RFD 0017 — batching window.
//! Emit 5 lines within a single 200 ms window; assert one Lines
//! notification carrying all 5 newline-separated.

use pi_tools::monitor::{MonitorConfig, MonitorNotification, MonitorTool};
use pi_tools::{Tool, ToolContext};
use serde_json::json;
use std::time::Duration;

#[tokio::test]
async fn monitor_batching_window() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let tool = MonitorTool::new(
        tx,
        MonitorConfig {
            batch_window: Duration::from_millis(300),
            ..Default::default()
        },
    );

    let ctx = ToolContext::default();
    // 5 echos with no sleep — they all fall inside one batch window.
    tool.invoke(
        &ctx,
        "c",
        json!({
            "op": "start",
            "command": "printf 'a\\nb\\nc\\nd\\ne\\n'; sleep 1",
            "description": "burst",
        }),
    )
    .await
    .unwrap();

    // First Lines event should carry all 5 lines.
    let mut first_lines: Option<String> = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while first_lines.is_none() && std::time::Instant::now() < deadline {
        if let Ok(Some(MonitorNotification::Lines { lines, .. })) =
            tokio::time::timeout(Duration::from_millis(500), rx.recv()).await
        {
            first_lines = Some(lines);
        }
    }
    let lines = first_lines.expect("got a Lines event");
    let count = lines.lines().count();
    assert_eq!(count, 5, "expected 5 lines in one batch, got: {lines:?}");
}
