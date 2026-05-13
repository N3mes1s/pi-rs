# RFD 0005 — Subagents and the `task` tool

- **Status:** Implemented
- **Author:** pi-rs maintainers
- **Created:** 2026-04-27
- **Implemented:** e846b43

## Summary

Add a `task` tool that lets the parent agent delegate a self-contained
unit of work to a *subagent*: a fresh runtime with its own message
history, its own model selection, its own (allowlisted) tool registry,
its own session JSONL, and an isolated working directory. Subagents
are defined as Markdown-with-frontmatter files in
`~/.pi/agent/agents/*.md` or `<repo>/.pi/agents/*.md`. The `task` tool
collapses any subagent's full transcript into a single `tool_result`
in the parent's stream so the parent's context window stays clean.

This is the first half of what oh-my-pi calls "subagents" (Claude Code
terminology: `Task` tool, "context isolation"). The second half —
running subagents inside a `git worktree` — is RFD 0006.

## Background

Pi-rs already has skills (`crates/pi-coding-agent/src/skills.rs`) but
they are read-only Markdown injected into the parent's prompt. Skills
are *prompt annotations*, not *executable contexts*. They can't pick a
different model, restrict tools, or run in parallel — the four traits
that make oh-my-pi's `task` tool useful.

The runtime we need to fork already exists: `pi_agent_core::Runtime`
(see `crates/pi-agent-core/src/runtime.rs:163`) accepts a
`RuntimeConfig` and produces an `AgentSession`. `AgentSession::prompt`
runs one user-turn loop with the full tool/auto-approve/streaming
machinery. We can spawn a child runtime per subagent task by cloning
the parent's `RuntimeConfig` with three substitutions: a new
`SessionManager`, a filtered `ToolRegistry`, and a model resolved via
`ModelRoles` (see `crates/pi-agent-core/src/settings.rs:132`).

References:
* Anthropic: [Subagents in the SDK](https://platform.claude.com/docs/en/agent-sdk/subagents),
  [Custom subagents](https://code.claude.com/docs/en/sub-agents).
* Oh-my-pi: `packages/coding-agent/src/task/{discovery,executor,index}.ts`.
* Claude Code: `Task` tool emits one `tool_result` regardless of how
  many turns the subagent took — exactly the contract this RFD adopts.

## Proposal

### File layout

```
crates/pi-coding-agent/src/native/task/
├── mod.rs              # pub re-exports, registers TaskTool in startup
├── discovery.rs        # walk dirs, parse frontmatter → AgentDefinition
├── definition.rs       # AgentDefinition struct + serde
├── executor.rs         # spawn_subagent(): clones RuntimeConfig, runs prompt
├── tool.rs             # TaskTool: impl pi_tools::Tool
└── tests/
    ├── discovery.rs
    ├── definition_parse.rs
    └── executor_stub.rs
```

### Subagent definition format

A subagent is a Markdown file with YAML frontmatter. Body = system
prompt. Loaded by `discovery::load_all()` from (later wins):

1. Bundled (compiled into the binary via `include_dir!`).
2. `~/.pi/agent/agents/*.md`
3. `<repo>/.pi/agents/*.md`

```markdown
---
name: code-reviewer
description: Reviews a diff for correctness, security, and style.
tools: [read, grep, find, bash, lsp]
spawns: explore                # or "*", or omit (= no further spawning)
model: pi/slow                 # role alias OR concrete provider/model
thinking: medium               # off | low | medium | high
output:                         # JTD-shape declarative schema, optional
  properties:
    overall_correctness: { enum: [correct, incorrect] }
    notes: { type: string }
---
You are a senior reviewer. Read the diff at $DIFF_PATH and produce a
verdict using only the read-only tools available to you.
```

```rust
// crates/pi-coding-agent/src/native/task/definition.rs
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentDefinition {
    pub name: String,
    pub description: String,
    /// Body of the markdown file after the frontmatter delimiter.
    #[serde(skip)]
    pub system_prompt: String,
    /// Allowlist. Empty/omitted = inherit parent registry verbatim.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Recursive spawn restriction. None = no nested task. Some("*")
    /// = unrestricted. Some(list) = only these names.
    #[serde(default)]
    pub spawns: Option<SpawnsRule>,
    /// Settings-style model spec. Falls back to parent's resolved
    /// model if the role doesn't resolve.
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub thinking: Option<String>,
    /// Required output schema (forwarded to provider as response_format
    /// for providers that support it; ignored otherwise). Out of scope
    /// for v1: pass through, don't validate.
    #[serde(default)]
    pub output: Option<serde_json::Value>,
    /// Discovery metadata.
    #[serde(skip)]
    pub source: AgentSource,
    #[serde(skip)]
    pub file_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub enum SpawnsRule { All, Named(Vec<String>) }

#[derive(Debug, Clone, Default)]
pub enum AgentSource { #[default] Bundled, User, Project }
```

The serde shape mirrors oh-my-pi's frontmatter exactly; the body is
parsed separately because we want `system_prompt` typed as `String`,
not a YAML string field.

### `task` tool input schema

```rust
// tool.rs::TaskTool::spec()
ToolSpec {
    name: "task".into(),
    description:
        "Delegate one or more units of work to a named subagent. Each \
         task runs in a fresh runtime with isolated context, optionally \
         a different model, and a (possibly restricted) tool set. \
         Returns a single result block per task — the subagent's own \
         transcript stays out of your context.".into(),
    input_schema: json!({
        "type": "object",
        "required": ["agent", "tasks"],
        "properties": {
            "agent":   { "type": "string", "description": "Subagent name." },
            "context": { "type": "string", "description": "Shared markdown \
              prepended to every task's first user message. Optional." },
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
            "isolated": { "type": "boolean", "default": false,
              "description": "If true, run inside a git worktree (RFD 0006)." }
        }
    }),
}
```

### Executor

```rust
// executor.rs
pub async fn run_one(
    parent: &Runtime,
    parent_session: &AgentSession,
    agent: &AgentDefinition,
    task: &TaskInput,
) -> Result<TaskOutcome, ExecutorError> {
    let parent_cfg = parent.config();

    // 1. Resolve model. Per-task settings override > agent.model >
    //    parent's currently active model > Settings::model.
    let model = resolve_subagent_model(parent_cfg, agent)?;

    // 2. Build the subagent's tool registry: parent's, filtered by
    //    `agent.tools` if present, with the `task` tool itself
    //    inserted only if `agent.spawns.is_some()`. Always strip
    //    `web_search` for sub-subagents to avoid runaway spend.
    let mut tools = parent_cfg.tools.clone();
    if !agent.tools.is_empty() {
        tools.keep_only(&agent.tools);
    }
    if matches!(agent.spawns, Some(_)) {
        tools.register(Arc::new(TaskTool::new()));
    } else {
        tools.unregister("task");
    }

    // 3. Fresh SessionManager rooted at the parent's session dir but
    //    with a new id (and parent_id linked back).
    let child_session_dir = parent_cfg.session_manager.dir().to_path_buf();
    let child_session_mgr = SessionManager::on_disk_with_parent(
        child_session_dir, parent_cfg.cwd.clone(), parent_session.id().into(),
    )?;

    // 4. Synthesize compact context.
    let user_msg = synth_first_message(parent_session, agent, task).await?;

    // 5. Build child RuntimeConfig (clone parent, swap the bits).
    let child_cfg = RuntimeConfig {
        session_manager: child_session_mgr,
        tools,
        settings:        with_model_and_thinking(parent_cfg.settings.clone(),
                                                 &model, agent.thinking.as_deref()),
        system_prompt:   agent.system_prompt.clone(),
        context_files:   Vec::new(),       // explicit fork point
        ..parent_cfg.clone_shareable()      // auth, registry, gate, …
    };

    // 6. Run a single prompt turn loop.
    let runtime = Runtime::new(child_cfg);
    let session = runtime.create_session(/* sender = */ None)?;
    let final_msg = session.prompt(user_msg).await?;
    Ok(TaskOutcome::from_message(task.id.clone(), final_msg, &session))
}
```

`Runtime::config()` and `parent_cfg.clone_shareable()` are new helpers
the RFD adds; they do field-by-field cloning of `RuntimeConfig`,
deliberately *not* `derive(Clone)` because some fields (the
`SessionManager`) we want to swap.

### Context fork

The subagent does **not** receive the parent's full transcript. It
receives a synthesised compact summary plus the per-task assignment.
This is the same call Claude Code makes (its run_task() seeds
sub_messages with only the task prompt; the parent's history is
inaccessible).

```rust
async fn synth_first_message(
    parent: &AgentSession,
    agent: &AgentDefinition,
    task: &TaskInput,
) -> Result<String, ExecutorError> {
    // For v1, we keep this dumb: the parent's *current* user prompt
    // (plus any shared `context` from the task tool input) plus the
    // assignment. A future RFD ("smart context fork") can swap this
    // for an LLM-summarised digest.
    let last_user = parent.messages().await.iter().rev()
        .find_map(|m| match m.role {
            Role::User => first_text(&m.content),
            _ => None,
        }).unwrap_or_default();
    Ok(format!(
        "<parent_context>\n{}\n</parent_context>\n\n## Task `{}` — {}\n\n{}",
        last_user.trim(),
        task.id,
        task.description.as_deref().unwrap_or(""),
        task.assignment,
    ))
}
```

This is the smallest thing that works. The "real" context fork (an
LLM-summarised digest) lives behind a follow-up RFD so we can ship the
plumbing without blocking on summarisation quality.

### Concurrency

```rust
pub async fn run_batch(
    parent: &Runtime,
    parent_session: &AgentSession,
    agent: &AgentDefinition,
    tasks: Vec<TaskInput>,
    max_concurrency: usize,           // default 5
) -> Vec<TaskOutcome> {
    use futures::{stream, StreamExt};
    stream::iter(tasks)
        .map(|t| run_one(parent, parent_session, agent, &t))
        .buffer_unordered(max_concurrency)
        .collect::<Vec<_>>().await
        .into_iter()
        .map(|r| r.unwrap_or_else(TaskOutcome::from_error))
        .collect()
}
```

Default `max_concurrency = 5` (matches oh-my-pi). Configurable via
`Settings::task::max_concurrency` (a new struct on `Settings`).

### Result shape

The parent only ever sees one `tool_result` per `task` invocation:

```rust
#[derive(Debug, Clone, Serialize)]
pub struct TaskBatchResult {
    pub agent: String,
    pub total_duration_ms: u64,
    pub usage: Usage,                    // sum across all subtasks
    pub results: Vec<TaskOutcome>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskOutcome {
    pub id: String,
    pub agent: String,
    pub success: bool,
    pub model_output: String,            // last assistant text
    pub structured: Option<serde_json::Value>,  // if `output` schema set
    pub session_file: PathBuf,           // for drill-down later
    pub duration_ms: u64,
    pub tokens: u64,
    pub aborted: bool,
}
```

`model_output` is concatenated into the parent's `tool_result.content`;
the rich payload sits in `display` so the TUI can offer a drill-down
panel. The subagent's full JSONL is written to disk and linked via
`parent_id` — `pi --resume` lands on the parent unchanged; `pi -r`
with the child id resumes the subagent's session.

### Settings

```rust
// crates/pi-agent-core/src/settings.rs
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TaskSettings {
    /// Default subagent fan-out cap.
    #[serde(default = "default_max_concurrency")]
    pub max_concurrency: usize,
    /// Per-agent overrides — agent name → model id/alias.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub agent_models: BTreeMap<String, String>,
}
fn default_max_concurrency() -> usize { 5 }

// Add `pub task: TaskSettings,` to `Settings`.
```

## Test plan

1. **`tests/discovery.rs`** — write a project agent and a user agent
   with the same name; assert project wins; assert that omitting
   `tools:` yields an `AgentDefinition` with `tools.is_empty()`.
2. **`tests/definition_parse.rs`** — round-trip the example
   `code-reviewer` markdown above; reject definitions with missing
   `name` / `description` (`deny_unknown_fields`).
3. **`tests/executor_stub.rs`** — run `executor::run_one` against a
   `Runtime` whose provider factory returns a stub provider that
   echoes `"OK: {task.id}"`. Assert exactly one `TaskOutcome` per
   input task and `parent_session.messages()` has *not* gained the
   subagent's intermediate turns.
4. **`tests/parent_context_isolated.rs`** — confirm that the `task`
   tool's effect on the parent's transcript is exactly one
   `tool_result` per call (not the subagent's full history).
5. **End-to-end smoke (gated on `ANTHROPIC_API_KEY`)** — load the
   bundled `code-reviewer` agent, fan two assignments out, assert
   each `TaskOutcome.success == true` and `usage.cost_usd > 0`.
6. **Recursive spawn restriction** — agent A (`spawns: ["B"]`) tries
   to launch agent C; assert the inner `task` invocation returns an
   error tool_result citing the spawn rule.

## Out of scope

- **LLM-summarised context fork** — v1 forks the parent's last user
  message verbatim. A "smart" fork that runs a smol-model digest pass
  is RFD 0014.
- **Worktree isolation** — `isolated: true` is parsed but is a no-op
  in v1 (logged as a warning). RFD 0006 implements it; the schema
  field exists today so the contract is stable.
- **Streaming subagent progress to the TUI** — v1 collapses the
  subagent's stream entirely. Live-relayed progress events are RFD
  0015.
- **MCP proxying** — oh-my-pi forwards parent MCP server connections
  into the subagent. We don't support MCP yet (RFD 0016 pulls that
  in). Until then, MCP tools are dropped from the child registry.
- **Output schema validation** — frontmatter `output:` is forwarded as
  `response_format` where supported; pi-rs does not validate against
  it. v2 will.

## Open questions

- **Auto-derive `spawns: ["explore"]` for any agent that lists
  `tools: [read, grep, find]`?** Tempting (most read-only review
  agents need to fan out), but "magic" rules harden quickly. Lean no
  for v1; revisit if we keep typing the same line.
- **Should the parent's auto-approve mode propagate to subagents, or
  do subagents always run `auto_policy` regardless?** Lean toward
  inherit (least surprise). Test the security implications first;
  audit the smol-model judge cost in `auto_judge` mode.
- **One session JSONL per task, or one per agent batch?** Lean per
  task. Per-batch loses the parent_id linkage that powers
  `pi --resume <child>`.
- **Should `task` be allowed to *itself* be the agent's only tool?**
  I.e. an "orchestrator" agent. Probably yes. Flag for review.
