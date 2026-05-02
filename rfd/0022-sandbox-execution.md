# RFD 0022 — Sandbox Execution for Tool Decisions

- **Status:** Implemented (v1.0)
- **Author:** pi-rs maintainers
- **Created:** 2026-01-XX
- **Implemented:** 2026-04-30

## Summary

Separate the LLM's *decisions* (what tool to run: name + input JSON) from the *execution* of those decisions (the actual side effects: disk, shell, network). Today in pi-rs, a `ToolCall` flows directly from the LLM into an in-process `tool.invoke()` call; we want decisions to cross an isolation boundary so:

1. **Sandboxing**: tool execution runs in a provider-controlled sandbox (local process, container, VM, or remote sandbox service) rather than inline in the agent.
2. **Observability**: every decision + result is observable end-to-end (timing, exit status, blast radius, diffs) via a new `SessionEntryKind::SandboxAction` telemetry variant.
3. **Clean context**: the LLM context stays pristine while the sandbox can be ephemeral or persistent and provider-portable.
4. **Auto-approve integration**: the existing `ToolGate` approval flow is preserved; blocked tools never reach the sandbox.

## Background

### Existing architecture

- **Tool invocation** (crates/pi-tools/src/lib.rs): `Tool::invoke()` receives input JSON, runs synchronously or async inline, returns `ToolResult`.
- **Runtime integration** (`crates/pi-agent-core/src/runtime.rs` ~700+): tool calls execute in the agent loop; a `ToolGate` (if present) blocks before invoking.
- **Auto-approval** (crates/pi-coding-agent/src/auto_approve/): policy + optional judge model decide approve/reject/ask before any tool runs.
- **Telemetry** (crates/pi-stats/): `SessionEntryKind` variants flow JSONL → SQLite; routing decisions (RFD 0020) became a first-class stats category.
- **Isolation precedent** (RFD 0005 + 0006): `task` subagent tool spawns a full child runtime in a worktree; decisions cross that boundary but execution stays in-process.

### Inspiration: wromm

The sister project `~/code/wromm` orchestrates sandboxes across providers (Docker, E2B, Sprites, etc.). Key ideas:

1. **Provider abstraction** (`src/provider.rs`): unify sandbox lifecycle (provision → exec → stop/destroy) across implementations.
2. **Capability traits** (`src/capability.rs`): explicit `Snapshot`, `Suspend`, `ExportState`, etc.; providers expose what they support.
3. **Portable spec** (`src/spec.rs`): declare environment intent (packages, runtimes, env vars, ports) once, replay on target.
4. **Environment as data** (`src/state.rs`): deltas capture "what changed" so migration is feasible.

For pi-rs, we adopt the mental model but **stay minimal**: a single `SandboxProvider` trait with methods for `prepare`, `execute_tool`, and `cleanup`. Implementations include:

- `LocalProcessProvider`: fork a subprocess in a dedicated tmpdir (default, immediate).
- Future: `ContainerProvider` (Docker), `VmProvider` (E2B / AWS Sandbox), etc.

### What we're *not* doing

- **Pod/volume/image management**: defer to wormm or user-supplied custom provider.
- **State export/import** between provider invocations: that's a future versioning story.
- **Decision retry / rollback**: if a tool fails, the LLM sees the error and decides; no implicit recovery.
- **Tool standardization**: tools keep their current shape (input JSON, output `ToolResult`); sandboxing is orthogonal.

## Proposal

### 1. Core trait: `SandboxProvider`

New crate `crates/pi-sandbox/` with three modules: `provider.rs`, `local.rs`, `registry.rs`.

**File layout:**
```
crates/pi-sandbox/
├── Cargo.toml
├── src/
│   ├── lib.rs
│   ├── provider.rs          # SandboxProvider trait
│   ├── local.rs             # LocalProcessProvider impl
│   └── registry.rs          # SandboxRegistry (for later: plugin system)
└── tests/
    ├── local_process_sandbox.rs
    └── provider_dispatch.rs
```

**Trait definition** (`provider.rs`):

```rust
/// Sandbox decision + result telemetry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxAction {
    pub action_id: String,
    pub sandbox_provider: String,           // "local" | "docker" | "e2b" | ...
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    pub started_at: i64,                    // timestamp_ms
    pub finished_at: Option<i64>,
    pub duration_ms: Option<u64>,
    pub exit_status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub is_error: bool,
}

#[async_trait]
pub trait SandboxProvider: Send + Sync {
    /// Human-readable provider name (e.g. "local-process").
    fn name(&self) -> &'static str;

    /// Execute a tool decision in the sandbox.
    /// - `ctx`: the ToolContext (cwd, max output bytes, etc.)
    /// - `tool_name`, `tool_input`: the decision to execute
    /// Returns: `(stdout, stderr, exit_status)` tuple
    async fn execute_tool(
        &self,
        ctx: &ToolContext,
        tool_name: &str,
        tool_input: &serde_json::Value,
    ) -> Result<SandboxExecution, SandboxError>;

    /// Optional: clean up any persistent state (e.g. stop a container, close a session).
    /// Called once per session, or on explicit user request.
    async fn cleanup(&self) -> Result<(), SandboxError> {
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct SandboxExecution {
    pub stdout: String,
    pub stderr: String,
    pub exit_status: i32,
}

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("sandbox provider error: {0}")]
    Provider(String),
    #[error("tool not found: {0}")]
    ToolNotFound(String),
    #[error("timeout")]
    Timeout,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
```

### 2. Local process provider

**`local.rs`**: fork a subprocess per tool invocation.

```rust
pub struct LocalProcessProvider {
    timeout: Duration,
}

#[async_trait]
impl SandboxProvider for LocalProcessProvider {
    fn name(&self) -> &'static str {
        "local-process"
    }

    async fn execute_tool(
        &self,
        ctx: &ToolContext,
        tool_name: &str,
        tool_input: &serde_json::Value,
    ) -> Result<SandboxExecution, SandboxError> {
        // Look up the tool in the (embedded) ToolRegistry
        let tool = BUILTIN_REGISTRY.get(tool_name)
            .ok_or(SandboxError::ToolNotFound(tool_name.into()))?;

        // Invoke the tool (still in-process, but isolated via tmpdir).
        // In a future version, this could spawn a subprocess instead.
        let result = tool.invoke(ctx, "<sandboxed>", tool_input.clone()).await?;

        Ok(SandboxExecution {
            stdout: result.model_output,
            stderr: String::new(),
            exit_status: if result.is_error { 1 } else { 0 },
        })
    }
}
```

(MVP: the LocalProcessProvider still calls `tool.invoke()` inline, just with a dedicated tmpdir context. Future: spawn an actual subprocess with serialized tool input.)

### 3. Runtime integration

**New hook in `RuntimeConfig`** (crates/pi-agent-core/src/runtime.rs):

```rust
pub struct RuntimeConfig {
    // ... existing fields ...
    
    /// Optional sandbox provider. When Some, tool decisions execute
    /// through the sandbox boundary instead of inline. When None,
    /// legacy inline invocation applies.
    pub sandbox_provider: Option<Arc<dyn SandboxProvider>>,
}
```

**Tool invocation loop** (crates/pi-agent-core/src/runtime.rs, in `run_loop()`):

Replace the current:
```rust
match tool.invoke(&tool_ctx, &call.id, call.input.clone()).await { ... }
```

with conditional routing:

```rust
let (result, sandbox_action) = if let Some(provider) = &self.cfg.sandbox_provider {
    // Decision crosses boundary → sandbox execution.
    self.execute_via_sandbox(provider, &tool, &call, &tool_ctx).await?
} else {
    // Legacy: inline execution.
    (self.execute_inline(&tool, &call, &tool_ctx).await?, None)
};

// Emit sandbox action telemetry if present.
if let Some(action) = sandbox_action {
    let _ = self.cfg.session_manager.append(
        &self.id,
        SessionEntryKind::SandboxAction { action },
    );
}

// Emit standard ToolResult regardless of execution path.
let _ = self.cfg.session_manager.append(
    &self.id,
    SessionEntryKind::ToolResult { result },
);
```

### 4. Session telemetry: `SessionEntryKind::SandboxAction`

**New variant in crates/pi-agent-core/src/session.rs:**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionEntryKind {
    // ... existing variants ...
    
    /// Records a tool execution in a sandbox. Emitted *before* the
    /// corresponding ToolResult so the LLM sees both decision and outcome.
    /// Consumed by pi-stats for per-provider breakdown, latency analysis,
    /// error rate tracking.
    SandboxAction {
        provider: String,           // "local-process", "docker", "e2b", ...
        tool_name: String,
        duration_ms: u64,
        exit_status: i32,
        is_error: bool,
    },
}
```

(Note: we emit a *compact* telemetry row, not the full `SandboxAction` struct, to keep JSONL lines scannable and stats schema simple.)

### 5. Stats schema and aggregation

**New table in crates/pi-stats/src/schema.rs:**

```sql
CREATE TABLE IF NOT EXISTS sandbox_actions (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    session_file    TEXT    NOT NULL,
    entry_id        TEXT    NOT NULL,
    folder          TEXT    NOT NULL,
    timestamp_ms    INTEGER NOT NULL,
    provider        TEXT    NOT NULL,
    tool_name       TEXT    NOT NULL,
    duration_ms     INTEGER NOT NULL,
    exit_status     INTEGER NOT NULL,
    is_error        INTEGER NOT NULL DEFAULT 0,
    UNIQUE (session_file, entry_id)
);
CREATE INDEX IF NOT EXISTS idx_sandbox_provider    ON sandbox_actions(provider);
CREATE INDEX IF NOT EXISTS idx_sandbox_tool_name   ON sandbox_actions(tool_name);
CREATE INDEX IF NOT EXISTS idx_sandbox_ts          ON sandbox_actions(timestamp_ms);
```

**New aggregator in crates/pi-stats/src/aggregate.rs:**

```rust
#[derive(Debug, Clone, Serialize)]
pub struct SandboxStats {
    pub provider: String,
    pub executions: u64,
    pub errors: u64,
    pub error_rate: f64,
    pub avg_duration_ms: f64,
}

pub fn by_sandbox_provider(c: &Connection) -> rusqlite::Result<Vec<SandboxStats>> {
    let mut stmt = c.prepare(
        "SELECT provider,
                COUNT(*),
                SUM(CASE WHEN is_error=1 THEN 1 ELSE 0 END),
                AVG(duration_ms)
           FROM sandbox_actions
          GROUP BY provider
          ORDER BY COUNT(*) DESC"
    )?;
    // ... collect and format results ...
}
```

### 6. CLI verb: `pi --stats sandbox-actions`

**New case in crates/pi-stats/src/cli.rs:**

```rust
#[derive(Debug, Clone, Copy)]
pub enum StatsVerb {
    Server,
    Sync,
    Json,
    RouteSavings,
    SandboxActions,  // NEW
}

impl StatsVerb {
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        match s {
            // ... existing ...
            "sandbox-actions" | "sandbox" => Ok(Self::SandboxActions),
            // ...
        }
    }
}

// In run():
StatsVerb::SandboxActions => {
    let mut stats = aggregate::by_sandbox_provider(&conn)?;
    stats.sort_by(|a, b| a.provider.cmp(&b.provider));
    
    println!("{:<15} {:<12} {:<10} {:<12} {:<10}",
        "provider", "executions", "errors", "error_rate", "avg_ms");
    
    for row in &stats {
        let rate = (row.errors as f64 / row.executions as f64) * 100.0;
        println!("{:<15} {:<12} {:<10} {:<12.1}% {:<10.2}",
            row.provider, row.executions, row.errors, rate, row.avg_duration_ms);
    }
    Ok(())
}
```

### 7. Auto-approve interaction

**No changes required**: the existing `ToolGate::approve()` call happens *before* the sandbox routing decision. A rejected tool never reaches the sandbox provider.

```rust
if let Some(gate) = &self.cfg.tool_gate {
    let outcome = gate.approve(&call.name, &call.input).await;
    if outcome != ToolGateOutcome::Approve {
        // Block the call (standard ToolResult with is_error=true)
        // Do NOT route to sandbox.
        continue;
    }
}

// Only approved calls reach the sandbox (or inline execution).
let (result, sandbox_action) = if let Some(provider) = &self.cfg.sandbox_provider {
    self.execute_via_sandbox(provider, &tool, &call, &tool_ctx).await?
} else {
    ...
}
```

## Implementation schedule

### Phase 1: Core (testing at each commit)
1. **[COMMIT 1]** Create `crates/pi-sandbox/Cargo.toml`, stub `lib.rs`, define `SandboxProvider` trait.
2. **[COMMIT 2]** Implement `LocalProcessProvider` with tests.
3. **[COMMIT 3]** Wire `SandboxProvider` into `RuntimeConfig` + stub the routing logic.

### Phase 2: Telemetry (test: JSONL generation + stats ingestion)
4. **[COMMIT 4]** Add `SessionEntryKind::SandboxAction` variant.
5. **[COMMIT 5]** Emit sandbox action entries from the runtime.
6. **[COMMIT 6]** Add schema table + ingest logic to pi-stats.

### Phase 3: Observability (test: end-to-end CLI)
7. **[COMMIT 7]** Add `aggregate::by_sandbox_provider()` + new `SandboxActions` stats verb.
8. **[COMMIT 8]** Wire `pi --stats sandbox-actions` in CLI.

### Phase 4: Dogfood + validation
9. **[INTEGRATION]** Run a session with `--sandbox-provider local-process`; confirm JSONL entries are emitted.
10. **[VALIDATION]** Run `pi --stats sandbox-actions` and confirm non-empty output.

## Out of scope / deferred

- **Remote sandbox providers** (Docker, E2B, Sprites, etc.): future RFDs will expand the provider registry.
- **Subprocess sandboxing** (actual process isolation): MVP uses in-process tools with dedicated tmpdir. Subprocess spawning is a future optimization.
- **Tool result caching / memoization** across invocations.
- **Blast radius analysis** (diffs, fs mutations): future enhancement once we have persistent sandboxes.
- **Config file for sandbox settings** (e.g. `~/.pi/agent/sandbox.json`): start with defaults, CLI flags only.

## Revision history

- **v1.0 (2026-04-30):** Initial implementation landed. `pi-sandbox` crate
  exposes `SandboxProvider` + `LocalProcessProvider`. `RuntimeConfig` gains
  `sandbox_provider: Option<Arc<dyn SandboxProvider>>`; the dispatch site in
  `runtime.rs::run_loop()` routes through the provider when set.
  `SessionEntryKind::SandboxAction` records compact telemetry (provider,
  tool_name, duration_ms, exit_status, is_error) emitted before the matching
  `ToolResult`. `pi-stats` adds the `sandbox_actions` table, ingest path,
  `aggregate::by_sandbox_provider()`, and the `sandbox-actions` (alias
  `sandbox`) verb on `pi --stats`.

## Open questions

1. **Should `SandboxProvider` be pluggable via Rust trait objects, or should we build out a dynamic dispatch layer like wromm's factory?** 
   - *Decision*: Trait objects for now (Arc<dyn SandboxProvider>); future extension via a registry pattern if needed.

2. **Do we emit the full `SandboxAction` struct or a compact telemetry row?**
   - *Decision*: Compact row (provider, tool_name, duration_ms, exit_status, is_error); the full ToolResult is captured separately.

3. **Should `LocalProcessProvider` actually fork a subprocess, or invoke tools in-process with a tmpdir?**
   - *Decision*: MVP invokes in-process with a dedicated tmpdir context to minimize complexity. Future commits can fork subprocesses once the trait stabilizes.

4. **Do we require users to opt-in to sandbox execution, or should it be enabled by default?**
   - *Decision*: Opt-in: `RuntimeConfig::sandbox_provider` defaults to `None` (legacy inline invocation). CLI users can enable with `--sandbox-provider local` (future).

## Testing strategy

### Unit tests (per crate)

- `crates/pi-sandbox/tests/local_process_sandbox.rs`: mock ToolRegistry, test execute_tool() with various inputs.
- `crates/pi-stats/tests/sandbox_action_ingest.rs`: JSONL fixture with SandboxAction entries; verify schema + aggregation.

### Integration tests

- Session runs with sandbox enabled; confirm JSONL contains both SandboxAction and ToolResult per tool call.
- `pi --stats sandbox-actions` against a populated stats.db; verify per-provider breakdown.

### Dogfood

- Run this feature's own development on itself (inner loop with `--sandbox-provider local-process`).
- Generate sandbox-action telemetry and verify `pi --stats sandbox-actions` output is non-empty and sensible.

## References

- **RFD 0005**: Subagents and the task tool (context isolation precedent).
- **RFD 0006**: Worktree isolation for task execution.
- **RFD 0020**: Routing decisions telemetry (RoutingDecision session entry model).
- **wromm ARCHITECTURE.md**: Provider abstraction, capability traits, portable specs.
- **pi-ai Usage** (crates/pi-ai/src/cost.rs): token/cost roll-up pattern for telemetry.
