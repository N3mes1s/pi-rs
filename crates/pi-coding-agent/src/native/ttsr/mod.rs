//! TTSR — Time-Travelling Streamed Rules.
//!
//! User-defined rules live as Markdown files in `~/.pi/agent/ttsr/`.
//! Each rule has YAML frontmatter with a `ttsrTrigger` regex and a body.
//! When the regex matches an assistant text delta during streaming the
//! current generation is aborted and the rule body is injected as a
//! `<system_reminder>` user message before the assistant turn restarts.
//!
//! This crate ships:
//!
//! * [`Rule`] / [`RuleSet`] — frontmatter parser + filesystem loader.
//! * [`Matcher`] — running over a stream of text deltas, tracking
//!   already-fired rules so each fires at most once per session.
//! * The injection format helper [`render_reminder`].
//!
//! Wiring this module into the actual `pi-agent-core` run loop requires
//! a per-delta hook on `AgentSessionRuntime::stream_loop`; that's a
//! cross-crate refactor and is deferred. The current module is
//! complete-and-tested in isolation so the wiring step is mechanical.

pub mod matcher;
pub mod rule;

pub use matcher::{Matcher, MatchResult};
pub use rule::{render_reminder, Rule, RuleSet, default_dir};
