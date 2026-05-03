//! pi-tools-core — file + process tools for pi-rs (no network deps).
//!
//! Mirrors the four essential tools described in the pi blog post:
//! `read`, `write`, `edit`, `bash`, plus the additional read-only `grep`,
//! `find`, `ls` (disabled by default in upstream pi).
//!
//! Each tool implements [`Tool`] and is registered in a [`ToolRegistry`].
//!
//! The POD types (`ToolSpec`, `ToolResult`, `ToolError`) come from
//! `pi-tool-types`. The `Tool` trait and `ToolContext` are defined here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use pi_tool_types::{ToolError, ToolResult, ToolSpec};

use async_trait::async_trait;

/// Execution context passed to every tool invocation.
#[derive(Debug, Clone)]
pub struct ToolContext {
    pub cwd: PathBuf,
    pub max_output_bytes: usize,
}

impl Default for ToolContext {
    fn default() -> Self {
        Self {
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            max_output_bytes: 256 * 1024,
        }
    }
}

/// The tool trait every tool must implement.
#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    fn read_only(&self) -> bool;
    async fn invoke(
        &self,
        ctx: &ToolContext,
        call_id: &str,
        input: serde_json::Value,
    ) -> Result<ToolResult, ToolError>;
}

pub mod bash;
pub mod edit;
pub mod find;
pub mod grep;
pub mod ls;
pub mod monitor;
pub mod read;
pub mod write;

#[derive(Default, Clone)]
pub struct ToolRegistry {
    inner: BTreeMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_defaults() -> Self {
        let mut r = Self::default();
        r.register(Arc::new(read::ReadTool));
        r.register(Arc::new(write::WriteTool));
        r.register(Arc::new(edit::EditTool));
        r.register(Arc::new(bash::BashTool));
        r
    }

    pub fn with_extras() -> Self {
        let mut r = Self::with_defaults();
        r.register(Arc::new(grep::GrepTool));
        r.register(Arc::new(find::FindTool));
        r.register(Arc::new(ls::LsTool));
        r
    }

    /// Read-only inspection tool set: `read`, `grep`, `find`, `ls`. No
    /// shell, no filesystem mutation. Per RFD 0027 §4.5 #12 (Hardening
    /// H7): the safe-by-default tool set for embedders.
    pub fn with_readonly_extras() -> Self {
        let mut r = Self::default();
        r.register(Arc::new(read::ReadTool));
        r.register(Arc::new(grep::GrepTool));
        r.register(Arc::new(find::FindTool));
        r.register(Arc::new(ls::LsTool));
        r
    }

    /// Full tool set including `bash` (code execution) and the
    /// mutation tools (`write`, `edit`). Per RFD 0027 §4.5 #12: the
    /// name itself is the safety signal — production callers should
    /// prefer `with_readonly_extras()` or build the registry
    /// explicitly via `new()` + `register()`.
    ///
    /// `with_unsafe_extras()` is the renamed-for-safety alias of
    /// `with_extras()`. Both return the identical tool set today;
    /// pi-rs's own binary continues to use `with_extras()` and
    /// `with_defaults()`. Per RFD 0027 §3 deprecation policy, the
    /// older names live until SDK 1.0+4 MINOR releases (~6 months
    /// past 1.0).
    pub fn with_unsafe_extras() -> Self {
        Self::with_extras()
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.inner.insert(tool.spec().name, tool);
    }

    pub fn unregister(&mut self, name: &str) {
        self.inner.remove(name);
    }

    pub fn names(&self) -> Vec<String> {
        self.inner.keys().cloned().collect()
    }

    pub fn keep_only(&mut self, names: &[String]) {
        let allowed: std::collections::HashSet<&str> = names.iter().map(|s| s.as_str()).collect();
        self.inner.retain(|k, _| allowed.contains(k.as_str()));
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.inner.get(name).cloned()
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.inner.values().map(|t| t.spec()).collect()
    }
}

pub fn resolve_path(ctx: &ToolContext, p: &str) -> PathBuf {
    let expanded = shellexpand::tilde(p).into_owned();
    let path = Path::new(&expanded);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        ctx.cwd.join(path)
    }
}

pub(crate) fn truncate_for_model(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let cut = text
        .char_indices()
        .take_while(|(i, _)| *i < max_bytes)
        .last()
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    let mut s = String::with_capacity(cut + 64);
    s.push_str(&text[..cut]);
    s.push_str(&format!("\n\n[…truncated {} bytes…]", text.len() - cut));
    s
}
