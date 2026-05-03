//! Per RFD 0028 §D.3: generic spawn primitive for halo cycle
//! subprocesses (orchestrate today, compiled-agent in §D.7).
//!
//! Matches halo's existing sync style (std::process::Command,
//! background reader threads, polling wait loop) — verified
//! against `step_orchestrate` at cycle.rs:655-687, which is the
//! pattern this primitive generalises.
//!
//! The two HARD invariants from §D.3:
//! 1. `process_group(0)` before exec, so halo's existing SIGINT
//!    handler can `killpg(child_pgid, SIGINT)` via the
//!    `pid_shared: Arc<AtomicI32>` rendezvous (matches
//!    `cycle.rs:673` + `:682-684`).
//! 2. Halo's process env is inherited by the child unless
//!    `env_extra` overrides specific keys. This is THE secrets
//!    surface — e.g., `ANTHROPIC_API_KEY` flows from halo's
//!    own env into the compiled agent's `from_env_explicit`
//!    via plain inheritance.

use std::collections::BTreeMap;
use std::io::{Read, Write as _};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use thiserror::Error;

use pi_sdk::AgentEvent;

use crate::halo::jsonl::parse_event_line;

/// Operator-specified bits + halo plumbing for spawning a single
/// cycle subprocess. Two cycle shapes use this:
/// - `step_orchestrate` (existing; refactor to use this is a
///   follow-up commit per RFD §D.9).
/// - compiled-agent cycles (the §D consumer).
#[derive(Debug)]
pub struct CycleSubprocessCommand<'a> {
    pub name: &'a str,
    pub binary: &'a Path,
    pub args: &'a [String],
    /// Piped to the child's stdin. Compiled agents read this via
    /// `read_prompt_from_args_or_stdin` per RFD 0028 §B.11.
    pub prompt: &'a str,
    /// Working directory for the child (typically the halo-owned
    /// clone per RFD 0025 §259).
    pub cwd: &'a Path,
    /// Additional env vars to set on the child beyond what halo
    /// inherits from its own process env. Halo inherits ITS env
    /// by default; this is purely additive (NOT the secrets
    /// channel — see RFD §D.3).
    pub env_extra: &'a BTreeMap<String, String>,
    /// Wall-clock cap. `None` = no cap (operator's halo.toml
    /// `timeout_secs = 0` lowers to None).
    pub timeout: Option<Duration>,
    /// Halo's existing SIGINT-propagation rendezvous: subprocess
    /// publishes the child PID here; halo's signal handler reads
    /// it and `killpg`s the PG when it gets SIGINT/SIGTERM.
    /// (Same contract as `CycleCtx.orchestrate_pid_shared`.)
    pub pid_shared: Arc<AtomicI32>,
    /// Halo's signal flag — set by the run.rs handler. `spawn`
    /// polls this every 500 ms and SIGTERMs the child on detect.
    pub signal_received: Arc<AtomicBool>,
    /// When `true`, inherit stdout + stderr from the parent so
    /// child diagnostics flow to the operator's terminal directly
    /// (the orchestrate-cycle shape — cargo's compile output is
    /// what the operator wants to see live). When `false` (the
    /// default for compiled-agent cycles), pipe stdout for JSONL
    /// parsing and ring-buffer stderr at 16 KiB.
    ///
    /// Phase 2d added this so `step_orchestrate` could share the
    /// primitive without losing operator visibility into cargo
    /// runs. With `inherit_stdio = true`, the resulting
    /// `CycleSubprocessOutcome.events` is always empty and
    /// `stderr_tail` is always "" — the parent's stdio handles
    /// own the bytes.
    pub inherit_stdio: bool,
}

/// Result of spawning a cycle subprocess.
#[derive(Debug)]
pub struct CycleSubprocessOutcome {
    /// `-1` if the child was killed by signal or could not be waited.
    pub exit_code: i32,
    /// Parsed `AgentEvent` JSONL stream from stdout (skipping any
    /// lines that fail to deserialize per `parse_event_line`).
    pub events: Vec<AgentEvent>,
    /// Last STDERR_TAIL_BYTES of stderr (for cycle-log diagnostics).
    /// May contain non-UTF8 bytes replaced with U+FFFD.
    pub stderr_tail: String,
    pub wall_time: Duration,
    /// True if the cycle was terminated by `signal_received` going
    /// high (operator pressed ^C).
    pub signaled: bool,
    /// True if the cycle was terminated by `timeout` elapsing.
    pub timed_out: bool,
}

#[derive(Debug, Error)]
pub enum SubprocessError {
    #[error("could not spawn {bin}: {source}")]
    Spawn {
        bin: String,
        #[source]
        source: std::io::Error,
    },
    #[error("could not write prompt to child stdin: {0}")]
    StdinWrite(std::io::Error),
    #[error("could not wait on child: {0}")]
    Wait(std::io::Error),
}

/// Last N bytes of stderr to retain. Anything beyond this is
/// dropped (stderr_tail in CycleSubprocessOutcome holds the
/// most recent slice).
const STDERR_TAIL_BYTES: usize = 16 * 1024;
/// Per-line cap for stdout JSONL. A misbehaving compiled agent
/// emitting one giant line can no longer OOM halo: anything
/// past this byte count is dropped + warn-logged. 1 MiB
/// comfortably exceeds any legitimate AgentEvent (largest
/// realistic variant is `MonitorEvent { lines: String }` and a
/// 1 MiB monitor batch is already a misuse).
const MAX_JSONL_LINE_BYTES: usize = 1024 * 1024;
/// Stdout read chunk size — small enough that backpressure
/// stays smooth, large enough to amortize syscall overhead.
const STDOUT_CHUNK_BYTES: usize = 4 * 1024;
/// Polling interval for the wait loop.
const WAIT_POLL: Duration = Duration::from_millis(500);
/// Grace period between SIGTERM and SIGKILL on timeout/signal.
const KILL_GRACE: Duration = Duration::from_secs(2);

/// Spawn a cycle subprocess per `cmd`, drain stdout as JSONL,
/// cap stderr at the last 16 KiB, honor signal + timeout.
///
/// Returns the structured outcome on normal exit, signal-driven
/// termination, OR timeout. Spawn-time failures (cargo not
/// installed, EACCES, etc.) come back as `Err(SubprocessError)`.
pub fn spawn_cycle_subprocess(
    cmd: &CycleSubprocessCommand<'_>,
) -> Result<CycleSubprocessOutcome, SubprocessError> {
    let started = Instant::now();

    let mut command = build_command(cmd);
    command.stdin(Stdio::piped());
    if cmd.inherit_stdio {
        // Orchestrate-cycle shape: cargo's diagnostics go straight
        // to the operator's terminal. No JSONL parsing, no stderr
        // ring-buffering.
        command.stdout(Stdio::inherit()).stderr(Stdio::inherit());
    } else {
        // Compiled-agent shape: pipe stdout for JSONL parsing,
        // pipe stderr for the 16 KiB ring-buffer.
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
    }
    let mut child = command.spawn().map_err(|e| SubprocessError::Spawn {
        bin: cmd.binary.display().to_string(),
        source: e,
    })?;

    let child_pid = child.id() as i32;
    cmd.pid_shared.store(child_pid, Ordering::SeqCst);

    // Stdin: write the prompt + close.
    let mut stdin = child.stdin.take().expect("piped stdin");
    let prompt = cmd.prompt.to_owned();
    let stdin_handle = thread::spawn(move || -> std::io::Result<()> {
        stdin.write_all(prompt.as_bytes())?;
        // Implicit drop closes the pipe — child sees EOF on stdin.
        Ok(())
    });

    // Stdout + stderr handling depends on inherit_stdio.
    // - inherit_stdio = true (orchestrate shape): no reader threads
    //   spawned; child writes directly to operator's terminal.
    //   events_buf + stderr_tail stay empty.
    // - inherit_stdio = false (compiled-agent shape): byte-level
    //   chunked stdout reader + 16 KiB ring-buffered stderr.
    //
    // Stdout reader fixes from prior reviewer passes (only
    // applied when inherit_stdio = false):
    //  - `BufRead::lines()` returned `Err(InvalidData)` on any
    //    non-UTF-8 byte and the previous `let Ok(line) = line
    //    else { break };` silently dropped every subsequent line.
    //    Now we read raw bytes and `from_utf8_lossy` each line.
    //  - `BufRead::lines()` could allocate a 1 GiB `String` on a
    //    runaway child. MAX_JSONL_LINE_BYTES bounds in-flight memory.
    let events_buf = Arc::new(Mutex::new(Vec::<AgentEvent>::new()));
    let stderr_tail = Arc::new(Mutex::new(Vec::<u8>::with_capacity(STDERR_TAIL_BYTES)));

    let stdout_handle = if cmd.inherit_stdio {
        None
    } else {
        let mut stdout = child.stdout.take().expect("piped stdout");
        let events_writer = events_buf.clone();
        Some(thread::spawn(move || {
            let mut chunk = [0u8; STDOUT_CHUNK_BYTES];
            let mut line_buf: Vec<u8> = Vec::with_capacity(STDOUT_CHUNK_BYTES);
            let mut overflowed = false;
            loop {
                match stdout.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        for &b in &chunk[..n] {
                            if b == b'\n' {
                                if !overflowed {
                                    let s = String::from_utf8_lossy(&line_buf);
                                    if let Some(evt) = parse_event_line(&s) {
                                        let mut guard = events_writer
                                            .lock()
                                            .expect("events mutex poisoned");
                                        guard.push(evt);
                                    }
                                }
                                line_buf.clear();
                                overflowed = false;
                                continue;
                            }
                            if line_buf.len() < MAX_JSONL_LINE_BYTES {
                                line_buf.push(b);
                            } else if !overflowed {
                                tracing::warn!(
                                    cap = MAX_JSONL_LINE_BYTES,
                                    "compiled-agent JSONL line exceeded {} bytes; dropping until next newline",
                                    MAX_JSONL_LINE_BYTES
                                );
                                overflowed = true;
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            if !overflowed && !line_buf.is_empty() {
                let s = String::from_utf8_lossy(&line_buf);
                if let Some(evt) = parse_event_line(&s) {
                    let mut guard = events_writer.lock().expect("events mutex poisoned");
                    guard.push(evt);
                }
            }
        }))
    };

    let stderr_handle = if cmd.inherit_stdio {
        None
    } else {
        let mut stderr = child.stderr.take().expect("piped stderr");
        let stderr_writer = stderr_tail.clone();
        Some(thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            loop {
                match stderr.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut guard = stderr_writer.lock().expect("stderr mutex poisoned");
                        let total = guard.len() + n;
                        if total > STDERR_TAIL_BYTES {
                            let drop = total - STDERR_TAIL_BYTES;
                            let drop = drop.min(guard.len());
                            guard.drain(..drop);
                        }
                        guard.extend_from_slice(&chunk[..n]);
                    }
                    Err(_) => break,
                }
            }
        }))
    };

    // Wait loop: poll for exit, signal, or timeout.
    let mut signaled = false;
    let mut timed_out = false;
    let exit_status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {}
            Err(e) => return Err(SubprocessError::Wait(e)),
        }
        if cmd.signal_received.load(Ordering::SeqCst) {
            signaled = true;
            terminate_pid(child_pid);
            break wait_with_grace(&mut child);
        }
        if let Some(t) = cmd.timeout {
            if started.elapsed() >= t {
                timed_out = true;
                terminate_pid(child_pid);
                break wait_with_grace(&mut child);
            }
        }
        thread::sleep(WAIT_POLL);
    };

    // Drain readers + clear the shared-pid rendezvous.
    let _ = stdin_handle.join();
    if let Some(h) = stdout_handle { let _ = h.join(); }
    if let Some(h) = stderr_handle { let _ = h.join(); }
    cmd.pid_shared.store(0, Ordering::SeqCst);

    let exit_code = match exit_status {
        Some(s) => s.code().unwrap_or(-1),
        None => -1,
    };
    let events = events_buf.lock().expect("events mutex poisoned").clone();
    let stderr_bytes = stderr_tail
        .lock()
        .expect("stderr mutex poisoned")
        .clone();
    let stderr_tail = String::from_utf8_lossy(&stderr_bytes).into_owned();

    Ok(CycleSubprocessOutcome {
        exit_code,
        events,
        stderr_tail,
        wall_time: started.elapsed(),
        signaled,
        timed_out,
    })
}

/// Resolve `cmd.binary` per RFD §D.2 path-resolution rules + apply
/// cwd, args, env_extra, process_group(0), close-stdin-via-pipe.
fn build_command(cmd: &CycleSubprocessCommand<'_>) -> Command {
    // Path resolution is done by std::process::Command:
    // - absolute / relative-with-slash → used as-is.
    // - bare name (no `/`) → resolved via $PATH.
    // The operator-facing rule "relative-to-halo.toml-parent-dir"
    // is applied by the CALLER via
    // `CompiledAgentSpec::resolve_binary(halo_toml_parent)`.
    // Phase 2b's wiring code calls that method before constructing
    // the CycleSubprocessCommand. Direct callers (tests, future
    // shapes) pass pre-resolved absolute paths.
    let mut c = Command::new(cmd.binary);
    c.args(cmd.args);
    c.current_dir(cmd.cwd);
    // Halo inherits env by default — DO NOT call env_clear.
    // env_extra is purely additive.
    for (k, v) in cmd.env_extra {
        c.env(k, v);
    }
    // Per §D.3: MUST set process_group(0) so SIGINT can kill the
    // whole process tree via killpg. Same call shape that
    // `cycle.rs:673` uses for the existing orchestrate spawn.
    c.process_group(0);
    c
}

/// Send SIGTERM to the child's whole process group. Best-effort;
/// kernel-level failures are swallowed.
///
/// Negative PID semantics: `kill(-pid, sig)` targets the process
/// group with PGID = abs(pid). Since `build_command` set
/// `process_group(0)`, the child's PGID equals its PID — so
/// passing `-child_pid` reaches the shell + every descendent
/// (e.g., a `sleep` child of a `/bin/sh -c ...` wrapper).
/// Without the negative form, only the leader gets signaled and
/// orphans like `sleep` continue to completion.
fn terminate_pid(pid: i32) {
    // SAFETY: `kill(2)` is safe to call with any PID; it returns
    // an error rather than UB when the PID doesn't exist.
    unsafe {
        libc::kill(-pid, libc::SIGTERM);
    }
}

/// After SIGTERM, give the child KILL_GRACE to exit cleanly; then
/// SIGKILL if still alive. Returns the final ExitStatus or None.
fn wait_with_grace(child: &mut std::process::Child) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + KILL_GRACE;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(s)) => return Some(s),
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(_) => return None,
        }
    }
    let _ = child.kill();
    child.wait().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn empty_env() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    fn cmd_for<'a>(
        binary: &'a Path,
        cwd: &'a Path,
        env: &'a BTreeMap<String, String>,
        prompt: &'a str,
        args: &'a [String],
        signal: Arc<AtomicBool>,
        pid_shared: Arc<AtomicI32>,
        timeout: Option<Duration>,
    ) -> CycleSubprocessCommand<'a> {
        CycleSubprocessCommand {
            name: "test-cycle",
            binary,
            args,
            prompt,
            cwd,
            env_extra: env,
            timeout,
            pid_shared,
            signal_received: signal,
            // Default: pipe stdio so tests can assert events +
            // stderr_tail. The orchestrate-shape `inherit_stdio: true`
            // path is exercised by `inherit_stdio_skips_event_parsing`.
            inherit_stdio: false,
        }
    }

    #[test]
    fn spawn_captures_stdout_jsonl_and_stderr_tail() {
        let tmp = tempfile::tempdir().unwrap();
        // Mock binary that:
        // - reads stdin (proves prompt was piped),
        // - emits 2 lines of valid AgentEvent JSONL on stdout,
        // - writes a marker to stderr,
        // - exits 0.
        let bin = write_script(
            tmp.path(),
            "agent",
            r#"#!/bin/sh
read -r prompt
printf '%s\n' '{"session_id":"s","entry_id":"e1","timestamp":0,"kind":{"type":"session_started","id":"s","cwd":"/x","model":"m","provider":"p"}}'
printf '%s\n' '{"session_id":"s","entry_id":"e2","timestamp":0,"kind":{"type":"turn_complete"}}'
printf 'diagnostic line\n' >&2
exit 0
"#,
        );
        let env = empty_env();
        let signal = Arc::new(AtomicBool::new(false));
        let pid = Arc::new(AtomicI32::new(0));
        let args: Vec<String> = vec![];
        let outcome = spawn_cycle_subprocess(&cmd_for(
            &bin, tmp.path(), &env, "hello\n", &args, signal, pid, None,
        ))
        .expect("spawn ok");

        assert_eq!(outcome.exit_code, 0);
        assert_eq!(outcome.events.len(), 2, "should parse both JSONL lines");
        assert!(outcome.stderr_tail.contains("diagnostic line"));
        assert!(!outcome.signaled);
        assert!(!outcome.timed_out);
    }

    #[test]
    fn spawn_skips_malformed_jsonl_lines_keeps_reading() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = write_script(
            tmp.path(),
            "agent",
            r#"#!/bin/sh
printf '%s\n' '{"session_id":"s","entry_id":"e","timestamp":0,"kind":{"type":"session_started","id":"s","cwd":"/x","model":"m","provider":"p"}}'
printf '%s\n' 'this is not json'
printf '%s\n' '{"session_id":"s","entry_id":"e2","timestamp":0,"kind":{"type":"turn_complete"}}'
exit 0
"#,
        );
        let env = empty_env();
        let signal = Arc::new(AtomicBool::new(false));
        let pid = Arc::new(AtomicI32::new(0));
        let args: Vec<String> = vec![];
        let outcome = spawn_cycle_subprocess(&cmd_for(
            &bin, tmp.path(), &env, "", &args, signal, pid, None,
        ))
        .expect("spawn ok");
        // Malformed line dropped; valid lines kept.
        assert_eq!(outcome.events.len(), 2);
    }

    #[test]
    fn spawn_publishes_pid_into_pid_shared_during_run() {
        let tmp = tempfile::tempdir().unwrap();
        let pidfile = tmp.path().join("pid");
        let bin = write_script(
            tmp.path(),
            "agent",
            &format!(
                "#!/bin/sh\necho $$ > {}\nsleep 0.2\nexit 0\n",
                pidfile.display()
            ),
        );
        let env = empty_env();
        let signal = Arc::new(AtomicBool::new(false));
        let pid_shared = Arc::new(AtomicI32::new(0));
        let pid_observer = pid_shared.clone();
        let observed_during = std::sync::Arc::new(std::sync::Mutex::new(0i32));
        let observed_writer = observed_during.clone();

        let watcher = thread::spawn(move || {
            // Poll quickly until pid_shared transitions to non-zero.
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                let p = pid_observer.load(Ordering::SeqCst);
                if p != 0 {
                    *observed_writer.lock().unwrap() = p;
                    return;
                }
                thread::sleep(Duration::from_millis(10));
            }
        });
        let args: Vec<String> = vec![];
        let outcome = spawn_cycle_subprocess(&cmd_for(
            &bin, tmp.path(), &env, "", &args, signal, pid_shared.clone(), None,
        ))
        .expect("spawn ok");
        watcher.join().unwrap();
        assert_eq!(outcome.exit_code, 0);
        // pid_shared should have been non-zero during the run.
        let observed = *observed_during.lock().unwrap();
        assert!(observed > 0, "pid_shared was never observed non-zero");
        // And cleared back to 0 after.
        assert_eq!(pid_shared.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn spawn_inherits_env_and_env_extra_overrides() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = write_script(
            tmp.path(),
            "agent",
            r#"#!/bin/sh
printf 'INHERITED=%s\n' "$PI_BUILD_TEST_INHERITED" >&2
printf 'EXTRA=%s\n' "$PI_BUILD_TEST_EXTRA" >&2
exit 0
"#,
        );
        // SAFETY: single-threaded test setting + the var name is
        // unique enough to not collide with anything else.
        std::env::set_var("PI_BUILD_TEST_INHERITED", "from-halo");
        let mut extra = BTreeMap::new();
        extra.insert("PI_BUILD_TEST_EXTRA".into(), "from-extra".into());
        let signal = Arc::new(AtomicBool::new(false));
        let pid = Arc::new(AtomicI32::new(0));
        let args: Vec<String> = vec![];
        let outcome = spawn_cycle_subprocess(&cmd_for(
            &bin, tmp.path(), &extra, "", &args, signal, pid, None,
        ))
        .expect("spawn ok");
        std::env::remove_var("PI_BUILD_TEST_INHERITED");
        assert_eq!(outcome.exit_code, 0);
        assert!(outcome.stderr_tail.contains("INHERITED=from-halo"));
        assert!(outcome.stderr_tail.contains("EXTRA=from-extra"));
    }

    #[test]
    fn spawn_timeout_fires_and_terminates_child() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = write_script(
            tmp.path(),
            "agent",
            "#!/bin/sh\nsleep 30\nexit 0\n",
        );
        let env = empty_env();
        let signal = Arc::new(AtomicBool::new(false));
        let pid = Arc::new(AtomicI32::new(0));
        let args: Vec<String> = vec![];
        let started = Instant::now();
        let outcome = spawn_cycle_subprocess(&cmd_for(
            &bin,
            tmp.path(),
            &env,
            "",
            &args,
            signal,
            pid,
            Some(Duration::from_millis(800)),
        ))
        .expect("spawn ok");
        let elapsed = started.elapsed();
        assert!(outcome.timed_out, "outcome should report timeout");
        // Should NOT have run for 30 seconds.
        assert!(
            elapsed < Duration::from_secs(10),
            "timeout did not fire (elapsed {elapsed:?})",
        );
        // Child was killed → exit code is non-zero (signal exit
        // varies by system; just assert the run terminated quickly).
    }

    #[test]
    fn spawn_signal_received_terminates_child() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = write_script(
            tmp.path(),
            "agent",
            "#!/bin/sh\nsleep 30\nexit 0\n",
        );
        let env = empty_env();
        let signal = Arc::new(AtomicBool::new(false));
        let signal_setter = signal.clone();
        let pid = Arc::new(AtomicI32::new(0));
        // Trip the signal flag after 600 ms.
        let trigger = thread::spawn(move || {
            thread::sleep(Duration::from_millis(600));
            signal_setter.store(true, Ordering::SeqCst);
        });
        let args: Vec<String> = vec![];
        let outcome = spawn_cycle_subprocess(&cmd_for(
            &bin, tmp.path(), &env, "", &args, signal, pid, None,
        ))
        .expect("spawn ok");
        trigger.join().unwrap();
        assert!(outcome.signaled, "outcome should report signal");
        assert!(!outcome.timed_out);
    }

    #[test]
    fn spawn_caps_stderr_at_16kib() {
        let tmp = tempfile::tempdir().unwrap();
        // Emit 32 KiB of stderr; only the last 16 KiB should be retained.
        let bin = write_script(
            tmp.path(),
            "agent",
            r#"#!/bin/sh
yes "abcdefghijklmno" | head -c 32768 >&2
exit 0
"#,
        );
        let env = empty_env();
        let signal = Arc::new(AtomicBool::new(false));
        let pid = Arc::new(AtomicI32::new(0));
        let args: Vec<String> = vec![];
        let outcome = spawn_cycle_subprocess(&cmd_for(
            &bin, tmp.path(), &env, "", &args, signal, pid, None,
        ))
        .expect("spawn ok");
        assert_eq!(outcome.exit_code, 0);
        assert!(
            outcome.stderr_tail.len() <= STDERR_TAIL_BYTES,
            "stderr_tail should be capped at {} bytes, got {}",
            STDERR_TAIL_BYTES,
            outcome.stderr_tail.len(),
        );
    }

    #[test]
    fn spawn_nonexistent_binary_returns_spawn_error() {
        let env = empty_env();
        let signal = Arc::new(AtomicBool::new(false));
        let pid = Arc::new(AtomicI32::new(0));
        let bogus = Path::new("/no/such/binary/anywhere");
        let cwd = Path::new("/tmp");
        let args: Vec<String> = vec![];
        let result = spawn_cycle_subprocess(&cmd_for(
            bogus, cwd, &env, "", &args, signal, pid, None,
        ));
        assert!(matches!(result, Err(SubprocessError::Spawn { .. })));
    }

    // ── Adversarial-review-2 fixes ─────────────────────────────────

    #[test]
    fn spawn_tolerates_non_utf8_byte_mid_stream() {
        // Per adversarial review #1: a single non-UTF-8 byte on
        // stdout used to break the loop and silently drop every
        // subsequent valid event (including final Usage +
        // TurnComplete, breaking spend attribution). Now the
        // reader is byte-level + lossy-decode; one bad byte
        // converts to U+FFFD inside one bad line, parse_event_line
        // rejects that line, and subsequent valid lines parse
        // correctly.
        let tmp = tempfile::tempdir_in("/home/nemesis/code").unwrap();
        let bin = write_script(
            tmp.path(),
            "agent",
            r#"#!/bin/sh
printf '%s\n' '{"session_id":"s","entry_id":"e1","timestamp":0,"kind":{"type":"session_started","id":"s","cwd":"/x","model":"m","provider":"p"}}'
printf '\xff\xfe garbage byte mid-line\n'
printf '%s\n' '{"session_id":"s","entry_id":"e3","timestamp":0,"kind":{"type":"turn_complete"}}'
exit 0
"#,
        );
        let env = empty_env();
        let signal = Arc::new(AtomicBool::new(false));
        let pid = Arc::new(AtomicI32::new(0));
        let args: Vec<String> = vec![];
        let outcome = spawn_cycle_subprocess(&cmd_for(
            &bin, tmp.path(), &env, "", &args, signal, pid, None,
        ))
        .expect("spawn ok");
        // Two valid events parsed; the non-UTF-8 line is dropped
        // by parse_event_line (lossy decode → "?? garbage" which
        // isn't valid JSON).
        assert_eq!(
            outcome.events.len(),
            2,
            "non-UTF-8 byte must NOT silently drop subsequent valid lines"
        );
    }

    #[test]
    fn spawn_caps_jsonl_line_at_max_length() {
        // Per adversarial review #2: a single huge line used to
        // cause unbounded heap allocation. Now lines past 1 MiB
        // are dropped + warn-logged once.
        let tmp = tempfile::tempdir_in("/home/nemesis/code").unwrap();
        // Emit one valid line, then a 2 MiB line of `x`, then
        // one more valid line. The over-cap middle line should
        // be dropped; the bookends should parse.
        let bin = write_script(
            tmp.path(),
            "agent",
            r#"#!/bin/sh
printf '%s\n' '{"session_id":"s","entry_id":"e1","timestamp":0,"kind":{"type":"session_started","id":"s","cwd":"/x","model":"m","provider":"p"}}'
yes "x" | head -c 2097152 | tr -d '\n'
printf '\n'
printf '%s\n' '{"session_id":"s","entry_id":"e3","timestamp":0,"kind":{"type":"turn_complete"}}'
exit 0
"#,
        );
        let env = empty_env();
        let signal = Arc::new(AtomicBool::new(false));
        let pid = Arc::new(AtomicI32::new(0));
        let args: Vec<String> = vec![];
        let outcome = spawn_cycle_subprocess(&cmd_for(
            &bin, tmp.path(), &env, "", &args, signal, pid, None,
        ))
        .expect("spawn ok");
        assert_eq!(outcome.exit_code, 0);
        // The 2 MiB line is dropped; the two valid bookends parse.
        assert_eq!(
            outcome.events.len(),
            2,
            "2 MiB line should be dropped; bookend lines should still parse"
        );
    }

    #[test]
    fn inherit_stdio_skips_event_parsing_and_stderr_capture() {
        // Phase 2d: orchestrate-shape spawn. inherit_stdio = true
        // means the child writes directly to our stdout/stderr
        // (which `cargo test` captures); events_buf + stderr_tail
        // stay empty. The wait + signal + timeout machinery still
        // works.
        let tmp = tempfile::tempdir_in("/home/nemesis/code").unwrap();
        let bin = write_script(
            tmp.path(),
            "agent",
            r#"#!/bin/sh
printf '%s\n' '{"session_id":"s","entry_id":"e1","timestamp":0,"kind":{"type":"session_started","id":"s","cwd":"/x","model":"m","provider":"p"}}'
printf 'diag\n' >&2
exit 0
"#,
        );
        let env = empty_env();
        let signal = Arc::new(AtomicBool::new(false));
        let pid = Arc::new(AtomicI32::new(0));
        let args: Vec<String> = vec![];
        // Build the inherit_stdio variant by hand (cmd_for's
        // default is pipe-mode).
        let cmd = CycleSubprocessCommand {
            name: "orchestrate-like",
            binary: &bin,
            args: &args,
            prompt: "",
            cwd: tmp.path(),
            env_extra: &env,
            timeout: None,
            pid_shared: pid,
            signal_received: signal,
            inherit_stdio: true,
        };
        let outcome = spawn_cycle_subprocess(&cmd).expect("spawn ok");
        assert_eq!(outcome.exit_code, 0);
        // Events buffer is empty (no JSONL parsing thread).
        assert!(
            outcome.events.is_empty(),
            "inherit_stdio = true → events MUST be empty (no parser)"
        );
        // stderr_tail empty too (no ring-buffer thread).
        assert!(
            outcome.stderr_tail.is_empty(),
            "inherit_stdio = true → stderr_tail MUST be empty"
        );
    }

    #[test]
    fn spawn_handles_trailing_partial_line_without_newline() {
        let tmp = tempfile::tempdir_in("/home/nemesis/code").unwrap();
        // Last line has no terminating \n.
        let bin = write_script(
            tmp.path(),
            "agent",
            r#"#!/bin/sh
printf '%s\n' '{"session_id":"s","entry_id":"e1","timestamp":0,"kind":{"type":"session_started","id":"s","cwd":"/x","model":"m","provider":"p"}}'
printf '%s'   '{"session_id":"s","entry_id":"e2","timestamp":0,"kind":{"type":"turn_complete"}}'
exit 0
"#,
        );
        let env = empty_env();
        let signal = Arc::new(AtomicBool::new(false));
        let pid = Arc::new(AtomicI32::new(0));
        let args: Vec<String> = vec![];
        let outcome = spawn_cycle_subprocess(&cmd_for(
            &bin, tmp.path(), &env, "", &args, signal, pid, None,
        ))
        .expect("spawn ok");
        // Both lines parse — the trailing one is flushed at EOF.
        assert_eq!(outcome.events.len(), 2);
    }
}
