//! Glue: implement `pi_agent_core::ToolGate` for our two-layer
//! auto-approve module so the runtime can plug it into the agent loop.

use std::sync::Arc;

use async_trait::async_trait;
use pi_agent_core::{GateContext, ToolGate, ToolGateOutcome};

use super::{gate, Judge, Mode, Outcome, Policy};

/// Adapter exposing the auto_approve gate to the pi-agent-core runtime.
/// Cheap to clone (Arc).
#[derive(Clone)]
pub struct AutoApproveGate {
    inner: Arc<Inner>,
}

struct Inner {
    mode: Mode,
    policy: Policy,
    judge: Option<Judge>,
}

impl AutoApproveGate {
    pub fn new(mode: Mode, policy: Policy, judge: Option<Judge>) -> Self {
        Self {
            inner: Arc::new(Inner {
                mode,
                policy,
                judge,
            }),
        }
    }

    pub fn mode(&self) -> Mode {
        self.inner.mode
    }
}

#[async_trait]
impl ToolGate for AutoApproveGate {
    async fn approve(
        &self,
        _ctx: &GateContext,
        tool_name: &str,
        input: &serde_json::Value,
    ) -> ToolGateOutcome {
        // AutoApproveGate's policy file format pre-dates RFD 0027 H3
        // and does not key on session_id / turn_index / parent_session.
        // The context is accepted for trait-conformance; embedders
        // shipping their own ToolGate that needs scoping use ctx.
        let out = gate(
            self.inner.mode,
            &self.inner.policy,
            self.inner.judge.as_ref(),
            tool_name,
            input,
        )
        .await;
        match out {
            Outcome::Approve => ToolGateOutcome::Approve,
            Outcome::Reject(reason) => ToolGateOutcome::Reject(reason),
            Outcome::AskUser(reason) => ToolGateOutcome::AskUser(reason),
        }
    }
}
