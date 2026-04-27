//! Adapter that plugs the streaming [`Matcher`] into the
//! `pi-agent-core` runtime via the [`StreamInterceptor`] trait.
//!
//! On every assistant text delta, [`TtsrInterceptor::on_text_delta`]
//! feeds the matcher and — when a rule fires — returns
//! [`InterceptAction::AbortAndInject`] with the rendered
//! `<system_reminder>` body. The runtime then aborts the in-flight
//! turn, appends that body as a new user message, and re-issues the
//! assistant turn.
//!
//! The matcher's `fired` set is *session-scoped*: once a rule has
//! fired, it never fires again for the lifetime of this interceptor.
//! That gives us the one-shot-per-session semantics from upstream pi.

use std::sync::Arc;

use async_trait::async_trait;
use pi_agent_core::{InterceptAction, StreamInterceptor};
use tokio::sync::Mutex;

use super::matcher::{MatchResult, Matcher};
use super::rule::{render_reminder, RuleSet};

/// Stream interceptor that wraps a TTSR [`Matcher`] in a `Mutex` so it
/// can be shared across the runtime.
pub struct TtsrInterceptor {
    rules: Arc<RuleSet>,
    inner: Mutex<MatcherState>,
}

/// We don't store the borrowed `Matcher<'a>` directly because the trait
/// is `'static`. Instead we keep the parts the matcher cares about
/// (buffer + fired set) here and re-derive the borrow on each call.
struct MatcherState {
    buffer: String,
    fired: std::collections::HashSet<String>,
}

impl TtsrInterceptor {
    pub fn new(rules: Arc<RuleSet>) -> Self {
        Self {
            rules,
            inner: Mutex::new(MatcherState {
                buffer: String::new(),
                fired: std::collections::HashSet::new(),
            }),
        }
    }

    /// Test helper: report which rule names have fired so far.
    pub async fn fired_names(&self) -> Vec<String> {
        let g = self.inner.lock().await;
        let mut v: Vec<String> = g.fired.iter().cloned().collect();
        v.sort();
        v
    }
}

#[async_trait]
impl StreamInterceptor for TtsrInterceptor {
    async fn turn_start(&self) {
        let mut g = self.inner.lock().await;
        g.buffer.clear();
    }

    async fn on_text_delta(&self, text: &str) -> InterceptAction {
        let mut g = self.inner.lock().await;
        // Reconstruct a Matcher view backed by `g`'s state. This is
        // cheap; the matcher just consults the rules slice + state.
        let mut tmp_rules_view = Matcher::new(&self.rules);
        // Move our persisted state into the temporary matcher.
        std::mem::swap(&mut g.buffer, tmp_rules_view.buffer_mut());
        std::mem::swap(&mut g.fired, tmp_rules_view.fired_mut());
        let result = tmp_rules_view.feed(text);
        // Persist the updated state back.
        std::mem::swap(&mut g.buffer, tmp_rules_view.buffer_mut());
        std::mem::swap(&mut g.fired, tmp_rules_view.fired_mut());

        match result {
            MatchResult::None => InterceptAction::Continue,
            MatchResult::Fired { rule_index } => {
                let (rule, _) = &self.rules.rules()[rule_index];
                InterceptAction::AbortAndInject(render_reminder(rule))
            }
        }
    }
}
