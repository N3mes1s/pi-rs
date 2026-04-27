//! [`Matcher`] consumes a stream of assistant text deltas and reports
//! when a TTSR rule fires. State: a `HashSet<rule_name>` of already-fired
//! rules so each fires at most once per session.
//!
//! The matcher is *streaming-aware*: it accumulates an internal buffer
//! of seen text so a regex like `\bplan\b` still fires when the trigger
//! word arrives across two delta boundaries (`"pl"`, `"an"`).

use std::collections::HashSet;

use super::rule::{Rule, RuleSet};

/// Outcome of feeding a delta to the [`Matcher`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchResult {
    /// No rule matched.
    None,
    /// `rule_index` (in [`RuleSet::rules`]) just fired. The caller
    /// should abort the in-flight assistant turn, append the rendered
    /// reminder as a user message, and re-issue the turn.
    Fired { rule_index: usize },
}

/// Streaming TTSR matcher. Cheap to construct; clone borrows the
/// underlying [`RuleSet`].
pub struct Matcher<'a> {
    rules: &'a RuleSet,
    /// Accumulated delta text. Trimmed in [`Matcher::feed`] so it can't
    /// grow without bound across long turns.
    buffer: String,
    fired: HashSet<String>,
}

const MAX_BUFFER: usize = 8 * 1024;

impl<'a> Matcher<'a> {
    pub fn new(rules: &'a RuleSet) -> Self {
        Self {
            rules,
            buffer: String::new(),
            fired: HashSet::new(),
        }
    }

    /// Reset the per-turn buffer (call this at the start of each new
    /// assistant turn). Does NOT clear `fired` — that's session-scoped.
    pub fn turn_reset(&mut self) {
        self.buffer.clear();
    }

    /// Forget all fired rules (test-only / "/ttsr clear" reset).
    pub fn clear_fired(&mut self) {
        self.fired.clear();
    }

    /// Has `rule.name` already fired in this session?
    pub fn has_fired(&self, rule: &Rule) -> bool {
        self.fired.contains(&rule.name)
    }

    /// Feed one delta. Returns `Fired` for the *first* rule that matches
    /// against the accumulated buffer and hasn't fired yet. Once
    /// returned, the rule is recorded in `fired`.
    pub fn feed(&mut self, delta: &str) -> MatchResult {
        self.buffer.push_str(delta);
        if self.buffer.len() > MAX_BUFFER {
            // Keep the tail. Rules with long-distance lookbacks won't
            // survive this; that's acceptable for streaming.
            let cut = self.buffer.len() - MAX_BUFFER;
            self.buffer = self.buffer.split_off(cut);
        }
        for (i, (rule, regex)) in self.rules.rules().iter().enumerate() {
            if self.fired.contains(&rule.name) {
                continue;
            }
            if regex.is_match(&self.buffer) {
                self.fired.insert(rule.name.clone());
                return MatchResult::Fired { rule_index: i };
            }
        }
        MatchResult::None
    }
}
