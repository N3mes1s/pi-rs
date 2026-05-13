//! RFD 0017 — volume guardrail.
//! Emit 200 lines in ~1 second; the monitor should auto-stop with
//! `cancelled: true, aborted_reason: Some("volume_cap")`.

use pi_tools::monitor::{MonitorConfig, MonitorNotification, MonitorTool};
use pi_tools::{Tool, ToolContext};
use serde_json::json;
use std::time::Duration;

#[tokio::test]
async fn monitor_volume_guardrail() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let tool = MonitorTool::new(
        tx,
        MonitorConfig {
            batch_window: Duration::from_millis(50),
            volume_cap_lines: 100,
            volume_cap_window: Duration::from_secs(5),
            ..Default::default()
        },
    );

    let ctx = ToolContext::default();
    tool.invoke(
        &ctx,
        "c",
        json!({
            "op": "start",
            "command": "for i in $(seq 1 500); do echo line $i; done; sleep 30",
            "description": "flood",
        }),
    )
    .await
    .unwrap();

    let mut ended: Option<(bool, Option<String>)> = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    while ended.is_none() && std::time::Instant::now() < deadline {
        if let Ok(Some(n)) = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
            if let MonitorNotification::Ended {
                cancelled,
                aborted_reason,
                ..
            } = n
            {
                ended = Some((cancelled, aborted_reason));
            }
        }
    }
    let (cancelled, reason) = ended.expect("Ended within 8s");
    assert!(cancelled, "should be marked cancelled");
    assert_eq!(reason.as_deref(), Some("volume_cap"));
}
