//! pi-tools-net — network-using tools (web_search, …).
//! Companion to pi-tools-core.
//!
//! Depends only on `pi-tool-types` for the shared `Tool` / `ToolContext` /
//! `ToolError` types.  Network-specific deps (`reqwest`, etc.) are not
//! pulled into `pi-tools-core`.

// Re-export the shared tool types so that web_search.rs can use
// `crate::{Tool, ToolContext, ToolError}` without modification.
pub use pi_tool_types::{Tool, ToolContext, ToolError, ToolResult, ToolSpec};

pub mod web_search;
pub use web_search::WebSearchTool;
