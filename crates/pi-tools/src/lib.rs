//! pi-tools — meta-crate re-exporting pi-tools-core +
//! pi-tools-net for back-compat. Embedders should depend on
//! the split crates directly when possible.
//!
//! # Back-compat guarantee
//!
//! Every `use pi_tools::Tool` / `use pi_tools::ToolRegistry` import that
//! existed before the split continues to compile and behave identically.
//! In particular, `ToolRegistry::with_extras()` still registers all built-in
//! tools including `web_search`.

use std::sync::Arc;

pub use pi_tools_core::{
    bash, edit, find, grep, ls, monitor, read, resolve_path, write, DuplicateName,
    Tool, ToolContext, ToolError, ToolResult, ToolSpec,
};
pub use pi_tools_net::web_search;
pub use pi_tools_net::WebSearchTool;

// ── ToolRegistry ─────────────────────────────────────────────────────────────

/// The canonical tool registry for pi-rs agent sessions.
///
/// Wraps `pi_tools_core::ToolRegistry` and patches `with_extras()` to also
/// include `WebSearchTool`, preserving the pre-split behaviour.
#[derive(Default, Clone)]
pub struct ToolRegistry(pi_tools_core::ToolRegistry);

impl ToolRegistry {
    pub fn new() -> Self {
        Self(pi_tools_core::ToolRegistry::new())
    }

    /// Four essential tools: `read`, `write`, `edit`, `bash`.
    pub fn with_defaults() -> Self {
        Self(pi_tools_core::ToolRegistry::with_defaults())
    }

    /// All built-in tools: `read`, `write`, `edit`, `bash`, `grep`, `find`,
    /// `ls`, and `web_search`.
    ///
    /// Matches the pre-split behaviour of the original `pi-tools` crate.
    pub fn with_extras() -> Self {
        let mut r = Self(pi_tools_core::ToolRegistry::with_extras());
        r.register(Arc::new(WebSearchTool::default()))
            .expect("with_extras: web_search collides with built-in (impossible)");
        r
    }

    /// Read-only inspection tool set: `read`, `grep`, `find`, `ls`. No
    /// shell, no filesystem mutation, no network. Per RFD 0027 §4.5 #12
    /// (Hardening H7): the safe-by-default tool set for SDK embedders.
    pub fn with_readonly_extras() -> Self {
        Self(pi_tools_core::ToolRegistry::with_readonly_extras())
    }

    /// Full tool set including `bash` (code execution), mutation
    /// tools (`write`, `edit`), and `web_search`. The name itself is
    /// the safety signal — production callers should prefer
    /// `with_readonly_extras()` or build the registry explicitly.
    ///
    /// `with_unsafe_extras()` is the renamed-for-safety alias of
    /// `with_extras()`; both return the identical tool set today.
    pub fn with_unsafe_extras() -> Self {
        Self::with_extras()
    }

    /// Register a tool. Per RFD 0027 §4.5 #5 (Hardening H3): rejects
    /// collisions with `Err(DuplicateName)`. Use [`register_or_replace`]
    /// for explicit overrides.
    ///
    /// [`register_or_replace`]: Self::register_or_replace
    pub fn register(&mut self, tool: Arc<dyn Tool>) -> Result<(), DuplicateName> {
        self.0.register(tool)
    }

    /// Register a tool, replacing any existing entry with the same
    /// name. Use when override is intentional.
    pub fn register_or_replace(&mut self, tool: Arc<dyn Tool>) {
        self.0.register_or_replace(tool);
    }

    pub fn unregister(&mut self, name: &str) {
        self.0.unregister(name);
    }

    pub fn names(&self) -> Vec<String> {
        self.0.names()
    }

    pub fn keep_only(&mut self, names: &[String]) {
        self.0.keep_only(names);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.0.get(name)
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.0.specs()
    }
}
