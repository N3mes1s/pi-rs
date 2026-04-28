//! Subprocess-based extensions.
//!
//! pi.dev's blog post explicitly favours "CLI tools with READMEs" over
//! plugin runtimes. We honour that by treating each `.pi/extensions/<name>/`
//! directory (or any path passed via `-e`) as a self-describing process
//! that exposes:
//!
//! * `pi-extension.json` — manifest declaring exported tools and slash
//!   commands and the executable to invoke.
//! * the executable accepts a single JSON-line on stdin and writes one
//!   JSON-line response on stdout.
//!
//! Manifest schema:
//!
//! ```json
//! {
//!   "name": "deploy",
//!   "version": "0.1.0",
//!   "executable": "./bin/deploy",
//!   "tools": [
//!     {
//!       "name": "deploy",
//!       "description": "Deploy current branch",
//!       "input_schema": {"type": "object", "properties": {"target": {"type": "string"}}}
//!     }
//!   ],
//!   "commands": [
//!     {"name": "deploy-status", "description": "Print deploy status"}
//!   ]
//! }
//! ```
//!
//! Each tool invocation runs `executable invoke <tool>` with input on
//! stdin; each command invocation runs `executable command <name> <args>`.

use async_trait::async_trait;
use pi_ai::{ToolResult, ToolSpec};
use pi_tools::{Tool, ToolContext, ToolError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionToolManifest {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub input_schema: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionCommandManifest {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionKeybinding {
    pub chord: String,
    pub command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionHook {
    /// One of: "tool_call" | "tool_result" | "assistant_message" | "user_message"
    pub event: String,
    /// Path to the executable (relative paths resolved against the extension root).
    pub executable: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionManifest {
    pub name: String,
    #[serde(default)]
    pub version: String,
    pub executable: String,
    #[serde(default)]
    pub tools: Vec<ExtensionToolManifest>,
    #[serde(default)]
    pub commands: Vec<ExtensionCommandManifest>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub keybindings: Vec<ExtensionKeybinding>,
    #[serde(default)]
    pub hooks: Vec<ExtensionHook>,
    /// Built-in tool names that this extension replaces. The named builtins are
    /// unregistered before the extension's own tools are registered, so a
    /// same-named extension tool cleanly shadows the builtin.
    #[serde(default)]
    pub replaces_builtin: Vec<String>,
    /// Optional executable run once at startup (fire-and-forget). Relative
    /// paths are resolved against the extension root. Failures are logged as
    /// warnings and never abort startup.
    #[serde(default)]
    pub startup_executable: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LoadedExtension {
    pub manifest: ExtensionManifest,
    pub root: PathBuf,
}

impl LoadedExtension {
    pub fn executable_path(&self) -> PathBuf {
        let exe = Path::new(&self.manifest.executable);
        if exe.is_absolute() {
            exe.to_path_buf()
        } else {
            self.root.join(exe)
        }
    }

    pub fn timeout(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.manifest.timeout_ms.unwrap_or(120_000))
    }
}

/// A `Tool` implementation that delegates to the subprocess.
pub struct ExtensionTool {
    pub ext: Arc<LoadedExtension>,
    pub spec: ExtensionToolManifest,
}

#[async_trait]
impl Tool for ExtensionTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.spec.name.clone(),
            description: self.spec.description.clone(),
            input_schema: if self.spec.input_schema.is_null() {
                serde_json::json!({"type": "object"})
            } else {
                self.spec.input_schema.clone()
            },
        }
    }

    fn read_only(&self) -> bool {
        false
    }

    async fn invoke(
        &self,
        _ctx: &ToolContext,
        call_id: &str,
        input: Value,
    ) -> Result<ToolResult, ToolError> {
        let exe = self.ext.executable_path();
        let payload = serde_json::json!({
            "call_id": call_id,
            "tool": self.spec.name,
            "input": input,
        });
        let stdin_text = serde_json::to_string(&payload).unwrap_or_default();

        let mut cmd = tokio::process::Command::new(&exe);
        cmd.arg("invoke")
            .arg(&self.spec.name)
            .current_dir(&self.ext.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| ToolError::Other(format!("spawn extension: {e}")))?;
        if let Some(stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            let mut stdin = stdin;
            let _ = stdin.write_all(stdin_text.as_bytes()).await;
            let _ = stdin.shutdown().await;
        }
        let timeout = self.ext.timeout();
        let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
            Ok(res) => res.map_err(ToolError::Io)?,
            Err(_) => {
                return Ok(ToolResult {
                    tool_use_id: call_id.into(),
                    model_output: format!(
                        "ERROR: extension `{}` timed out after {}ms",
                        self.spec.name,
                        timeout.as_millis()
                    ),
                    display: None,
                    is_error: true,
                });
            }
        };
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if !output.status.success() {
            return Ok(ToolResult {
                tool_use_id: call_id.into(),
                model_output: format!(
                    "ERROR: extension `{}` exited with {}\n{}",
                    self.spec.name,
                    output.status.code().unwrap_or(-1),
                    stderr.trim()
                ),
                display: None,
                is_error: true,
            });
        }
        // Extension contract: stdout is a single JSON line of either:
        //   {"output": "...", "display": ..., "is_error": false}
        // or a bare string, which we wrap.
        if let Ok(val) = serde_json::from_str::<Value>(&stdout) {
            let model_output = val
                .get("output")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| stdout.clone());
            let is_error = val
                .get("is_error")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let display = val.get("display").cloned();
            return Ok(ToolResult {
                tool_use_id: call_id.into(),
                model_output,
                display,
                is_error,
            });
        }
        Ok(ToolResult {
            tool_use_id: call_id.into(),
            model_output: stdout,
            display: None,
            is_error: false,
        })
    }
}

/// Discover extensions from one or more roots.
pub fn discover(roots: &[PathBuf]) -> Vec<LoadedExtension> {
    let mut out = Vec::new();
    for r in roots {
        if r.is_dir() {
            // Either the root itself is an extension, or it's a parent of
            // many extensions.
            let manifest = r.join("pi-extension.json");
            if manifest.is_file() {
                if let Some(ext) = load_one(r) {
                    out.push(ext);
                }
                continue;
            }
            if let Ok(rd) = std::fs::read_dir(r) {
                for ent in rd.flatten() {
                    let p = ent.path();
                    if p.is_dir() && p.join("pi-extension.json").is_file() {
                        if let Some(ext) = load_one(&p) {
                            out.push(ext);
                        }
                    }
                }
            }
        } else if r.is_file() && r.file_name().and_then(|n| n.to_str()) == Some("pi-extension.json")
        {
            if let Some(parent) = r.parent() {
                if let Some(ext) = load_one(parent) {
                    out.push(ext);
                }
            }
        }
    }
    out
}

fn load_one(root: &Path) -> Option<LoadedExtension> {
    let manifest_path = root.join("pi-extension.json");
    let txt = std::fs::read_to_string(&manifest_path).ok()?;
    let manifest: ExtensionManifest = serde_json::from_str(&txt).ok()?;
    Some(LoadedExtension {
        manifest,
        root: root.to_path_buf(),
    })
}

/// Build `Tool` adapters for every tool exported by every extension.
pub fn extension_tools(exts: &[LoadedExtension]) -> Vec<Arc<dyn Tool>> {
    let mut out: Vec<Arc<dyn Tool>> = Vec::new();
    for ext in exts {
        let arc = Arc::new(ext.clone());
        for spec in &ext.manifest.tools {
            out.push(Arc::new(ExtensionTool {
                ext: arc.clone(),
                spec: spec.clone(),
            }));
        }
    }
    out
}

// ── HookDispatcher ────────────────────────────────────────────────────────────

/// Dispatches lifecycle events to registered extension hooks.
///
/// Build once via [`HookDispatcher::from_extensions`] and call
/// [`HookDispatcher::dispatch`] at each interesting event.
pub struct HookDispatcher {
    /// event name → list of (extension_root, hook_executable, timeout)
    per_event: std::collections::HashMap<String, Vec<(PathBuf, PathBuf, std::time::Duration)>>,
}

impl HookDispatcher {
    /// Build a dispatcher from a slice of already-loaded extensions.
    ///
    /// Hook executables that are relative paths are resolved against
    /// `extension.root`, mirroring [`LoadedExtension::executable_path`].
    pub fn from_extensions(exts: &[LoadedExtension]) -> Self {
        let mut per_event: std::collections::HashMap<
            String,
            Vec<(PathBuf, PathBuf, std::time::Duration)>,
        > = std::collections::HashMap::new();

        for ext in exts {
            let timeout = ext.timeout();
            for hook in &ext.manifest.hooks {
                let exe_path = {
                    let p = Path::new(&hook.executable);
                    if p.is_absolute() {
                        p.to_path_buf()
                    } else {
                        ext.root.join(p)
                    }
                };
                per_event.entry(hook.event.clone()).or_default().push((
                    ext.root.clone(),
                    exe_path,
                    timeout,
                ));
            }
        }

        Self { per_event }
    }

    /// Fire all hooks registered for `event`, writing `payload` as a single
    /// JSON line to each hook's stdin.  Hooks run concurrently; errors are
    /// logged via [`tracing::warn!`] and never propagate.
    pub async fn dispatch(&self, event: &str, payload: &serde_json::Value) {
        let hooks = match self.per_event.get(event) {
            Some(h) if !h.is_empty() => h,
            _ => return,
        };

        let line = match serde_json::to_string(payload) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(
                    "hook_dispatcher: failed to serialize payload for event `{event}`: {e}"
                );
                return;
            }
        };

        let futs: Vec<_> = hooks
            .iter()
            .map(|(root, exe, timeout)| {
                let line = line.clone();
                let root = root.clone();
                let exe = exe.clone();
                let timeout = *timeout;
                let event = event.to_string();
                async move {
                    let mut cmd = tokio::process::Command::new(&exe);
                    cmd.current_dir(&root)
                        .stdin(Stdio::piped())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null());

                    let mut child = match cmd.spawn() {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::warn!(
                                "hook_dispatcher: failed to spawn hook `{}` for event `{event}`: {e}",
                                exe.display()
                            );
                            return;
                        }
                    };

                    if let Some(stdin) = child.stdin.take() {
                        use tokio::io::AsyncWriteExt;
                        let mut stdin = stdin;
                        let _ = stdin.write_all(line.as_bytes()).await;
                        let _ = stdin.write_all(b"\n").await;
                        let _ = stdin.shutdown().await;
                    }

                    match tokio::time::timeout(timeout, child.wait()).await {
                        Ok(Ok(_)) => {}
                        Ok(Err(e)) => {
                            tracing::warn!(
                                "hook_dispatcher: hook `{}` for event `{event}` wait error: {e}",
                                exe.display()
                            );
                        }
                        Err(_) => {
                            tracing::warn!(
                                "hook_dispatcher: hook `{}` for event `{event}` timed out after {}ms",
                                exe.display(),
                                timeout.as_millis()
                            );
                        }
                    }
                }
            })
            .collect();

        futures::future::join_all(futs).await;
    }
}

// ── Replacement helpers ───────────────────────────────────────────────────────

/// For every loaded extension that declares `replaces_builtin`, unregister the
/// named builtins from `reg` before registering the extension's own tools.
///
/// This is extracted as a free function so it can be unit-tested independently
/// of the full startup machinery.
pub fn apply_replacements(reg: &mut pi_tools::ToolRegistry, exts: &[LoadedExtension]) {
    for ext in exts {
        for name in &ext.manifest.replaces_builtin {
            reg.unregister(name);
        }
    }
}

// ── Startup hooks ─────────────────────────────────────────────────────────────

/// Run the `startup_executable` for every loaded extension that declares one.
///
/// Each executable is spawned and awaited (with `timeout_ms` or 30 000 ms as
/// the fallback). stdout/stderr are discarded. Any error is logged via
/// [`tracing::warn!`] and never propagates — startup hooks are best-effort.
pub async fn run_startup_hooks(exts: &[LoadedExtension]) {
    for ext in exts {
        let se = match &ext.manifest.startup_executable {
            Some(s) => s.clone(),
            None => continue,
        };

        let exe_path = {
            let p = std::path::Path::new(&se);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                ext.root.join(p)
            }
        };

        let timeout = std::time::Duration::from_millis(ext.manifest.timeout_ms.unwrap_or(30_000));
        let ext_name = ext.manifest.name.clone();
        let root = ext.root.clone();

        let mut cmd = tokio::process::Command::new(&exe_path);
        cmd.current_dir(&root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    "startup_hook: failed to spawn `{}` for extension `{ext_name}`: {e}",
                    exe_path.display()
                );
                continue;
            }
        };

        match tokio::time::timeout(timeout, child.wait_with_output()).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                tracing::warn!("startup_hook: extension `{ext_name}` hook wait error: {e}");
            }
            Err(_) => {
                tracing::warn!(
                    "startup_hook: extension `{ext_name}` startup hook timed out after {}ms",
                    timeout.as_millis()
                );
            }
        }
    }
}

/// Run an extension command (slash-command style). Returns stdout.
pub async fn run_command(ext: &LoadedExtension, name: &str, args: &str) -> std::io::Result<String> {
    let exe = ext.executable_path();
    let mut cmd = tokio::process::Command::new(&exe);
    cmd.arg("command")
        .arg(name)
        .arg(args)
        .current_dir(&ext.root);
    let timeout = ext.timeout();
    let child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "extension timeout"))??;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    Ok(stdout)
}
