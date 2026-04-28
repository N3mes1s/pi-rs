//! `monitor` tool — RFD 0017.
//!
//! Streams stdout from a long-lived background command back to the
//! agent as one notification per line (with 200 ms batching).
//!
//! `op = "start"` forks a `tokio::process::Command`, attaches a reader
//! task, and stores a [`MonitorHandle`] for later `stop`. Each line
//! batch is emitted as a [`MonitorNotification::Lines`] on the
//! channel handed to the tool at construction time. When the child
//! exits (or `stop` cancels it, or the volume guard trips) the tool
//! sends a single [`MonitorNotification::Ended`].

use async_trait::async_trait;
use pi_ai::{ToolResult, ToolSpec};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::oneshot;

use crate::{resolve_path, Tool, ToolContext, ToolError};

/// Notification emitted by a [`MonitorTool`]. The tool itself doesn't
/// know about `pi_agent_core::AgentEventKind`; the host (typically
/// `pi-coding-agent`) bridges these notifications onto the event
/// channel and into the [`MonitorPump`].
#[derive(Debug, Clone)]
pub enum MonitorNotification {
    Lines {
        monitor_id: String,
        description: String,
        lines: String,
    },
    Ended {
        monitor_id: String,
        description: String,
        exit_code: Option<i32>,
        cancelled: bool,
        aborted_reason: Option<String>,
    },
}

pub type MonitorSender = tokio::sync::mpsc::UnboundedSender<MonitorNotification>;
pub type MonitorReceiver = tokio::sync::mpsc::UnboundedReceiver<MonitorNotification>;

/// Per-monitor configuration. Values come from the agent's tool input
/// or fall back to [`MonitorConfig::default`].
#[derive(Debug, Clone)]
pub struct MonitorConfig {
    pub max_concurrent: usize,
    pub batch_window: Duration,
    pub volume_cap_lines: usize,
    pub volume_cap_window: Duration,
    pub default_timeout: Duration,
    pub max_timeout: Duration,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 8,
            batch_window: Duration::from_millis(200),
            volume_cap_lines: 100,
            volume_cap_window: Duration::from_secs(5),
            default_timeout: Duration::from_millis(300_000),
            max_timeout: Duration::from_millis(3_600_000),
        }
    }
}

struct MonitorHandle {
    id: String,
    command: String,
    description: String,
    pid: u32,
    started_at: Instant,
    persistent: bool,
    /// Drop-on-stop: signals the reader task to kill the child.
    cancel: Option<oneshot::Sender<()>>,
}

/// `monitor` tool. Construct via [`MonitorTool::new`] with the channel
/// the host wants notifications delivered on.
pub struct MonitorTool {
    sender: MonitorSender,
    config: MonitorConfig,
    handles: Arc<Mutex<HashMap<String, MonitorHandle>>>,
}

impl MonitorTool {
    pub fn new(sender: MonitorSender, config: MonitorConfig) -> Self {
        Self {
            sender,
            config,
            handles: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Map exposing the live handle table; the host drains this on
    /// session drop to kill children.
    #[allow(dead_code)]
    #[allow(private_interfaces)]
    pub(crate) fn handles_internal(&self) -> Arc<Mutex<HashMap<String, MonitorHandle>>> {
        self.handles.clone()
    }

    /// Number of currently-active monitors (for diagnostics/tests).
    pub fn active_count(&self) -> usize {
        self.handles.lock().unwrap().len()
    }

    /// PIDs of currently-active monitors (for diagnostics/tests).
    pub fn active_pids(&self) -> Vec<u32> {
        self.handles.lock().unwrap().values().map(|h| h.pid).collect()
    }

    /// Stop every active monitor. Used on session drop.
    pub fn stop_all(&self) {
        let ids: Vec<String> = {
            let g = self.handles.lock().unwrap();
            g.keys().cloned().collect()
        };
        for id in ids {
            self.stop_one(&id);
        }
    }

    fn stop_one(&self, id: &str) -> bool {
        let mut g = self.handles.lock().unwrap();
        if let Some(mut h) = g.remove(id) {
            if let Some(tx) = h.cancel.take() {
                let _ = tx.send(());
            }
            // Also send a SIGTERM to the pid as a belt-and-braces
            // measure (the reader task does this too on cancel, but
            // the reader may already have exited).
            #[cfg(unix)]
            {
                use std::os::raw::c_int;
                extern "C" {
                    fn kill(pid: i32, sig: c_int) -> c_int;
                }
                unsafe {
                    kill(h.pid as i32, 15 /* SIGTERM */);
                }
            }
            true
        } else {
            false
        }
    }
}

#[async_trait]
impl Tool for MonitorTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "monitor".into(),
            description:
                "Run a long-lived background command whose stdout streams \
                 back as one notification per line (batched within ~200 ms). \
                 Use for tail -f, dev servers, poll loops over CI / PR state, \
                 anywhere you'd want `bash` if it didn't block. Pipe through \
                 `grep --line-buffered` to filter — every stdout line becomes a \
                 chat message. Set `persistent: true` for session-length watches \
                 that exit only on stop. Ends naturally when the script exits. \
                 Best practices: merge stderr with `2>&1` for cargo/test output; \
                 widen filters to cover failure signals (silence on a crash looks \
                 like silence on success); prefer `until <cond>; do sleep 2; done` \
                 when you only need a one-shot wait. ops: start | stop | list."
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "op": {
                        "type": "string",
                        "enum": ["start", "stop", "list"],
                        "description": "start a new watcher, stop an existing one (by id), or list active ones."
                    },
                    "command": {"type": "string", "description": "Shell command. Required for `start`."},
                    "description": {"type": "string", "description": "Short label shown in event notifications. Required for `start`."},
                    "persistent": {"type": "boolean", "default": false, "description": "Run for the lifetime of the session. Default: bounded by `timeout_ms`."},
                    "timeout_ms": {"type": "integer", "default": 300000, "description": "Auto-stop deadline when not persistent. Default 5 min, max 1 h."},
                    "id": {"type": "string", "description": "Required for `stop`. Returned from a prior `start`."}
                },
                "required": ["op"]
            }),
        }
    }

    fn read_only(&self) -> bool {
        false
    }

    async fn invoke(
        &self,
        ctx: &ToolContext,
        call_id: &str,
        input: Value,
    ) -> Result<ToolResult, ToolError> {
        let op = input
            .get("op")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing `op`".into()))?;
        match op {
            "start" => self.op_start(ctx, call_id, input).await,
            "stop" => self.op_stop(call_id, input).await,
            "list" => self.op_list(call_id).await,
            other => Err(ToolError::InvalidInput(format!(
                "unknown op `{other}` (expected start | stop | list)"
            ))),
        }
    }
}

impl MonitorTool {
    async fn op_start(
        &self,
        ctx: &ToolContext,
        call_id: &str,
        input: Value,
    ) -> Result<ToolResult, ToolError> {
        let command = input
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing `command`".into()))?
            .to_string();
        let description = input
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("monitor")
            .to_string();
        let persistent = input
            .get("persistent")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let raw_timeout = input
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(self.config.default_timeout.as_millis() as u64);
        let timeout = Duration::from_millis(raw_timeout.min(self.config.max_timeout.as_millis() as u64));

        let cwd = match input.get("cwd").and_then(|v| v.as_str()) {
            Some(p) => resolve_path(ctx, p),
            None => ctx.cwd.clone(),
        };

        // Concurrent-monitor cap.
        {
            let g = self.handles.lock().unwrap();
            if g.len() >= self.config.max_concurrent {
                return Err(ToolError::Other(format!(
                    "monitor cap reached ({} active). Call op:list and op:stop one before starting another.",
                    self.config.max_concurrent
                )));
            }
        }

        let id = format!("mon_{}", uuid_like());
        let mut cmd = Command::new("bash");
        cmd.arg("-lc")
            .arg(&command)
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = cmd.spawn().map_err(ToolError::Io)?;
        let pid = child
            .id()
            .ok_or_else(|| ToolError::Other("spawn returned no pid".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ToolError::Other("no stdout pipe".into()))?;
        let stderr = child.stderr.take();

        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();

        let handle = MonitorHandle {
            id: id.clone(),
            command: command.clone(),
            description: description.clone(),
            pid,
            started_at: Instant::now(),
            persistent,
            cancel: Some(cancel_tx),
        };
        let started_at_ms = chrono::Utc::now().timestamp_millis();
        self.handles.lock().unwrap().insert(id.clone(), handle);

        let sender = self.sender.clone();
        let handles = self.handles.clone();
        let cfg = self.config.clone();
        let id_clone = id.clone();
        let desc_clone = description.clone();

        tokio::spawn(reader_task(
            id_clone,
            desc_clone,
            child,
            stdout,
            stderr,
            cancel_rx,
            sender,
            handles,
            cfg,
            persistent,
            timeout,
        ));

        Ok(ToolResult {
            tool_use_id: call_id.into(),
            model_output: format!(
                "monitor `{}` started (pid {}). description: {}",
                id, pid, description
            ),
            display: Some(json!({
                "kind": "monitor",
                "op": "start",
                "monitor": {
                    "id": id,
                    "pid": pid,
                    "command": command,
                    "description": description,
                    "persistent": persistent,
                    "started_at": started_at_ms,
                }
            })),
            is_error: false,
        })
    }

    async fn op_stop(&self, call_id: &str, input: Value) -> Result<ToolResult, ToolError> {
        let id = input
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing `id`".into()))?;
        let stopped = self.stop_one(id);
        Ok(ToolResult {
            tool_use_id: call_id.into(),
            model_output: if stopped {
                format!("monitor `{}` stopped", id)
            } else {
                format!("monitor `{}` not found (already exited?)", id)
            },
            display: Some(json!({
                "kind": "monitor",
                "op": "stop",
                "id": id,
                "stopped": stopped,
            })),
            is_error: false,
        })
    }

    async fn op_list(&self, call_id: &str) -> Result<ToolResult, ToolError> {
        let g = self.handles.lock().unwrap();
        let monitors: Vec<Value> = g
            .values()
            .map(|h| {
                json!({
                    "id": h.id,
                    "command": h.command,
                    "description": h.description,
                    "pid": h.pid,
                    "elapsed_ms": h.started_at.elapsed().as_millis() as u64,
                    "persistent": h.persistent,
                })
            })
            .collect();
        let count = monitors.len();
        Ok(ToolResult {
            tool_use_id: call_id.into(),
            model_output: format!("{} active monitor(s)", count),
            display: Some(json!({
                "kind": "monitor",
                "op": "list",
                "monitors": monitors,
            })),
            is_error: false,
        })
    }
}

/// One reader task per monitor: pumps stdout (and stderr) into batched
/// notifications, handles timeout / volume guard / cancel.
#[allow(clippy::too_many_arguments)]
async fn reader_task(
    id: String,
    description: String,
    mut child: tokio::process::Child,
    stdout: tokio::process::ChildStdout,
    stderr: Option<tokio::process::ChildStderr>,
    cancel: oneshot::Receiver<()>,
    sender: MonitorSender,
    handles: Arc<Mutex<HashMap<String, MonitorHandle>>>,
    cfg: MonitorConfig,
    persistent: bool,
    timeout: Duration,
) {
    let (line_tx, mut line_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

    // stdout reader
    let lt = line_tx.clone();
    let stdout_jh = tokio::spawn(async move {
        let mut br = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = br.next_line().await {
            if lt.send(line).is_err() {
                break;
            }
        }
    });

    // stderr reader (also surfaced as monitor lines, prefixed)
    let stderr_jh = if let Some(stderr) = stderr {
        let lt = line_tx.clone();
        Some(tokio::spawn(async move {
            let mut br = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = br.next_line().await {
                if lt.send(line).is_err() {
                    break;
                }
            }
        }))
    } else {
        None
    };

    drop(line_tx);

    let mut pending: Vec<String> = Vec::new();
    let mut window_deadline: Option<tokio::time::Instant> = None;
    let mut volume_window: VecDeque<Instant> = VecDeque::new();

    let mut cancel = cancel;
    let mut cancelled = false;
    let mut aborted_reason: Option<String> = None;
    let mut exit_code: Option<i32> = None;

    let started = Instant::now();

    loop {
        let sleep_dur = if let Some(d) = window_deadline {
            let now = tokio::time::Instant::now();
            if d > now { d - now } else { Duration::from_millis(0) }
        } else {
            Duration::from_secs(3600)
        };

        let timeout_remaining = if persistent {
            Duration::from_secs(86_400 * 365)
        } else {
            timeout.saturating_sub(started.elapsed())
        };

        tokio::select! {
            biased;
            _ = &mut cancel => {
                cancelled = true;
                break;
            }
            _ = tokio::time::sleep(timeout_remaining), if !persistent => {
                cancelled = true;
                aborted_reason = Some("timeout".into());
                break;
            }
            line = line_rx.recv() => {
                match line {
                    Some(l) => {
                        let now = Instant::now();
                        volume_window.push_back(now);
                        let cutoff = now - cfg.volume_cap_window;
                        while let Some(&front) = volume_window.front() {
                            if front < cutoff {
                                volume_window.pop_front();
                            } else {
                                break;
                            }
                        }
                        if volume_window.len() > cfg.volume_cap_lines {
                            // Volume guard: too noisy.
                            pending.push(l);
                            cancelled = true;
                            aborted_reason = Some("volume_cap".into());
                            // Flush before bailing.
                            flush_pending(&mut pending, &id, &description, &sender);
                            break;
                        }
                        pending.push(l);
                        if window_deadline.is_none() {
                            window_deadline = Some(tokio::time::Instant::now() + cfg.batch_window);
                        }
                    }
                    None => {
                        // Both readers closed — child exiting.
                        flush_pending(&mut pending, &id, &description, &sender);
                        // Wait for child status.
                        if let Ok(status) = child.wait().await {
                            exit_code = status.code();
                        }
                        break;
                    }
                }
            }
            _ = tokio::time::sleep(sleep_dur), if window_deadline.is_some() => {
                window_deadline = None;
                flush_pending(&mut pending, &id, &description, &sender);
            }
        }
    }

    // Cancellation path: kill the child, drain.
    if cancelled {
        flush_pending(&mut pending, &id, &description, &sender);
        let _ = child.start_kill();
        // brief grace period
        let _ = tokio::time::timeout(Duration::from_millis(200), child.wait()).await;
        if let Ok(Some(status)) = child.try_wait() {
            exit_code = status.code();
        }
    }

    // Best effort drain remaining buffered lines.
    while let Ok(Some(l)) = tokio::time::timeout(Duration::from_millis(20), line_rx.recv()).await {
        pending.push(l);
    }
    flush_pending(&mut pending, &id, &description, &sender);

    let _ = stdout_jh.await;
    if let Some(j) = stderr_jh {
        let _ = j.await;
    }

    handles.lock().unwrap().remove(&id);
    let _ = sender.send(MonitorNotification::Ended {
        monitor_id: id,
        description,
        exit_code,
        cancelled,
        aborted_reason,
    });
}

fn flush_pending(
    pending: &mut Vec<String>,
    id: &str,
    description: &str,
    sender: &MonitorSender,
) {
    if pending.is_empty() {
        return;
    }
    let lines = std::mem::take(pending).join("\n");
    let _ = sender.send(MonitorNotification::Lines {
        monitor_id: id.to_string(),
        description: description.to_string(),
        lines,
    });
}

fn uuid_like() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let ts = chrono::Utc::now().timestamp_millis();
    format!("{:x}{:04x}", ts, seq & 0xffff)
}
