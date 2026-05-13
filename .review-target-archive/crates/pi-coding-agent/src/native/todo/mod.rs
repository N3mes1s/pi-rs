//! Native `todo` tool — phased task lists persisted to `<cwd>/.pi/todo.json`.
//!
//! Five operations:
//!
//! * `replace`     — set the entire phase list.
//! * `add_phase`   — append a new phase.
//! * `add_task`    — append a task to a phase.
//! * `update`      — change a task's status (also re-normalises so exactly
//!                   one task is `in_progress` at any time, see
//!                   [`Todo::normalise`]).
//! * `remove_task` — delete a task by id.
//!
//! Persistence: the entire [`Todo`] is serialised to `.pi/todo.json` after
//! every mutating op. The file is created on first use.

pub mod model;
pub mod store;
pub mod tool;

pub use model::{Phase, Task, TaskState, Todo};
pub use store::{load, save};
pub use tool::TodoTool;
