//! RFD 0017 — verify the `MonitorPump` injects queued events into the
//! next assistant turn via the [`StreamInterceptor`] contract.
//!
//! Mirror of `tests/native_ttsr.rs`'s mechanical interceptor test:
//! we don't need a real provider to exercise the `turn_start` →
//! `on_text_delta` → `AbortAndInject` flow.

use pi_agent_core::{InterceptAction, StreamInterceptor};
use pi_coding_agent::native::monitor::MonitorPump;
use pi_tools::monitor::MonitorNotification;

#[tokio::test]
async fn monitor_pump_injects_into_next_turn() {
    let pump = MonitorPump::new();

    // Queue 3 events.
    pump.enqueue(MonitorNotification::Lines {
        monitor_id: "mon_1".into(),
        description: "dev-server".into(),
        lines: "Compiled in 1.2s".into(),
    })
    .await;
    pump.enqueue(MonitorNotification::Lines {
        monitor_id: "mon_1".into(),
        description: "dev-server".into(),
        lines: "warning: unused import".into(),
    })
    .await;
    pump.enqueue(MonitorNotification::Lines {
        monitor_id: "mon_2".into(),
        description: "ci-checks".into(),
        lines: "my-test ✓ passed".into(),
    })
    .await;

    // turn_start arms the pump.
    pump.turn_start().await;

    // First text delta of the next assistant turn should fire an
    // AbortAndInject carrying the rendered <monitor_events> block.
    let action = pump.on_text_delta("hello, ").await;
    let block = match action {
        InterceptAction::AbortAndInject(s) => s,
        InterceptAction::Continue => panic!("expected AbortAndInject"),
    };

    assert!(block.contains("<monitor_events>"));
    assert!(block.contains("</monitor_events>"));
    assert!(block.contains("[monitor:dev-server] Compiled in 1.2s"));
    assert!(block.contains("[monitor:dev-server] warning: unused import"));
    assert!(block.contains("[monitor:ci-checks] my-test ✓ passed"));

    // After firing once, the queue is drained — a second delta in the
    // same turn must not re-fire.
    let again = pump.on_text_delta("world").await;
    assert_eq!(again, InterceptAction::Continue);

    // And starting a fresh turn with no new events also stays Continue.
    pump.turn_start().await;
    assert_eq!(
        pump.on_text_delta("anything").await,
        InterceptAction::Continue,
    );
}

#[tokio::test]
async fn monitor_pump_renders_ended_marker() {
    let pump = MonitorPump::new();
    pump.enqueue(MonitorNotification::Ended {
        monitor_id: "mon_1".into(),
        description: "dev-server".into(),
        exit_code: Some(0),
        cancelled: false,
        aborted_reason: None,
    })
    .await;
    pump.turn_start().await;
    let block = match pump.on_text_delta("x").await {
        InterceptAction::AbortAndInject(s) => s,
        _ => panic!("expected inject"),
    };
    assert!(block.contains("[monitor:dev-server]"));
    assert!(block.contains("ended exit=0"));
}
