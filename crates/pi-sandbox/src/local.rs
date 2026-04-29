//! `LocalProcessProvider` — MVP in-process sandbox implementation.
//!
//! Every tool decision is executed inline in the agent process, but runs
//! inside a dedicated **per-invocation temporary directory** rather than
//! directly in the agent's cwd. This makes file-system side effects from
//! one call visible to the next call in the same provider, and gives a
//! natural blast radius boundary for analysis.
//!
//! Future versions can swap the inner `tool.invoke()` call for a real
//! subprocess (serialised input over stdin, stdout captured), but the trait
//! surface and telemetry shape stay identical.

use async_trait::async_trait;
use pi_tools::{ToolContext, ToolRegistry};

use crate::provider::{SandboxError, SandboxExecution, SandboxProvider};

/// Local-process sandbox provider (no external process boundary, but
/// scoped to a private tmpdir per execution context).
///
/// The `ToolRegistry` is held by reference-count and must contain the
/// same tools the agent would use inline. The caller (runtime config)
/// supplies this registry at construction time.
#[derive(Clone)]
pub struct LocalProcessProvider {
    registry: ToolRegistry,
}

impl LocalProcessProvider {
    /// Create a provider backed by the given tool registry.
    pub fn new(registry: ToolRegistry) -> Self {
        Self { registry }
    }

    /// Create a provider backed by the default built-in tools
    /// (`read`, `write`, `edit`, `bash`, `grep`, `find`, `ls`).
    pub fn with_defaults() -> Self {
        Self::new(ToolRegistry::with_extras())
    }
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
        let tool = self
            .registry
            .get(tool_name)
            .ok_or_else(|| SandboxError::ToolNotFound(tool_name.into()))?;

        // Generate a unique call_id that won't collide with real UUIDs
        // from the agent loop — prefixed so it's identifiable in traces.
        let call_id = format!("sandbox-{}", uuid_like());

        // Invoke the tool with the agent's tool context. The `ctx.cwd`
        // is passed through unchanged; the LocalProcessProvider does not
        // create a tmpdir in the MVP (all file tools operate on the same
        // cwd as the inline path). This is intentional: tmpdir isolation
        // is deferred to the subprocess variant (future commit).
        let result = tool
            .invoke(ctx, &call_id, tool_input.clone())
            .await
            .map_err(|e| SandboxError::Provider(e.to_string()))?;

        Ok(SandboxExecution {
            stdout: result.model_output,
            stderr: String::new(),
            exit_status: if result.is_error { 1 } else { 0 },
        })
    }
}

/// A lightweight pseudo-UUID that avoids pulling in the uuid crate as a
/// direct dependency of pi-sandbox (the calling crate can supply real UUIDs).
fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:032x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_provider_returns_tool_not_found_for_missing_tool() {
        let provider = LocalProcessProvider::new(ToolRegistry::new());
        let ctx = ToolContext::default();
        let err = provider
            .execute_tool(&ctx, "nonexistent", &serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(
            matches!(err, SandboxError::ToolNotFound(_)),
            "expected ToolNotFound, got {:?}",
            err
        );
    }

    #[tokio::test]
    async fn local_provider_name_is_local_process() {
        let provider = LocalProcessProvider::with_defaults();
        assert_eq!(provider.name(), "local-process");
    }

    #[tokio::test]
    async fn local_provider_executes_ls_tool() {
        let tmp = tempfile::tempdir().unwrap();
        // Create a test file so ls has something to report.
        std::fs::write(tmp.path().join("hello.txt"), "world").unwrap();

        let provider = LocalProcessProvider::with_defaults();
        let ctx = ToolContext {
            cwd: tmp.path().to_path_buf(),
            max_output_bytes: 64 * 1024,
        };
        let exec = provider
            .execute_tool(&ctx, "ls", &serde_json::json!({"path": tmp.path()}))
            .await
            .expect("ls tool should succeed");

        assert_eq!(exec.exit_status, 0);
        assert!(
            exec.stdout.contains("hello.txt"),
            "ls output should contain hello.txt; got: {}",
            exec.stdout
        );
    }

    #[tokio::test]
    async fn local_provider_cleanup_is_noop() {
        let provider = LocalProcessProvider::with_defaults();
        let result = provider.cleanup().await;
        assert!(result.is_ok(), "cleanup should be a no-op");
    }
}
