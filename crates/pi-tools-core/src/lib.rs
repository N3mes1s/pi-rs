//! pi-tools-core — file + process tools for pi-rs (no network deps).
//!
//! Mirrors the four essential tools described in the pi blog post:
//! `read`, `write`, `edit`, `bash`, plus the additional read-only `grep`,
//! `find`, `ls` (disabled by default in upstream pi).
//!
//! Each tool implements [`Tool`] and is registered in a [`ToolRegistry`].
//!
//! The `Tool` trait and `ToolContext` struct are defined in `pi-tool-types`
//! and re-exported here so that callers see a single consistent API surface.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use pi_tool_types::{Tool, ToolContext, ToolError, ToolResult, ToolSpec};

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
