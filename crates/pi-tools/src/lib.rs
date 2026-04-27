//! pi-tools — built-in tools for pi-rs.
//!
//! Mirrors the four essential tools described in the pi blog post:
//! `read`, `write`, `edit`, `bash`, plus the additional read-only `grep`,
//! `find`, `ls` (disabled by default in upstream pi).
//!
//! Each tool implements [`Tool`] and is registered in a [`ToolRegistry`].

use async_trait::async_trait;
use pi_ai::{ToolResult, ToolSpec};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub mod bash;
pub mod edit;
pub mod find;
pub mod grep;
pub mod ls;
pub mod read;
pub mod web_search;
pub mod write;

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("tool not found: {0}")]
    NotFound(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

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

#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    fn read_only(&self) -> bool;
    async fn invoke(
        &self,
        ctx: &ToolContext,
        call_id: &str,
        input: Value,
    ) -> Result<ToolResult, ToolError>;
}

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
        r.register(Arc::new(web_search::WebSearchTool::default()));
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
        let allowed: std::collections::HashSet<&str> =
            names.iter().map(|s| s.as_str()).collect();
        self.inner.retain(|k, _| allowed.contains(k.as_str()));
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.inner.get(name).cloned()
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.inner.values().map(|t| t.spec()).collect()
    }
}

pub(crate) fn resolve_path(ctx: &ToolContext, p: &str) -> PathBuf {
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
    s.push_str(&format!(
        "\n\n[…truncated {} bytes…]",
        text.len() - cut
    ));
    s
}
