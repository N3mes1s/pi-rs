//! Subagents and the `task` tool — implements RFD 0005.
//!
//! Public surface (consumed from `startup.rs`):
//! * [`tool::TaskTool`] — the `pi_tools::Tool` implementation.
//! * [`tool::ParentHandle`] / [`tool::with_runtime`] — the task-local
//!   shim hosts use to propagate the calling runtime into nested tool
//!   invocations.
//! * [`discovery::load_all`] — frontmatter discovery across the three
//!   roots (bundled / user / project).
//! * [`definition::AgentDefinition`] — parsed frontmatter + body.
//! * [`executor`] — batch runner used by the tool.

pub mod definition;
pub mod discovery;
pub mod executor;
pub mod tool;

pub use definition::{AgentDefinition, AgentSource, SpawnsRule};
pub use executor::{TaskBatchResult, TaskInput, TaskOutcome};
pub use tool::{ParentHandle, TaskTool};
