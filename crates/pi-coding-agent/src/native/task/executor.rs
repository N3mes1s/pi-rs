//! Spawn a subagent: clone the parent's [`RuntimeConfig`] with four
//! substitutions (system prompt, model, tool registry, session manager)
//! and run a single `prompt()` turn. The full subagent transcript is
//! collapsed into a [`TaskOutcome`]; the parent only ever sees the
//! batched [`TaskBatchResult`] in its `tool_result`.

use futures::{stream, StreamExt};
use pi_agent_core::{AgentSession, AgentSessionRuntime, RuntimeConfig, Settings};
use pi_ai::{Role as AiRole, ThinkingLevel, Usage};
use pi_tools::ToolRegistry;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use super::definition::{AgentDefinition, SpawnsRule};

/// One unit of work passed to a subagent. Mirrors the JSON shape in the
/// `task` tool input schema.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TaskInput {
    pub id: String,
    #[serde(default)]
    pub description: Option<String>,
    pub assignment: String,
}

/// Result of one subagent run, as exposed to the parent.
#[derive(Debug, Clone, Serialize)]
pub struct TaskOutcome {
    pub id: String,
    pub agent: String,
    pub success: bool,
    pub model_output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured: Option<serde_json::Value>,
    pub session_file: PathBuf,
    pub duration_ms: u64,
    pub tokens: u64,
    pub aborted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl TaskOutcome {
    fn from_error(id: String, agent: String, msg: String) -> Self {
        Self {
            id,
            agent,
            success: false,
            model_output: format!("ERROR: {msg}"),
            structured: None,
            session_file: PathBuf::new(),
            duration_ms: 0,
            tokens: 0,
            aborted: false,
            error: Some(msg),
        }
    }
}

/// Aggregated batch result.
#[derive(Debug, Clone, Serialize)]
pub struct TaskBatchResult {
    pub agent: String,
    pub total_duration_ms: u64,
    pub usage: Usage,
    pub results: Vec<TaskOutcome>,
}

#[derive(Debug, thiserror::Error)]
pub enum ExecutorError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("runtime: {0}")]
    Runtime(String),
    #[error("spawn-rule violation: agent `{parent}` may not spawn `{child}`")]
    SpawnDenied { parent: String, child: String },
}

/// Resolve `(provider, model)` for a subagent, applying the precedence
/// from RFD 0005: per-task `Settings::task::agent_models` > agent's
/// `model:` field > parent's currently active model.
fn resolve_subagent_model(parent_cfg: &RuntimeConfig, agent: &AgentDefinition) -> (String, String) {
    let s = &parent_cfg.settings;
    let raw = s
        .task
        .agent_models
        .get(&agent.name)
        .cloned()
        .or_else(|| agent.model.clone());

    let target = match raw {
        Some(t) => t,
        None => return (s.provider.clone(), s.model.clone()),
    };

    // Role alias (`smol`, `slow`, …).
    if let Some(role) = pi_agent_core::settings::Role::parse(&target) {
        let resolved = s.roles.resolve(role, &s.model).to_string();
        if let Some((p, m)) = resolved.split_once('/') {
            return (p.to_string(), m.to_string());
        }
        return (s.provider.clone(), resolved);
    }

    // `provider/model`.
    if let Some((p, m)) = target.split_once('/') {
        return (p.to_string(), m.to_string());
    }

    // Bare model id — keep parent's provider.
    (s.provider.clone(), target)
}

/// Translate the optional `thinking:` frontmatter string into a
/// `ThinkingSetting`. Unknown values fall back to `Off` to match the
/// rest of the codebase's "loose parsing" convention.
fn parse_thinking(s: &str) -> ThinkingLevel {
    match s.trim().to_lowercase().as_str() {
        "low" => ThinkingLevel::Low,
        "medium" => ThinkingLevel::Medium,
        "high" => ThinkingLevel::High,
        "xhigh" | "x-high" => ThinkingLevel::XHigh,
        _ => ThinkingLevel::Off,
    }
}

/// Synthesise the subagent's first user message: the parent's most
/// recent user-text + the per-task assignment, wrapped in a tag the
/// subagent's prompt can refer to.
async fn synth_first_message(
    parent_session: &AgentSession,
    _agent: &AgentDefinition,
    shared_context: Option<&str>,
    task: &TaskInput,
) -> String {
    let last_user = parent_session
        .messages()
        .await
        .iter()
        .rev()
        .find(|m| matches!(m.role, AiRole::User))
        .map(|m| m.text())
        .unwrap_or_default();

    let mut s = String::new();
    s.push_str("<parent_context>\n");
    s.push_str(last_user.trim());
    s.push_str("\n</parent_context>\n\n");
    if let Some(ctx) = shared_context {
        if !ctx.trim().is_empty() {
            s.push_str("<shared_context>\n");
            s.push_str(ctx.trim());
            s.push_str("\n</shared_context>\n\n");
        }
    }
    s.push_str(&format!("## Task `{}`", task.id));
    if let Some(d) = &task.description {
        if !d.is_empty() {
            s.push_str(" — ");
            s.push_str(d);
        }
    }
    s.push_str("\n\n");
    s.push_str(&task.assignment);
    s
}

/// Build the subagent's filtered tool registry. Implements the rule
/// from the RFD: parent's tools, optionally restricted to
/// `agent.tools`, with the `task` tool itself toggled on/off based on
/// `agent.spawns`.
fn build_child_tools(parent_tools: &ToolRegistry, agent: &AgentDefinition) -> ToolRegistry {
    let mut tools = parent_tools.clone();
    if !agent.tools.is_empty() {
        // Always preserve `task` if the parent's allowlist included it
        // — `keep_only` only retains names that appear in the list.
        tools.keep_only(&agent.tools);
    }
    if agent.spawns.is_some() {
        // Re-register `task` so this subagent can fan out further.
        tools.register(Arc::new(super::tool::TaskTool::new()));
    } else {
        tools.unregister("task");
    }
    // RFD: always strip `web_search` for sub-subagents to prevent
    // runaway spend.
    tools.unregister("web_search");
    tools
}

/// Run one subagent task end-to-end.
pub async fn run_one(
    parent_handle: &super::tool::ParentHandle,
    agent: &AgentDefinition,
    shared_context: Option<&str>,
    task: &TaskInput,
) -> Result<TaskOutcome, ExecutorError> {
    let parent_cfg = &parent_handle.parent_cfg;
    let parent_session = &parent_handle.parent_session;
    let started = Instant::now();

    // 1. Model.
    let (child_provider, child_model) = resolve_subagent_model(parent_cfg, agent);

    // 2. Tools.
    let child_tools = build_child_tools(&parent_cfg.tools, agent);

    // 3. Settings: clone parent, override model + thinking.
    let mut child_settings: Settings = parent_cfg.settings.clone();
    child_settings.provider = child_provider.clone();
    child_settings.model = child_model.clone();
    if let Some(t) = &agent.thinking {
        child_settings.thinking = match parse_thinking(t) {
            ThinkingLevel::Off => pi_agent_core::settings::ThinkingSetting::Off,
            ThinkingLevel::Low => pi_agent_core::settings::ThinkingSetting::Low,
            ThinkingLevel::Medium => pi_agent_core::settings::ThinkingSetting::Medium,
            ThinkingLevel::High => pi_agent_core::settings::ThinkingSetting::High,
            ThinkingLevel::XHigh => pi_agent_core::settings::ThinkingSetting::XHigh,
        };
    }

    // 4. SessionManager: a fresh clone of the parent's manager — same
    //    base_dir + cwd, but `create()` will mint a new id. We record
    //    the parent's session id by appending a Meta entry that points
    //    at it (via `clone_branch`-style linkage is overkill for v1;
    //    we just store it on the outcome).
    let child_session_mgr = parent_cfg.session_manager.clone();

    // 5. Synthesised first message.
    let user_msg = synth_first_message(parent_session, agent, shared_context, task).await;

    // 6. Build child RuntimeConfig.
    let child_cfg = RuntimeConfig {
        session_manager: child_session_mgr,
        auth_storage: parent_cfg.auth_storage.clone(),
        model_registry: parent_cfg.model_registry.clone(),
        tools: child_tools,
        settings: child_settings,
        system_prompt: agent.system_prompt.clone(),
        context_files: Vec::new(),
        cwd: parent_cfg.cwd.clone(),
        provider_factory: parent_cfg.provider_factory.clone(),
        tool_gate: parent_cfg.tool_gate.clone(),
        gate_ask_is_approve: parent_cfg.gate_ask_is_approve,
        stream_interceptor: parent_cfg.stream_interceptor.clone(),
        sandbox_provider: parent_cfg.sandbox_provider.clone(),
    };

    // 7. Run a single prompt loop, with an updated ParentHandle scoped
    //    to the child so any nested `task` invocations see THIS agent
    //    as the spawn-rule authority and the new session as parent.
    let runtime = AgentSessionRuntime::new(child_cfg);
    let session = runtime
        .create_session(None)
        .map_err(|e| ExecutorError::Runtime(e.to_string()))?;
    let session_id = session.id().to_string();
    let session_meta = parent_cfg
        .session_manager
        .meta(&session_id)
        .map(|m| m.path)
        .unwrap_or_default();

    let child_handle = super::tool::ParentHandle {
        parent_cfg: Arc::new(runtime.config().clone()),
        parent_session: session.clone(),
        current_agent: Some(agent.clone()),
    };

    let prompt_result = super::tool::with_runtime(child_handle, session.prompt(user_msg)).await;
    match prompt_result {
        Ok(final_msg) => {
            let text = final_msg.text();
            Ok(TaskOutcome {
                id: task.id.clone(),
                agent: agent.name.clone(),
                success: true,
                model_output: text,
                structured: None,
                session_file: session_meta,
                duration_ms: started.elapsed().as_millis() as u64,
                tokens: 0,
                aborted: false,
                error: None,
            })
        }
        Err(e) => Ok(TaskOutcome {
            id: task.id.clone(),
            agent: agent.name.clone(),
            success: false,
            model_output: format!("ERROR: {e}"),
            structured: None,
            session_file: session_meta,
            duration_ms: started.elapsed().as_millis() as u64,
            tokens: 0,
            aborted: matches!(e, pi_agent_core::runtime::RuntimeError::Aborted),
            error: Some(e.to_string()),
        }),
    }
}

/// Run a batch of tasks concurrently with a fan-out cap.
pub async fn run_batch(
    parent_handle: &super::tool::ParentHandle,
    agent: &AgentDefinition,
    shared_context: Option<&str>,
    tasks: Vec<TaskInput>,
    max_concurrency: usize,
) -> TaskBatchResult {
    let started = Instant::now();
    let agent_name = agent.name.clone();
    let outcomes: Vec<TaskOutcome> = stream::iter(tasks)
        .map(|t| async move {
            match run_one(parent_handle, agent, shared_context, &t).await {
                Ok(o) => o,
                Err(e) => TaskOutcome::from_error(t.id.clone(), agent.name.clone(), e.to_string()),
            }
        })
        .buffer_unordered(max_concurrency.max(1))
        .collect()
        .await;

    TaskBatchResult {
        agent: agent_name,
        total_duration_ms: started.elapsed().as_millis() as u64,
        usage: Usage::default(),
        results: outcomes,
    }
}

/// Helper for the `task` tool: enforce `spawns` rule against the
/// currently-active agent. v1 always allows top-level `task` calls
/// (parent has no spawn restriction); only nested calls — i.e. a
/// subagent calling `task` again — are restricted.
pub fn check_spawn_allowed(
    parent_agent: Option<&AgentDefinition>,
    child_name: &str,
) -> Result<(), ExecutorError> {
    let Some(p) = parent_agent else {
        return Ok(());
    };
    match &p.spawns {
        None => Err(ExecutorError::SpawnDenied {
            parent: p.name.clone(),
            child: child_name.to_string(),
        }),
        Some(rule) => {
            if rule.allows(child_name) {
                Ok(())
            } else {
                Err(ExecutorError::SpawnDenied {
                    parent: p.name.clone(),
                    child: child_name.to_string(),
                })
            }
        }
    }
}

// Keep the SpawnsRule import used.
#[allow(dead_code)]
fn _spawn_rule_typecheck(r: &SpawnsRule) -> bool {
    matches!(r, SpawnsRule::All | SpawnsRule::Named(_))
}
