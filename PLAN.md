# Implementation plan: RFD 0017 — Native monitor tool

> NOTE: The `planner` subagent (gpt-4o, with `read/grep/find/bash/lsp`) was
> invoked twice per the orchestration rules. Both runs returned an empty
> text response (the agent loop exited after 28 output tokens with no
> content blocks; see session
> `monitor-agent/sessions/_home_user_quartet_monitor/c2ea7ede-…jsonl`).
> The pattern was exercised; we fall back on the RFD itself, which the
> orchestration prompt explicitly designates as authoritative when the
> planner diverges or — as here — produces nothing.

## Acceptance criteria
- [ ] `monitor` tool with `start` / `stop` / `list` ops streams stdout
      one notification per (batched) line.
- [ ] `AgentEventKind::MonitorEvent` and `MonitorEnded` variants.
- [ ] 200 ms batching window (configurable via
      `Settings::monitor::batch_window_ms`).
- [ ] Per-session cap (default 8), volume guardrail (>100 lines / 5 s),
      auto-stop on session drop.
- [ ] `MonitorPump` `StreamInterceptor` injects pending events into the
      next user turn as `<monitor_events>…</monitor_events>`.
- [ ] CLI: `pi --monitor list` / `pi --monitor stop ID`.
- [ ] All 6 RFD tests green.

## Files to create
- `crates/pi-tools/src/monitor.rs` — tool impl + `MonitorTool`,
  `MonitorHandle`, batching reader, volume guard, kill switch,
  emits `MonitorEvent`/`MonitorEnded` via injected event sender.
- `crates/pi-coding-agent/src/native/monitor/mod.rs`,
  `crates/pi-coding-agent/src/native/monitor/runtime_hook.rs` —
  `MonitorPump` impl of `StreamInterceptor`, queue of events, render.
- `crates/pi-tools/tests/monitor_start_stream_stop.rs`
- `crates/pi-tools/tests/monitor_explicit_stop.rs`
- `crates/pi-tools/tests/monitor_batching_window.rs`
- `crates/pi-tools/tests/monitor_volume_guardrail.rs`
- `crates/pi-tools/tests/monitor_session_drop_kills_children.rs`
- `crates/pi-coding-agent/tests/monitor_runtime_pump_injects_into_next_turn.rs`

## Files to modify
- `crates/pi-tools/src/lib.rs` — `pub mod monitor;` re-export.
- `crates/pi-agent-core/src/event.rs` — add `MonitorEvent` +
  `MonitorEnded` variants on `AgentEventKind`.
- `crates/pi-agent-core/src/settings.rs` — add `MonitorSettings`
  (`max_concurrent: usize = 8`, `batch_window_ms: u64 = 200`,
  `volume_cap_lines: usize = 100`, `volume_cap_window_ms = 5000`).
- `crates/pi-agent-core/src/runtime.rs` — drain monitor handles on
  session drop; thread the `EventSender` clone into the
  `MonitorTool` registration site.
- `crates/pi-coding-agent/src/startup.rs` — register `MonitorTool`
  with the runtime's `EventSender`; register `MonitorPump` as
  `stream_interceptor`.
- `crates/pi-coding-agent/src/cli.rs` — `--monitor list / stop ID`.
- `rfd/0017-monitor-tool.md` — Status flip.
- `rfd/README.md` — index row.

## Order of operations
1. `AgentEventKind::MonitorEvent` + `MonitorEnded` variants.
2. `Settings::monitor` block + defaults.
3. `pi-tools::monitor` skeleton: spec, op dispatch, types.
4. Spawn + line reader + 200 ms batching + volume guard + kill on
   stop + emit `MonitorEvent`/`MonitorEnded`.
5. Tests for tool-level behaviour (1, 2, 3, 4, session-drop).
6. `MonitorPump` `StreamInterceptor` queueing + render.
7. Integration test (5).
8. Wire registration in `startup.rs`; CLI plumbing.
9. RFD bump.

## Risks
- tokio child + Drop: must explicit-kill via stored `Child` + `Pid`.
- The pump's `turn_start` runs on every turn — must drain mpsc
  non-blockingly.
- Volume guard timing window must use a sliding deque to avoid edge
  cases where 100 lines arrive in 4.99 s + 1 line in 5.01 s.
