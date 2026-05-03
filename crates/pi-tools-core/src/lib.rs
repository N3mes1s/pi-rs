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

/// Returned by [`ToolRegistry::register`] when an entry with the
/// same name is already in the registry. Per RFD 0027 §4.5 #5
/// (Hardening H3): pre-H3 the inner `BTreeMap::insert` was silent
/// last-write-wins, so a malicious dependency that called
/// `register(BashTool::clone())` would shadow the real bash tool
/// invisibly. Now the caller MUST decide: reject (`register` errors)
/// or explicit override ([`ToolRegistry::register_or_replace`]).
#[derive(Debug, thiserror::Error)]
#[error("tool with name `{0}` is already registered")]
pub struct DuplicateName(pub String);

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_defaults() -> Self {
        let mut r = Self::default();
        r.register_unwrap(Arc::new(read::ReadTool));
        r.register_unwrap(Arc::new(write::WriteTool));
        r.register_unwrap(Arc::new(edit::EditTool));
        r.register_unwrap(Arc::new(bash::BashTool));
        r
    }

    /// Read-only inspection tool set: `read`, `grep`, `find`, `ls`. No
    /// shell, no filesystem mutation. Per RFD 0027 §4.5 #12 (Hardening
    /// H7): the safe-by-default tool set for embedders.
    pub fn with_readonly_extras() -> Self {
        let mut r = Self::default();
        r.register_unwrap(Arc::new(read::ReadTool));
        r.register_unwrap(Arc::new(grep::GrepTool));
        r.register_unwrap(Arc::new(find::FindTool));
        r.register_unwrap(Arc::new(ls::LsTool));
        r
    }

    /// Full tool set including `bash` (code execution) and the
    /// mutation tools (`write`, `edit`). Per RFD 0027 §4.5 #12: the
    /// name itself is the safety signal — production callers should
    /// prefer [`with_readonly_extras`](Self::with_readonly_extras)
    /// or build the registry explicitly via [`new`](Self::new) +
    /// [`register`](Self::register).
    ///
    /// (Polish-12: previously this was an alias for `with_extras()`;
    /// the alias was removed pre-publish since 0.x has no committed
    /// back-compat surface yet.)
    pub fn with_unsafe_extras() -> Self {
        let mut r = Self::with_defaults();
        r.register_unwrap(Arc::new(grep::GrepTool));
        r.register_unwrap(Arc::new(find::FindTool));
        r.register_unwrap(Arc::new(ls::LsTool));
        r
    }

    /// Register a tool. Per RFD 0027 §4.5 #5 (Hardening H3), this
    /// rejects collisions with `Err(DuplicateName)` so a malicious
    /// crate cannot transparently shadow `bash` (or any other tool)
    /// with an exfiltrating impl. Use
    /// [`register_or_replace`](Self::register_or_replace) for the
    /// explicit override case.
    pub fn register(&mut self, tool: Arc<dyn Tool>) -> Result<(), DuplicateName> {
        let name = tool.spec().name;
        if self.inner.contains_key(&name) {
            return Err(DuplicateName(name));
        }
        self.inner.insert(name, tool);
        Ok(())
    }

    /// Register a tool, replacing any existing entry with the same
    /// name. Use this when the override is intentional (testing,
    /// runtime patching, etc.). Production embedders should use
    /// [`register`](Self::register) and handle the
    /// `Err(DuplicateName)` explicitly.
    pub fn register_or_replace(&mut self, tool: Arc<dyn Tool>) {
        self.inner.insert(tool.spec().name, tool);
    }

    /// Internal convenience for the built-in `with_*` constructors,
    /// where each tool is statically known to be unique. Panics on
    /// collision (programmer error).
    ///
    /// Per code-review finding #8 (pass-2): kept module-private (no
    /// `pub(crate)` back-door for tests/futures to bypass the H3
    /// `Result<()>` check). The only callers are the `with_*`
    /// constructors in this same `impl ToolRegistry` block.
    fn register_unwrap(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.spec().name.clone();
        self.register(tool).unwrap_or_else(|_| {
            panic!("internal collision in built-in registry for `{name}`")
        });
    }

    #[cfg(test)]
    #[doc(hidden)]
    pub fn _len(&self) -> usize {
        self.inner.len()
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

#[cfg(test)]
mod h3_register_tests {
    //! Per RFD 0027 §4.5 #5 (Hardening H3): `register` must reject
    //! collisions; `register_or_replace` must accept them.

    use super::*;
    use async_trait::async_trait;
    use pi_tool_types::ToolResult;
    use serde_json::Value;

    struct StubTool {
        name: &'static str,
    }

    #[async_trait]
    impl Tool for StubTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.name.into(),
                description: "stub".into(),
                input_schema: serde_json::json!({"type":"object"}),
            }
        }
        fn read_only(&self) -> bool {
            true
        }
        async fn invoke(
            &self,
            _ctx: &ToolContext,
            _id: &str,
            _input: Value,
        ) -> Result<ToolResult, ToolError> {
            unreachable!("stub never invoked in unit test")
        }
    }

    #[test]
    fn register_rejects_duplicate_name() {
        let mut r = ToolRegistry::new();
        r.register(Arc::new(StubTool { name: "x" }))
            .expect("first register should succeed");
        let err = r
            .register(Arc::new(StubTool { name: "x" }))
            .expect_err("second register should fail with DuplicateName");
        assert_eq!(err.0, "x");
        assert_eq!(r._len(), 1, "rejected register must not insert");
    }

    #[test]
    fn register_or_replace_silently_overrides() {
        let mut r = ToolRegistry::new();
        r.register(Arc::new(StubTool { name: "x" })).expect("first");
        r.register_or_replace(Arc::new(StubTool { name: "x" }));
        assert_eq!(r._len(), 1, "override must keep the count at 1");
    }

    #[test]
    fn duplicate_name_error_displays_tool_name() {
        let err = DuplicateName("bash".into());
        assert!(format!("{err}").contains("bash"));
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
