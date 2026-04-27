//! Native `ask` tool — pose a structured multiple-choice question to the
//! user. The agent supplies `question`, `options`, optional
//! `descriptions`, and an `allow_multi` flag.
//!
//! In an interactive TUI a picker overlay would handle the selection and
//! return `{answers: string[]}`. That picker integration requires
//! plumbing a per-call callback through `ToolContext`, which is a wider
//! refactor than the per-item budget here (the runtime in pi-agent-core
//! doesn't currently expose any interactive hook). For now the tool
//! returns a structured `is_error: true, model_output: "ASK requires
//! interactive mode"` response in every mode, matching the print/json
//! contract exactly. The interactive picker should be wired up in a
//! follow-up by extending `ToolContext` with a callback channel.

pub mod tool;

pub use tool::{AskInput, AskTool};
