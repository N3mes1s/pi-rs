//! B2: Native `todo` tool.

use pi_ai::{ToolResult, ToolSpec};
use pi_coding_agent::native::todo::{self, Phase, Task, TaskState, Todo, TodoTool};
use pi_tools::{Tool, ToolContext};
use serde_json::json;
use tempfile::tempdir;

fn ctx(cwd: &std::path::Path) -> ToolContext {
    ToolContext {
        cwd: cwd.to_path_buf(),
        max_output_bytes: 256 * 1024,
    }
}

async fn invoke(tool: &TodoTool, cwd: &std::path::Path, input: serde_json::Value) -> ToolResult {
    let c = ctx(cwd);
    tool.invoke(&c, "call_1", input).await.expect("ok")
}

#[test]
fn task_state_parses_canonical_names_and_kebab_variant() {
    assert_eq!(TaskState::parse("pending"), Some(TaskState::Pending));
    assert_eq!(TaskState::parse("in_progress"), Some(TaskState::InProgress));
    assert_eq!(TaskState::parse("in-progress"), Some(TaskState::InProgress));
    assert_eq!(TaskState::parse("completed"), Some(TaskState::Completed));
    assert_eq!(TaskState::parse("abandoned"), Some(TaskState::Abandoned));
    assert_eq!(TaskState::parse("nope"), None);
}

#[test]
fn normalise_demotes_extra_in_progress_tasks_to_pending() {
    let mut t = Todo::new();
    t.add_phase("phase1");
    t.add_task(
        0,
        Task {
            id: "a".into(),
            text: "first".into(),
            state: TaskState::InProgress,
        },
    );
    t.add_task(
        0,
        Task {
            id: "b".into(),
            text: "second".into(),
            state: TaskState::InProgress,
        },
    );
    // Only the first survives as InProgress.
    assert_eq!(t.find("a").unwrap().state, TaskState::InProgress);
    assert_eq!(t.find("b").unwrap().state, TaskState::Pending);
}

#[test]
fn normalise_keeps_first_in_progress_across_phases() {
    let mut t = Todo::new();
    t.replace(vec![
        Phase {
            name: "p1".into(),
            tasks: vec![Task {
                id: "a".into(),
                text: "".into(),
                state: TaskState::Pending,
            }],
        },
        Phase {
            name: "p2".into(),
            tasks: vec![
                Task {
                    id: "b".into(),
                    text: "".into(),
                    state: TaskState::InProgress,
                },
                Task {
                    id: "c".into(),
                    text: "".into(),
                    state: TaskState::InProgress,
                },
            ],
        },
    ]);
    assert_eq!(t.find("b").unwrap().state, TaskState::InProgress);
    assert_eq!(t.find("c").unwrap().state, TaskState::Pending);
}

#[tokio::test]
async fn replace_persists_phases_to_disk() {
    let dir = tempdir().unwrap();
    let tool = TodoTool;
    let r = invoke(
        &tool,
        dir.path(),
        json!({
            "op": "replace",
            "phases": [
                {"name": "Setup", "tasks": [{"id": "t1", "text": "init repo", "state": "completed"}]},
                {"name": "Build", "tasks": [{"id": "t2", "text": "scaffold", "state": "in_progress"}]}
            ]
        }),
    )
    .await;
    assert!(!r.is_error);
    let stored = todo::load(dir.path()).unwrap();
    assert_eq!(stored.phases.len(), 2);
    assert_eq!(stored.phases[0].name, "Setup");
    assert_eq!(stored.phases[1].tasks[0].state, TaskState::InProgress);
}

#[tokio::test]
async fn add_phase_appends_to_existing_list() {
    let dir = tempdir().unwrap();
    let tool = TodoTool;
    invoke(&tool, dir.path(), json!({"op": "add_phase", "name": "P1"})).await;
    invoke(&tool, dir.path(), json!({"op": "add_phase", "name": "P2"})).await;
    let stored = todo::load(dir.path()).unwrap();
    assert_eq!(stored.phases.len(), 2);
    assert_eq!(stored.phases[1].name, "P2");
}

#[tokio::test]
async fn add_task_into_specific_phase_and_normalise() {
    let dir = tempdir().unwrap();
    let tool = TodoTool;
    invoke(&tool, dir.path(), json!({"op": "add_phase", "name": "A"})).await;
    invoke(&tool, dir.path(), json!({"op": "add_phase", "name": "B"})).await;
    invoke(
        &tool,
        dir.path(),
        json!({
            "op": "add_task",
            "phase_index": 1,
            "id": "x",
            "text": "do stuff",
            "state": "in_progress"
        }),
    )
    .await;
    let stored = todo::load(dir.path()).unwrap();
    assert_eq!(stored.phases[0].tasks.len(), 0);
    assert_eq!(stored.phases[1].tasks.len(), 1);
    assert_eq!(stored.phases[1].tasks[0].state, TaskState::InProgress);
}

#[tokio::test]
async fn add_task_with_bad_phase_index_returns_error() {
    let dir = tempdir().unwrap();
    let tool = TodoTool;
    let c = ctx(dir.path());
    let err = tool
        .invoke(
            &c,
            "x",
            json!({
                "op": "add_task",
                "phase_index": 99,
                "id": "x",
                "text": "y"
            }),
        )
        .await
        .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("out of range"), "got: {msg}");
}

#[tokio::test]
async fn update_changes_state_and_normalises() {
    let dir = tempdir().unwrap();
    let tool = TodoTool;
    invoke(&tool, dir.path(), json!({"op": "add_phase", "name": "P"})).await;
    invoke(
        &tool,
        dir.path(),
        json!({"op": "add_task", "phase_index": 0, "id": "a", "text": "a"}),
    )
    .await;
    invoke(
        &tool,
        dir.path(),
        json!({"op": "add_task", "phase_index": 0, "id": "b", "text": "b"}),
    )
    .await;
    invoke(
        &tool,
        dir.path(),
        json!({"op": "update", "id": "a", "state": "in_progress"}),
    )
    .await;
    invoke(
        &tool,
        dir.path(),
        json!({"op": "update", "id": "b", "state": "in_progress"}),
    )
    .await;
    let stored = todo::load(dir.path()).unwrap();
    // Most-recent in_progress wins is NOT the contract — the normaliser
    // walks in order and demotes later ones, so the *first* in_progress
    // (a) should remain in_progress and (b) gets demoted.
    // After two updates: 'a' was set to in_progress, then 'b' set to
    // in_progress. After 'b' is set, normalise walks: a=InProgress (kept),
    // b=InProgress (demoted to Pending).
    assert_eq!(stored.find("a").unwrap().state, TaskState::InProgress);
    assert_eq!(stored.find("b").unwrap().state, TaskState::Pending);
}

#[tokio::test]
async fn remove_task_drops_by_id() {
    let dir = tempdir().unwrap();
    let tool = TodoTool;
    invoke(&tool, dir.path(), json!({"op": "add_phase", "name": "P"})).await;
    invoke(
        &tool,
        dir.path(),
        json!({"op": "add_task", "phase_index": 0, "id": "a", "text": "a"}),
    )
    .await;
    invoke(&tool, dir.path(), json!({"op": "remove_task", "id": "a"})).await;
    let stored = todo::load(dir.path()).unwrap();
    assert!(stored.phases[0].tasks.is_empty());
}

#[tokio::test]
async fn unknown_op_is_an_error() {
    let dir = tempdir().unwrap();
    let tool = TodoTool;
    let c = ctx(dir.path());
    let err = tool
        .invoke(&c, "x", json!({"op": "set_fire"}))
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("unknown op"));
}

#[test]
fn spec_advertises_canonical_name_and_op_enum() {
    let s: ToolSpec = TodoTool.spec();
    assert_eq!(s.name, "todo");
    let ops = s.input_schema.get("properties").unwrap().get("op").unwrap();
    let enum_vals = ops.get("enum").unwrap().as_array().unwrap();
    let names: Vec<&str> = enum_vals.iter().map(|v| v.as_str().unwrap()).collect();
    assert!(names.contains(&"replace"));
    assert!(names.contains(&"add_phase"));
    assert!(names.contains(&"add_task"));
    assert!(names.contains(&"update"));
    assert!(names.contains(&"remove_task"));
}
