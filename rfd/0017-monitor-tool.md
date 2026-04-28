# RFD 0017 — Native `monitor` tool for streaming background events

- **Status:** Discussion
- **Author:** pi-rs maintainers
- **Created:** 2026-04-28
- **Implemented:** &lt;pending&gt;

## Summary

Add a native `monitor` tool to pi-rs that runs a long-lived shell
command in the background and streams its stdout back to the agent
**one notification per line**. Each new line wakes the agent loop
just like a tool result, lets the agent decide whether to act, and
costs no tokens during silence. Models the same primitive Claude
Code shipped in v2.1.98 ([Aiia 2026](https://aiia.ro/blog/claude-code-monitor-tool-background-scripts/),
[Anthropic Code Docs](https://code.claude.com/docs/en/monitoring-usage))
but wired through pi-rs's existing `pi_tools::Tool` trait + agent
event channel.

This is the missing primitive between `bash` (one-shot, blocking,
returns at end) and `task` (delegates a whole sub-conversation):
**a watch primitive** for "tell me when something happens." Use cases
mirror Claude Code's: dev-server stdout, `tail -f` on app logs,
poll-loops over GitHub PR checks, watching a long `cargo test`,
streaming Python script progress.

## Background

Pi-rs has two adjacent primitives today:

* `pi_tools::bash::BashTool` — runs a shell command, captures stdout
  + stderr, returns one ToolResult when it exits. Blocks the agent
  loop. Killed at a global default timeout (~120 s).
* `pi_tools::Tool` trait — `async fn invoke(...) -> Result<ToolResult,
  ToolError>` — fundamentally one-shot. There's no shape for
  "many results trickling in over time."

Claude Code's [Monitor tool](https://aiia.ro/blog/claude-code-monitor-tool-background-scripts/)
is a different shape: spawn → stream → emit one event per line →
agent processes selectively → either auto-end on script exit or
stay live for the session. Each line is a notification, not a tool
result; cost-free during idle (the agent's API turn doesn't include
silent watcher state).

Pi-rs's runtime already has the right primitive: `AgentEvent`
emitted via `EventSender` on a `tokio::sync::mpsc::UnboundedReceiver`
(see `pi_agent_core::event`). Mode handlers (print, json, tui)
already consume the channel. Adding **one new variant** to
`AgentEventKind` is enough to surface monitor lines.

References:
* [Piebald's mirror of Claude Code's Monitor description](https://github.com/Piebald-AI/claude-code-system-prompts/blob/main/system-prompts/tool-description-background-monitor-streaming-events.md)
  — best practices: grep `--line-buffered`, merge stderr with `2>&1`,
  widen alternation grep filters to cover failure signals, batch
  lines within 200 ms.
* [Anthropic harness design](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents)
  — the SDK's async-iterator streaming model maps naturally to one
  notification per stdout line.
* [Claude Code Monitor blog post — MindStudio](https://www.mindstudio.ai/blog/claude-code-monitor-tool-background-processes)
  — concrete poll-loop examples for CI checks + log tailing.
* Existing pi-rs tools to mirror: `pi_tools::bash::BashTool`
  (process spawn pattern), `pi_coding_agent::native::task::tool`
  (long-lived async work pattern with tokio::task_local).

## Proposal

### 1. New tool: `pi_tools::monitor`

```rust
// crates/pi-tools/src/monitor.rs
pub struct MonitorTool {
    /// Map of monitor_id → handle (kill-switch + receiver). Allows
    /// `monitor` op = "stop" and the runtime to clean up on session
    /// abort.
    handles: Arc<DashMap<String, MonitorHandle>>,
}

#[async_trait]
impl Tool for MonitorTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "monitor".into(),
            description:
                "Run a long-lived background command whose stdout streams \
                 back as one notification per line. Use for tail -f, dev \
                 servers, poll loops over CI / PR state, anywhere you'd \
                 want `bash` if it didn't block. Pipe through \
                 `grep --line-buffered` to filter — every stdout line \
                 becomes a chat message. Set `persistent: true` for \
                 session-length watches that exit only on TaskStop. \
                 Ends naturally when the script exits.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "op": {
                        "type": "string",
                        "enum": ["start", "stop", "list"],
                        "description": "start a new watcher, stop an \
                         existing one (by id), or list active ones."
                    },
                    "command": {
                        "type": "string",
                        "description": "Shell command. Required for `start`."
                    },
                    "description": {
                        "type": "string",
                        "description": "Short label shown in event \
                         notifications. Required for `start`."
                    },
                    "persistent": {
                        "type": "boolean", "default": false,
                        "description": "Run for the lifetime of the \
                         session. Default: bounded by `timeout_ms`."
                    },
                    "timeout_ms": {
                        "type": "integer", "default": 300000,
                        "description": "Auto-stop deadline when not \
                         persistent. Default 5 min, max 1 h."
                    },
                    "id": {
                        "type": "string",
                        "description": "Required for `stop`. Returned \
                         from a prior `start`."
                    }
                },
                "required": ["op"]
            }),
        }
    }

    fn read_only(&self) -> bool { false }

    async fn invoke(&self, ctx: &ToolContext, call_id: &str, input: Value)
        -> Result<ToolResult, ToolError>;
}
```

The tool dispatches on `op`:

* `start` → fork a child process with `tokio::process::Command`, take
  its stdout pipe, spawn a reader task that reads lines (newline-
  delimited; **batches lines within a 200 ms window** to match
  Claude Code's behaviour and reduce notification chatter), pushes
  each batch to the runtime via a new `AgentEventKind` variant,
  records a `MonitorHandle` with the child's `Pid` + cancel sender.
  Returns a `ToolResult` whose `display.monitor` contains
  `{ id, pid, description, started_at }`. Agent now has the id and
  can `stop` later.
* `stop` → look up the id, send a cancel + `kill -TERM` to the pid,
  wait briefly, then `kill -9` if needed. Return a `ToolResult`
  with `display.stopped: true`.
* `list` → return `display.monitors: [{ id, command, description,
  pid, started_at, persistent, status }]`.

### 2. New `AgentEventKind` variant

```rust
// crates/pi-agent-core/src/event.rs
pub enum AgentEventKind {
    // ... existing variants ...
    /// One notification from a `monitor` tool. Each notification is
    /// either one line of the watched command's stdout (when batching
    /// produces a single line) or several lines joined with `\n`
    /// (200 ms window). RFD 0017.
    MonitorEvent {
        /// Monitor id from the original `start` ToolResult.
        monitor_id: String,
        /// Short label set when the monitor was started.
        description: String,
        /// One or more stdout lines, joined with `\n`.
        lines: String,
    },
    /// Emitted exactly once when a monitor exits (script terminated
    /// or `stop` op called). Carries the exit code.
    MonitorEnded {
        monitor_id: String,
        description: String,
        exit_code: Option<i32>,
        /// `true` when the agent called `stop`; `false` when the
        /// child exited on its own.
        cancelled: bool,
    },
}
```

### 3. Runtime injection — feeding events back into the message stream

The agent loop needs to surface `MonitorEvent`s as
**synthetic user messages** so the next assistant turn can act on
them. The cleanest plumbing reuses the existing TTSR
`StreamInterceptor` pattern (the one that injects
`<system_reminder>` mid-stream):

```rust
// crates/pi-coding-agent/src/native/monitor/runtime_hook.rs (sketch)
pub struct MonitorPump {
    rx: tokio::sync::mpsc::UnboundedReceiver<MonitorEvent>,
    pending: Mutex<Vec<MonitorEvent>>,
}

#[async_trait]
impl pi_agent_core::StreamInterceptor for MonitorPump {
    async fn turn_start(&self) {
        // Drain all pending monitor events and prepend them to the
        // next user turn as a single <monitor_events> block.
    }
    async fn on_text_delta(&self, _: &str) -> InterceptAction {
        InterceptAction::Continue
    }
}
```

The runtime hook gets registered at `RuntimeConfig.stream_interceptor`
the same way TTSR does. When the agent's next turn begins, all
queued monitor events get folded into the user message as:

```text
<monitor_events>
[monitor:dev-server] Compiled in 1.2s with 0 warnings.
[monitor:dev-server] error[E0277]: trait bound `…` is not satisfied
[monitor:ci-checks] my-test ✓ passed
</monitor_events>
```

### 4. Lifecycle + safety

* **Per-session limits**: max **8 concurrent monitors** per session
  (configurable via `Settings::monitor::max_concurrent`). Hitting
  the cap rejects new `start` calls with a clean ToolError.
* **Auto-stop on session abort**: the session's `Drop` impl walks
  the `MonitorTool::handles` map and kills every child.
* **Output volume guardrail**: a monitor that emits >100 lines
  in 5 seconds gets auto-stopped with a ToolError telling the
  agent its filter is too wide. (Mirrors Claude Code's "automatically
  stopped if too many events" rule.)
* **Permission gate**: `MonitorTool::read_only() == false`, so
  the existing `auto_approve::AutoApproveGate` checks apply.
  `bash`-style allow/deny patterns extend transparently.

### 5. CLI surface

A small `pi --monitor` subcommand mirrors `pi --stats` /
`pi --evolve` for diagnostics:

```text
pi --monitor list     # list active monitors across all sessions
pi --monitor stop ID  # force-stop one
```

(The agent-facing path is the `monitor` tool input; the CLI is for
humans who notice a runaway watcher in `ps` and want to clean up.)

### 6. Tool prompt rules

The tool's `description` (above) embeds the same usage discipline
Claude Code teaches:

* Use `grep --line-buffered` in pipes to avoid block-buffering.
* Handle transient failures in poll loops (`curl … || true`).
* Merge stderr with `2>&1` for `cargo test`-style commands so the
  filter sees panic lines.
* Filter for **terminal states, not just the happy path** —
  silence on a crash looks identical to silence on success.
* Prefer `until <cond>; do sleep 2; done` for one-shot waits;
  reach for `monitor` only when many notifications are expected.

These rules go verbatim into the tool's `description` so the agent
sees them every time it picks up the spec.

## Test plan

1. **`tests/monitor_start_stream_stop.rs`** — happy path. Start a
   `for i in 1 2 3; do echo line $i; sleep 0.05; done` monitor;
   collect events from the receiver; assert exactly 3
   `MonitorEvent` lines + 1 `MonitorEnded { exit_code: Some(0),
   cancelled: false }`.
2. **`tests/monitor_explicit_stop.rs`** — start `tail -f /tmp/X`;
   write a few lines to `/tmp/X`; call `op: stop`; assert the
   final event is `MonitorEnded { cancelled: true }`.
3. **`tests/monitor_batching_window.rs`** — emit 5 lines within
   the 200 ms window; assert one `MonitorEvent` whose `lines`
   has 5 newline-separated entries (not 5 separate events).
4. **`tests/monitor_volume_guardrail.rs`** — emit 200 lines in 1
   second; assert the monitor auto-stops within 6 s and the
   `MonitorEnded` carries a `cancelled: true` plus a
   `display.aborted_reason: "volume_cap"` field on the ended
   tool result.
5. **`tests/monitor_runtime_pump_injects_into_next_turn.rs`** —
   end-to-end: register `MonitorPump`, queue 3 events, run
   `session.prompt(...)`; assert the next outgoing user message
   to the provider contains the literal `<monitor_events>` block
   with the 3 lines.
6. **`tests/monitor_session_drop_kills_children.rs`** — start a
   monitor, drop the `AgentSession`, assert the child process is
   gone (poll `kill -0 <pid>` for ~1 s).

## Out of scope

- **TUI live panel** showing active monitors. v1 ships only the
  agent-facing tool + a CLI `--monitor list` for diagnostics.
- **Distributed monitors** (`monitor` watching another machine
  via SSH). Out of scope; users can wrap `ssh host "tail -f …"`
  themselves.
- **Persistence across sessions.** Each session starts with zero
  monitors; restoring on `pi --resume` is RFD 0019.
- **Output capture beyond stdout.** stderr is the user's
  responsibility (`2>&1` in their command). v1 doesn't split.
- **Programmable filters** beyond grep-in-pipe. The tool runs the
  command verbatim — the user handles filtering with shell tools.

## Open questions

- **Naming**: `monitor` overlaps slightly with the macOS process
  monitor. Alternatives: `watch`, `tail`, `streaming`, `bash_bg`.
  Lean `monitor` to match Claude Code (familiar) — listed for the
  record.
- **Should `MonitorEvent` go through `AgentEvent` or a separate
  channel?** Decided: through `AgentEvent` — one channel, same
  serialization in `--json` mode, no new wire format.
- **Should the 200 ms batching window be configurable?** Yes,
  but as a *worktree-level* setting in `Settings::monitor::
  batch_window_ms`, not per-monitor input. Default 200 ms.
- **What happens if the agent calls `start` 9 times when the
  cap is 8?** Reject with ToolError naming the cap. The agent
  should `list` + `stop` an old one before retrying.

## Sources

- [Claude Code Monitor Tool announcement (Aiia, April 2026)](https://aiia.ro/blog/claude-code-monitor-tool-background-scripts/)
- [Claude Code Monitor Tool: Stop Polling, Start Reacting (claudefa.st)](https://claudefa.st/blog/guide/mechanics/monitor)
- [What Is the Claude Code Monitor Tool? (MindStudio)](https://www.mindstudio.ai/blog/claude-code-monitor-tool-background-processes)
- [Piebald mirror of the Monitor tool description](https://github.com/Piebald-AI/claude-code-system-prompts/blob/main/system-prompts/tool-description-background-monitor-streaming-events.md)
- [Anthropic — Effective harnesses for long-running agents](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents)
- [Anthropic Code Docs — Monitoring usage](https://code.claude.com/docs/en/monitoring-usage)
