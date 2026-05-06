# RFD 0031 — `monitor` streaming protocol for sandboxed providers

| Status | Discussion |
| ------ | ---------- |
| Author | opus-4.7 (1M context) |
| Date   | 2026-05-06 |
| Depends on | RFD 0017 (monitor tool), RFD 0023 (microvm sandbox provider), RFD 0026 (remote backends) |
| Supersedes | none |
| Related | RFD 0030 (lsp under microvm — sibling tool with overlapping streaming-notification needs) |

## Status note

RFD 0023 v1 marks `monitor` as `ToolDispatch::Unavailable` under
microvm/remote providers. The reason — encoded in the
`MonitorTool::dispatch()` override added in commit
`ea0b2ed` — is that `monitor` streams stdout as a sequence of
notifications on an out-of-band channel, and the current
`pi-sandbox-protocol` carries one `ToolRequest` →
`ToolResponse` pair per call before closing the channel.

This RFD designs the **streaming wire protocol** that lifts that
restriction, and specifies how it integrates with the per-call
hygiene + reset story in RFD 0023 §"Post-call hygiene".

## Summary

`monitor` (RFD 0017) starts a long-lived background command and
emits one `MonitorNotification::Lines { monitor_id, description,
lines }` per ~200 ms batch of stdout, plus one
`MonitorNotification::Ended { exit_code, cancelled,
aborted_reason }` when the child exits. Today these flow on a
`MonitorSender` channel built at agent-runtime startup.

Under sandbox dispatch the same call must:

1. Cross the vsock boundary (guest worker forks the child,
   batches its lines, ships them to host).
2. Land back on the **same** `MonitorSender` instance the
   runtime already wired up so existing UI / monitor-pump code
   doesn't change.
3. Survive per-call hygiene cleanly — the worker shouldn't
   garbage-collect a still-running monitor's child between
   tool calls.
4. Behave correctly across VM reset (`PI_SANDBOX_FC_MAX_CALLS=1`
   destroys the VM after each call; `monitor.start` starts a
   long-running process; the destroy kills the process —
   that's the right answer, document it).

The mechanism is a **multi-frame streaming response** on the
existing per-call vsock channel. After the worker accepts the
ToolRequest, instead of writing one ToolResponse and closing, it
writes:

```
MonitorFrame::Started { monitor_id }   -- one
MonitorFrame::Lines { monitor_id, description, lines }   -- 0..N
MonitorFrame::Ended { monitor_id, exit_code, cancelled, aborted_reason }   -- one
```

…then closes. Existing one-shot tools are unchanged: one
`ToolResponse` frame, then close. The only protocol change is
"the worker MAY emit `monitor`-typed frames before the terminal
ToolResponse for tool calls that opt in."

## Background

### Why `monitor` is currently `Unavailable`

`crates/pi-tools-core/src/monitor.rs::MonitorTool::invoke`
enqueues notifications on `self.sender: MonitorSender` (which is
a `tokio::sync::mpsc::UnboundedSender<MonitorNotification>`). The
notifications are read by a host-side pump
(`crates/pi-coding-agent/src/native/monitor_pump.rs` or similar)
that bridges them onto the agent event channel + UI.

`crates/pi-sandbox-protocol/src/lib.rs` declares one shape:

```rust
pub struct ToolRequest  { /* call_id, tool_name, tool_input, … */ }
pub struct ToolResponse { /* call_id, stdout, stderr, exit_status, is_error, … */ }
```

`framing` is newline-JSON, one frame per line. The worker's
`dispatch_request` is a flat function returning one
`ToolResponse`. Nothing in this protocol carries the stream of
notifications between the moment the agent issues `monitor.start`
and the moment the child exits.

Two consequences:

- **`monitor.start` returns synchronously** (correct), then
  notifications stream. Today the streaming part is invisible to
  the sandbox boundary because `monitor` is in `pi-tools-core`'s
  registry and runs in-process under `local-process`. Under
  `microvm`, that registry is in the GUEST and the
  `MonitorSender` is on the HOST — a chasm with no bridge.

- **`monitor.list` and `monitor.stop` are stateful** — they
  reference an in-memory `HashMap<String, MonitorHandle>` keyed
  by `monitor_id`. The handle owns the child process + the
  oneshot cancel channel. That state has to live on whichever
  side runs the child.

### Why this matters

`monitor` is the agent's only streaming-aware tool. The use
cases the RFD 0017 design doc names:
- `tail -f /var/log/foo` while the agent works on a fix
- `cargo watch` for re-builds during edits
- polling loops over GitHub PR state, CI status, deploy progress
- watching dev servers / hot-reload output

Without `monitor`, the agent falls back to repeatedly running
`bash` with `tail`/`grep`/`sleep` — which (a) is much more
expensive in tool calls, (b) misses bursts of activity between
polls, (c) doesn't compose with the agent's interrupt-on-event
flow that the runtime already supports.

Forcing operators to choose "sandbox boundary" vs "monitor
support" is, like LSP, a real UX regression vs `local-process`
mode.

## Design

### Wire-protocol extension

`pi-sandbox-protocol` currently declares one frame type per
direction. Extend with a `WorkerFrame` envelope:

```rust
// crates/pi-sandbox-protocol/src/lib.rs (NEW)

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum WorkerFrame {
    /// Final frame for one-shot tools (read/write/bash/etc.).
    /// Backward-compatible: existing host code that reads
    /// `ToolResponse` directly still works because the JSON
    /// has the same shape — just an extra `kind: "Response"`
    /// discriminator.
    Response(ToolResponse),

    /// Streaming frames for monitor-aware tools. Worker emits
    /// zero or more of these BEFORE a final `Response` carrying
    /// the call's terminal status.
    MonitorStarted { call_id: String, monitor_id: String },

    MonitorLines {
        call_id: String,
        monitor_id: String,
        description: String,
        lines: String,
    },

    MonitorEnded {
        call_id: String,
        monitor_id: String,
        exit_code: Option<i32>,
        cancelled: bool,
        aborted_reason: Option<String>,
    },
}
```

The worker writes `WorkerFrame::MonitorStarted` first, then any
`MonitorLines`, then `MonitorEnded`, then a final
`Response` (with `stdout = ""`, `is_error = false`, `exit_status
= 0` — the "tool call itself succeeded" frame; the child's exit
code is in `MonitorEnded`).

For a `monitor.list` or `monitor.stop` (synchronous ops with no
streaming), the worker writes a single `Response` and closes,
identical to today's behavior.

### Wire-protocol back-compat

Existing tools (bash, read, …) emit exactly one `Response` frame
per call, identical to today's bytes-on-the-wire. The `kind:
"Response"` tag is the only change, and `serde`'s tagged enum
serializes ToolResponse as `{"kind":"Response","call_id":...}`.
This IS a breaking wire change.

To stage rollout:

1. Bump `pi_sandbox_protocol::CURRENT_PROTOCOL_VERSION` from 1 to 2.
2. The host's `framing::read_response` becomes
   `framing::read_worker_frame`, returning `WorkerFrame`. The
   existing `read_response` shim is kept as a back-compat
   helper: it reads frames in a loop, discards
   `MonitorStarted/Lines/Ended` (logs them as "stream from
   non-monitor-aware host"), and returns when it sees `Response`.
3. Worker version emits `WorkerFrame::Response(...)` always for
   non-monitor calls.
4. Cold-boot rejects any guest reporting `proto_version != 2` —
   old workers don't talk to new hosts. Per-VM CID = per-version
   pool partition (already true via `rootfs_version` in the pool
   key — bump rootfs version when bumping protocol).

### Host-side bridge

`crates/pi-sandbox/src/microvm/firecracker.rs::FirecrackerVmHandle::execute`
reads `WorkerFrame`s in a loop:

```rust
async fn execute(...) -> Result<VmExecution, SandboxError> {
    let mut stream = vsock_connect(...);
    framing::write_request(...).await?;

    loop {
        match framing::read_worker_frame(&mut reader, frame_cap).await? {
            WorkerFrame::MonitorStarted { call_id, monitor_id } => {
                self.monitor_sender.send(MonitorStartedEvent { ... })?;
            }
            WorkerFrame::MonitorLines { lines, .. } => {
                self.monitor_sender.send(MonitorNotification::Lines { ... })?;
            }
            WorkerFrame::MonitorEnded { .. } => {
                self.monitor_sender.send(MonitorNotification::Ended { ... })?;
            }
            WorkerFrame::Response(resp) => {
                return Ok(VmExecution { result: resp.into(), ... });
            }
        }
    }
}
```

`monitor_sender: MonitorSender` is the SAME channel the runtime
already feeds via `MonitorTool::new(sender)` in local-process
mode. The sandbox provider gets a clone at construction time —
the embedder injects it the same way it'll inject the
`LspProxyHandler` for RFD 0030.

### Guest-worker `monitor` arm

Mirror the existing `web_search` proxy arm. When the worker sees
`req.tool_name == "monitor"`:

1. Look up the op (`start` | `stop` | `list`) from `tool_input`.
2. For `start`: fork the child via existing `MonitorTool` machinery,
   but instead of calling `self.sender.send(...)`, write
   `WorkerFrame::MonitorLines` / `MonitorEnded` directly to the
   open vsock connection.
3. For `stop`/`list`: synchronous, emit a single
   `WorkerFrame::Response(...)` and close.

State (the `HashMap<String, MonitorHandle>`) lives in the worker
process for the VM's lifetime. `monitor.list` queries that state,
`monitor.stop` cancels via the oneshot.

### Per-call hygiene + monitor lifetime

RFD 0023's per-call hygiene wipes `/tmp /var/tmp /root` between
calls. The worker spawns the monitor's child via `tokio::process::
Command`, so the child is the worker's process-group descendant.

**Conflict:** if call 1 starts a monitor (long-running `tail -f`),
the call returns its first burst of `MonitorLines` and then the
agent issues call 2. Today, `pre_call_hygiene` runs at the top of
`dispatch_request` for call 2 and wipes `/tmp`. The
`tail -f`-style child might be writing to `/tmp/foo` for call 1's
monitor — wipe makes no semantic sense.

**Resolution:**
- Monitor children are NOT killed by per-call hygiene. The
  hygiene only wipes filesystem paths; processes survive.
- The child's reads of `/tmp/foo` (if any) ARE invalidated — that
  file is gone. This matches the `local-process` semantic where
  monitor processes don't live in any particular fs context.
  Documented as: "monitor's child should not depend on the
  agent's scratch paths; use `/opt` or absolute paths to
  long-lived files."

### `PI_SANDBOX_FC_MAX_CALLS=1` interaction

When the operator sets full-reset mode, the VM is destroyed at
release time. Any monitor child running in that VM dies with the
VM (as part of the firecracker process tree teardown,
`kill_on_drop=true`).

The host bridge sees vsock EOF mid-stream. The runtime emits a
synthetic `MonitorNotification::Ended { exit_code: None,
cancelled: true, aborted_reason: Some("vm reset") }` for every
`monitor_id` known to be live in that VM, so the agent gets a
clean termination signal.

### Threat model deltas

The vsock-5001 channel is the same one we already have. No new
port. The wire-format extension introduces a new attack surface:
a malicious guest worker could flood the host with
`MonitorLines` to OOM the agent's UI. Mitigations:

- **Volume cap**: existing `MonitorConfig::volume_cap_lines = 100
  per 5 s` already in `pi-tools-core`. Honored on the worker
  side. If the cap fires, the worker emits one final
  `MonitorEnded { aborted_reason: Some("volume cap") }` and stops
  shipping frames.
- **Per-VM concurrent monitor cap**: `MonitorConfig::max_concurrent
  = 8` already enforced. Worker rejects a 9th `start` call.
- **Frame size cap**: `framing::DEFAULT_MAX_LINE_BYTES = 1 MiB`
  on the wire. A single `MonitorLines.lines` field can carry up
  to ~256 KiB of batched output (matches `CallLimits::max_output_bytes`).

Net new risk: small.

### Tool-disposition unwiring

When the host bridge is wired (sandbox provider has a
`MonitorSender` injected), the runtime overrides
`MonitorTool::dispatch()` to return `Guest`. When not wired
(provider was constructed with `MonitorSender = None`),
`Unavailable` stays. Same shape as RFD 0030's LSP enablement.

## Implementation plan

| Commit  | Description                                                                                        | Est LoC |
|---------|----------------------------------------------------------------------------------------------------|---------|
| `31-A`  | `pi-sandbox-protocol`: introduce `WorkerFrame` enum + version-bump to 2 + back-compat shim. Update both sides of `framing`. | 200 |
| `31-B`  | Worker: `monitor` arm in dispatch.rs, writing streaming frames directly to the vsock connection.   | 250     |
| `31-C`  | Host bridge: `FirecrackerVmHandle::execute` loops reading WorkerFrames, dispatches to `MonitorSender`. Embedder wires the sender into `MicroVmProvider::with_monitor_sender(...)`. | 250 |
| `31-D`  | Tool-disposition: `MonitorTool::dispatch()` returns `Guest` when `MonitorSender` is wired; default `Unavailable` otherwise. Mirror the LSP pattern from RFD 0030.                | 80      |
| `31-E`  | Per-VM monitor state: extend the existing `MonitorTool::handles` HashMap so `list`/`stop` work across multiple `start` calls in one VM lifetime.                                | 150     |
| `31-F`  | Reset interaction: when the host detects vsock EOF mid-stream, it synthesises `MonitorEnded { aborted_reason: Some("vm reset") }` for every `monitor_id` known to be live on that VM. Test: start a monitor, set MAX_CALLS=1, observe the synthetic Ended. | 150 |
| `31-G`  | Integration test (gated `PI_SANDBOX_FC_MONITOR_TEST=1`): agent calls `monitor.start { command: "for i in 1 2 3; do echo $i; sleep 0.1; done", description: "test" }`, asserts the host receives 3 (or fewer batched) `MonitorLines` frames + 1 `MonitorEnded { exit_code: 0 }`, in order. | 250 |
| `31-H`  | Documentation: NETWORKING.md gains a "monitor proxy" section; RFD 0023's tool-disposition matrix updates `monitor` from `unavailable` to `guest (when MonitorSender wired)`. RFD 0017 gets a forward reference to this RFD.                          | 80      |

**Total: ~1410 LoC across 8 commits.**

## Out of scope / deferred

- **`monitor` across VM rotations**: when MAX_CALLS=50 and the
  VM retires mid-monitor, the monitor dies. We could persist
  the monitor by re-spawning the child in the next VM. Not
  worth it — agents that want long-lived monitors should use
  `--sandbox-provider=local-process` or accept the rotation
  signal. Document.
- **Streaming for OTHER tools** (e.g. `bash` long-running
  build with progress). The wire-format machinery is generic —
  any tool could emit `WorkerFrame::Lines`-style frames. v1
  scopes to `monitor`. v2 considers `bash` streaming if
  there's user demand.
- **Per-monitor resource limits beyond what RFD 0017 has**.
  Today RFD 0017 has volume cap + concurrent cap + duration
  cap. v1 of the sandbox dispatch reuses those exactly. New
  limits live in RFD 0017's evolution path.

## Open questions

1. **Where does the host-side monitor state machine live?** Two
   choices: extend `pi-coding-agent`'s existing monitor pump (it
   already feeds `MonitorSender` from local-process), or create
   a sandbox-side adapter. The runtime already builds the
   `MonitorSender` once. Most natural: sandbox provider owns the
   `Arc<MonitorSender>` clone and ferries; pump is unchanged.

2. **Do we need to translate the `command` field in `monitor.start`?**
   The command runs inside the guest. If the agent says
   `monitor.start { command: "tail -f /work/foo.log", … }`, the
   `/work` path is guest-relative. With contextfs not yet
   integrated (G3), `/work` doesn't exist. So either
   `monitor.start` rejects `/work/`-prefixed paths until G3, or
   it works on guest-private paths only (`/tmp`, `/opt`).
   Probably the latter for v1 with a clear error if `/work/`
   appears.

3. **Backpressure**: if the host-side `MonitorSender` channel
   fills (consumer is slow), what should the worker do? Block?
   Drop? Bound the queue and emit `aborted_reason:
   "channel_overflow"`? Default: bound at `volume_cap_lines`,
   drop with a single emitted "lines dropped: N" notification.
   Same as the RFD 0017 in-memory backpressure.

## Revision history

- **v0.1 (2026-05-06):** Initial draft. Picks streaming-frame
  protocol over per-monitor vsock channel (one channel per call,
  multiple frame types over it) for protocol simplicity.
