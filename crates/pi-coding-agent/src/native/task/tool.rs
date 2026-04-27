//! `task` tool — agent-facing surface over [`super::executor`].
//!
//! Returns a single [`pi_ai::ToolResult`] per invocation regardless of
//! how many subtasks ran or how many turns each subagent took. Full
//! transcripts stay out of the parent's context window; the rich
//! payload sits in `display` so a TUI can drill down.

use async_trait::async_trait;
use pi_ai::{ToolResult, ToolSpec};
use pi_tools::{Tool, ToolContext, ToolError};
use serde_json::{json, Value};
use std::sync::Arc;

use super::discovery;
use super::executor::{self, TaskInput};

pub struct TaskTool;

impl TaskTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TaskTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for TaskTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "task".into(),
            description:
                "Delegate one or more units of work to a named subagent. Each task runs \
                 in a fresh runtime with isolated context, optionally a different model, \
                 and a (possibly restricted) tool set. Returns a single result block per \
                 task — the subagent's own transcript stays out of your context."
                    .into(),
            input_schema: json!({
                "type": "object",
                "required": ["agent", "tasks"],
                "properties": {
                    "agent":   { "type": "string", "description": "Subagent name." },
                    "context": { "type": "string", "description": "Shared markdown prepended to every task's first user message. Optional." },
                    "tasks": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "required": ["id", "assignment"],
                            "properties": {
                                "id":          { "type": "string" },
                                "description": { "type": "string" },
                                "assignment":  { "type": "string" }
                            }
                        }
                    },
                    "isolated": { "type": "boolean", "default": false, "description": "If true, run inside a git worktree (RFD 0006). v1: parsed but no-op." }
                }
            }),
        }
    }

    fn read_only(&self) -> bool {
        // Subagents may invoke arbitrary tools, including `bash`/`write`.
        // Auto-approve must inspect each child invocation independently.
        false
    }

    async fn invoke(
        &self,
        ctx: &ToolContext,
        call_id: &str,
        input: Value,
    ) -> Result<ToolResult, ToolError> {
        // 1. Parse input.
        let agent_name = input
            .get("agent")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing `agent`".into()))?
            .to_string();
        let shared_context = input
            .get("context")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let isolated = input
            .get("isolated")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if isolated {
            tracing::warn!(
                "task: isolated=true requested but worktree isolation is RFD 0006 (v1: no-op)"
            );
        }
        let tasks_v = input
            .get("tasks")
            .and_then(|v| v.as_array())
            .ok_or_else(|| ToolError::InvalidInput("missing `tasks` array".into()))?;
        let mut tasks: Vec<TaskInput> = Vec::with_capacity(tasks_v.len());
        for (i, t) in tasks_v.iter().enumerate() {
            let id = t
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ToolError::InvalidInput(format!("tasks[{i}].id missing")))?
                .to_string();
            let assignment = t
                .get("assignment")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ToolError::InvalidInput(format!("tasks[{i}].assignment missing"))
                })?
                .to_string();
            let description = t
                .get("description")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            tasks.push(TaskInput {
                id,
                description,
                assignment,
            });
        }
        if tasks.is_empty() {
            return Ok(error_result(
                call_id,
                "tasks array must be non-empty".to_string(),
            ));
        }

        // 2. Discover the agent.
        let repo_root = ctx.cwd.clone();
        let agents = discovery::load_all(&repo_root);
        let Some(agent) = agents.into_iter().find(|a| a.name == agent_name) else {
            return Ok(error_result(
                call_id,
                format!("unknown subagent `{agent_name}`"),
            ));
        };

        // 3. Spawn-rule check. If we're already running inside a
        //    subagent, that agent's `spawns:` rule decides.
        let handle = match current_runtime() {
            Some(h) => h,
            None => {
                return Ok(error_result(
                    call_id,
                    "task tool: runtime handle not registered (subagents disabled)".to_string(),
                ));
            }
        };
        if let Some(active) = &handle.current_agent {
            if let Err(e) = executor::check_spawn_allowed(Some(active), &agent.name) {
                return Ok(error_result(call_id, e.to_string()));
            }
        }

        // 4. (handle resolved above)

        // 5. Run.
        let max_conc = handle.parent_cfg.settings.task.max_concurrency.max(1);
        let result = executor::run_batch(
            &handle,
            &agent,
            shared_context.as_deref(),
            tasks,
            max_conc,
        )
        .await;

        // 6. Package.
        let model_output = result
            .results
            .iter()
            .map(|r| {
                format!(
                    "[task {} — {}] {}",
                    r.id,
                    if r.success { "ok" } else { "error" },
                    r.model_output.trim()
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let display = serde_json::to_value(&result).unwrap_or(Value::Null);
        let any_error = result.results.iter().any(|r| !r.success);
        Ok(ToolResult {
            tool_use_id: call_id.into(),
            model_output,
            display: Some(display),
            is_error: any_error,
        })
    }
}

fn error_result(call_id: &str, msg: String) -> ToolResult {
    ToolResult {
        tool_use_id: call_id.into(),
        model_output: format!("ERROR: {msg}"),
        display: Some(json!({ "ok": false, "error": msg })),
        is_error: true,
    }
}

// ── runtime / agent thread-locals ────────────────────────────────────────────
//
// The `task` tool needs a handle on the calling runtime (to clone its
// `RuntimeConfig`) and on the calling subagent (to enforce `spawns:`).
// The cleanest place to plumb this is the runtime itself, but we cannot
// touch `RuntimeConfig` per the RFD's hard rules. Workaround: a pair of
// thread-locals — set by the host before invoking `prompt()`, observed
// by `TaskTool::invoke`. This is exactly the pattern used by Anthropic's
// own SDK for the same plumbing problem.

use tokio::task_local;

#[derive(Clone)]
pub struct ParentHandle {
    pub parent_cfg: Arc<pi_agent_core::RuntimeConfig>,
    pub parent_session: pi_agent_core::AgentSession,
    /// The subagent currently active in the calling frame, if any.
    /// Top-level invocations from the user's primary agent leave this
    /// `None`. Set by [`super::executor::run_one`] when it dives into
    /// a child runtime so nested `task` calls can enforce `spawns:`.
    pub current_agent: Option<super::definition::AgentDefinition>,
}

task_local! {
    static CURRENT_RUNTIME: ParentHandle;
}

pub fn current_runtime() -> Option<ParentHandle> {
    CURRENT_RUNTIME.try_with(|h| h.clone()).ok()
}

/// Run `fut` with a task-local [`ParentHandle`] in scope so that any
/// `task` tool invocation made inside `fut` can locate the calling
/// runtime + session. Hosts call this around `session.prompt(...)`.
pub async fn with_runtime<F: std::future::Future>(handle: ParentHandle, fut: F) -> F::Output {
    CURRENT_RUNTIME.scope(handle, fut).await
}
