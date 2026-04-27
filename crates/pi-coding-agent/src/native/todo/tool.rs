//! `todo` tool — agent-facing surface over [`super::model::Todo`].
//!
//! Five operations dispatched on the `op` field of the input:
//!
//! | op            | extra fields                                     |
//! |---------------|--------------------------------------------------|
//! | `replace`     | `phases: [{name, tasks: [{id, text, state?}]}]`  |
//! | `add_phase`   | `name`                                           |
//! | `add_task`    | `phase_index` (or `phase`), `id`, `text`, `state?` |
//! | `update`      | `id`, `state`                                    |
//! | `remove_task` | `id`                                             |
//!
//! Every successful op persists the entire list to `<ctx.cwd>/.pi/todo.json`
//! and returns the new list as `display.todo` for the renderer.

use async_trait::async_trait;
use pi_ai::{ToolResult, ToolSpec};
use pi_tools::{Tool, ToolContext, ToolError};
use serde_json::{json, Value};

use super::model::{Phase, Task, TaskState, Todo};
use super::store;

pub struct TodoTool;

#[async_trait]
impl Tool for TodoTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "todo".into(),
            description:
                "Manage a phased todo list persisted at <cwd>/.pi/todo.json. \
                 Set `op` to one of: `replace` (set the entire list), \
                 `add_phase` (append a phase), `add_task` (append a task to a phase), \
                 `update` (change a task's status: pending|in_progress|completed|abandoned), \
                 `remove_task` (delete a task by id). Exactly one task is allowed to be \
                 in_progress at any time — the tool re-normalises automatically."
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "op":          { "type": "string", "enum": ["replace", "add_phase", "add_task", "update", "remove_task"] },
                    "phases":      { "type": "array", "description": "For op=replace. Array of {name, tasks: [{id, text, state}]}" },
                    "name":        { "type": "string", "description": "For op=add_phase. New phase name." },
                    "phase_index": { "type": "integer", "minimum": 0, "description": "For op=add_task. 0-based index." },
                    "id":          { "type": "string", "description": "For op=add_task / update / remove_task." },
                    "text":        { "type": "string", "description": "For op=add_task." },
                    "state":       { "type": "string", "enum": ["pending", "in_progress", "completed", "abandoned"] }
                },
                "required": ["op"]
            }),
        }
    }

    fn read_only(&self) -> bool {
        false
    }

    async fn invoke(
        &self,
        ctx: &ToolContext,
        call_id: &str,
        input: Value,
    ) -> Result<ToolResult, ToolError> {
        let op = input
            .get("op")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing `op`".into()))?
            .to_string();

        let mut todo = store::load(&ctx.cwd).map_err(ToolError::Io)?;

        let summary = match op.as_str() {
            "replace" => {
                let phases = parse_phases(input.get("phases"))?;
                todo.replace(phases);
                "todo: replaced".to_string()
            }
            "add_phase" => {
                let name = input
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidInput("missing `name`".into()))?;
                todo.add_phase(name);
                format!("todo: phase '{name}' added")
            }
            "add_task" => {
                let id = require_str(&input, "id")?;
                let text = require_str(&input, "text")?;
                let state = parse_state(input.get("state")).unwrap_or_default();
                let phase_index = input
                    .get("phase_index")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize)
                    .unwrap_or(0);
                let ok = todo.add_task(
                    phase_index,
                    Task {
                        id: id.into(),
                        text: text.into(),
                        state,
                    },
                );
                if !ok {
                    return Err(ToolError::InvalidInput(format!(
                        "phase_index {phase_index} out of range (have {} phases)",
                        todo.phases.len()
                    )));
                }
                format!("todo: task '{id}' added to phase {phase_index}")
            }
            "update" => {
                let id = require_str(&input, "id")?;
                let state = parse_state(input.get("state"))
                    .ok_or_else(|| ToolError::InvalidInput("missing or bad `state`".into()))?;
                if !todo.update(id, state) {
                    return Err(ToolError::InvalidInput(format!(
                        "no task with id '{id}'"
                    )));
                }
                format!("todo: task '{id}' → {state:?}")
            }
            "remove_task" => {
                let id = require_str(&input, "id")?;
                if !todo.remove_task(id) {
                    return Err(ToolError::InvalidInput(format!(
                        "no task with id '{id}'"
                    )));
                }
                format!("todo: task '{id}' removed")
            }
            other => {
                return Err(ToolError::InvalidInput(format!("unknown op `{other}`")));
            }
        };

        store::save(&ctx.cwd, &todo).map_err(ToolError::Io)?;

        Ok(ToolResult {
            tool_use_id: call_id.into(),
            model_output: format!("{summary}\n{}", render_text(&todo)),
            display: Some(json!({
                "kind": "todo",
                "todo": todo,
            })),
            is_error: false,
        })
    }
}

fn require_str<'a>(input: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    input
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidInput(format!("missing `{key}`")))
}

fn parse_state(v: Option<&Value>) -> Option<TaskState> {
    v.and_then(|x| x.as_str()).and_then(TaskState::parse)
}

fn parse_phases(v: Option<&Value>) -> Result<Vec<Phase>, ToolError> {
    let arr = v
        .and_then(|x| x.as_array())
        .ok_or_else(|| ToolError::InvalidInput("missing `phases` array".into()))?;
    let mut out = Vec::with_capacity(arr.len());
    for p in arr {
        let name = p
            .get("name")
            .and_then(|x| x.as_str())
            .ok_or_else(|| ToolError::InvalidInput("phase.name missing".into()))?;
        let tasks_v = p.get("tasks").and_then(|x| x.as_array());
        let mut tasks = Vec::new();
        if let Some(arr) = tasks_v {
            for t in arr {
                let id = t
                    .get("id")
                    .and_then(|x| x.as_str())
                    .ok_or_else(|| ToolError::InvalidInput("task.id missing".into()))?;
                let text = t
                    .get("text")
                    .and_then(|x| x.as_str())
                    .ok_or_else(|| ToolError::InvalidInput("task.text missing".into()))?;
                let state = parse_state(t.get("state")).unwrap_or_default();
                tasks.push(Task {
                    id: id.into(),
                    text: text.into(),
                    state,
                });
            }
        }
        out.push(Phase {
            name: name.into(),
            tasks,
        });
    }
    Ok(out)
}

/// Plain-text rendering used in `model_output` so the agent can see the
/// post-mutation list without re-reading the file.
pub fn render_text(todo: &Todo) -> String {
    let mut out = String::new();
    for (i, p) in todo.phases.iter().enumerate() {
        out.push_str(&format!("[{}] {}\n", i, p.name));
        for t in &p.tasks {
            let glyph = match t.state {
                TaskState::Pending => "[ ]",
                TaskState::InProgress => "[*]",
                TaskState::Completed => "[x]",
                TaskState::Abandoned => "[-]",
            };
            out.push_str(&format!("  {} {} — {}\n", glyph, t.id, t.text));
        }
    }
    if out.is_empty() {
        out.push_str("(empty)\n");
    }
    out
}
