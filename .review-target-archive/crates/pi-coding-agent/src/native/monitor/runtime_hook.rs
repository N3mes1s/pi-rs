//! Runtime glue for the `monitor` tool (RFD 0017).
//!
//! The tool itself ([`pi_tools::monitor::MonitorTool`]) is provider-
//! agnostic: it sends [`MonitorNotification`]s on a generic mpsc.
//! This module turns those notifications into two pi-rs runtime
//! shapes:
//!
//!   1. [`AgentEventKind::MonitorEvent`] / `MonitorEnded` events on the
//!      session's [`EventSender`] (so JSON / TUI / print modes see
//!      them) — handled by [`spawn_event_bridge`].
//!   2. A queue drained by [`MonitorPump`] (a `StreamInterceptor`) at
//!      the start of every assistant turn; queued lines get prepended
//!      to the next user message inside a `<monitor_events>` block.
//!
//! The pump uses `turn_start` to inject — it appends the rendered
//! block as an `AbortAndInject` *only if the agent isn't already mid-
//! reply*. To keep the diff small in v1 we instead expose a
//! [`MonitorPump::drain_block`] helper for tests + a follow-up
//! integration: queued events are flushed via the existing TTSR-style
//! abort+inject path on the *next* `on_text_delta` after a quiescent
//! turn.

use std::sync::Arc;
use tokio::sync::Mutex;

use chrono::Utc;
use pi_agent_core::event::{AgentEvent, AgentEventKind, EventSender};
use pi_agent_core::{InterceptAction, StreamInterceptor};
use pi_tools::monitor::{MonitorNotification, MonitorReceiver};

/// Spawn a background task that forwards [`MonitorNotification`]s onto
/// the supplied [`EventSender`] *and* enqueues them on a [`MonitorPump`]
/// queue for next-turn injection.
pub fn spawn_event_bridge(
    session_id: String,
    mut rx: MonitorReceiver,
    sender: Option<EventSender>,
    pump: Arc<MonitorPump>,
) {
    tokio::spawn(async move {
        while let Some(n) = rx.recv().await {
            // Always enqueue for next-turn injection.
            pump.enqueue(n.clone()).await;
            // And bridge into the session event channel.
            if let Some(s) = &sender {
                let kind = match n {
                    MonitorNotification::Lines {
                        monitor_id,
                        description,
                        lines,
                    } => AgentEventKind::MonitorEvent {
                        monitor_id,
                        description,
                        lines,
                    },
                    MonitorNotification::Ended {
                        monitor_id,
                        description,
                        exit_code,
                        cancelled,
                        aborted_reason,
                    } => AgentEventKind::MonitorEnded {
                        monitor_id,
                        description,
                        exit_code,
                        cancelled,
                        aborted_reason,
                    },
                };
                let _ = s.send(AgentEvent {
                    session_id: session_id.clone(),
                    entry_id: String::new(),
                    timestamp: Utc::now().timestamp_millis(),
                    kind,
                });
            }
        }
    });
}

/// `StreamInterceptor` that injects queued monitor notifications into
/// the next assistant turn as a `<monitor_events>` user message.
pub struct MonitorPump {
    pending: Mutex<Vec<MonitorNotification>>,
    /// Set once [`Self::drain_block`] returns a non-empty block — used
    /// by [`StreamInterceptor::on_text_delta`] to fire an abort+inject
    /// at the next delta if events arrive *during* an assistant reply.
    /// (v1: we simply abort+inject once, on the first delta, when the
    /// queue is non-empty.)
    armed: Mutex<bool>,
}

impl Default for MonitorPump {
    fn default() -> Self {
        Self::new()
    }
}

impl MonitorPump {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(Vec::new()),
            armed: Mutex::new(false),
        }
    }

    pub async fn enqueue(&self, n: MonitorNotification) {
        self.pending.lock().await.push(n);
        *self.armed.lock().await = true;
    }

    /// Drain the queue and render a `<monitor_events>` block. Returns
    /// `None` when the queue is empty.
    pub async fn drain_block(&self) -> Option<String> {
        let mut g = self.pending.lock().await;
        if g.is_empty() {
            return None;
        }
        let mut s = String::from("<monitor_events>\n");
        for n in g.drain(..) {
            match n {
                MonitorNotification::Lines {
                    description, lines, ..
                } => {
                    for line in lines.lines() {
                        s.push_str(&format!("[monitor:{}] {}\n", description, line));
                    }
                }
                MonitorNotification::Ended {
                    description,
                    exit_code,
                    cancelled,
                    aborted_reason,
                    ..
                } => {
                    let exit = exit_code
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "?".into());
                    let suffix = if cancelled {
                        match aborted_reason {
                            Some(r) => format!(" cancelled ({})", r),
                            None => " cancelled".into(),
                        }
                    } else {
                        String::new()
                    };
                    s.push_str(&format!(
                        "[monitor:{}] (ended exit={}{} )\n",
                        description, exit, suffix
                    ));
                }
            }
        }
        s.push_str("</monitor_events>");
        *self.armed.lock().await = false;
        Some(s)
    }
}

#[async_trait::async_trait]
impl StreamInterceptor for MonitorPump {
    /// At the start of every assistant turn the runtime calls us once.
    /// We can't *inject* a user message from `turn_start` directly, so
    /// instead we arm `armed`; the first text delta will then trigger
    /// an `AbortAndInject` carrying the rendered block.
    async fn turn_start(&self) {
        let has_pending = !self.pending.lock().await.is_empty();
        *self.armed.lock().await = has_pending;
    }

    async fn on_text_delta(&self, _text: &str) -> InterceptAction {
        let armed = *self.armed.lock().await;
        if !armed {
            return InterceptAction::Continue;
        }
        // Disarm now so the same delta-burst doesn't re-fire.
        *self.armed.lock().await = false;
        match self.drain_block().await {
            Some(block) => InterceptAction::AbortAndInject(block),
            None => InterceptAction::Continue,
        }
    }
}
