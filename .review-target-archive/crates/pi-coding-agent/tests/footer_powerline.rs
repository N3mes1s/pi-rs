//! Tests for the powerline footer.
//!
//! The footer renders at the bottom of the TUI and shows model, cwd, git status,
//! token usage, cost, and context-window metrics separated by ▶ powerline arrows.

use pi_agent_core::RouteMode;
use pi_coding_agent::footer::GitStatus;
use pi_coding_agent::renderer::Transcript;
use pi_tui::{ColorSpec, NamedColor, Theme};
use std::collections::BTreeSet;
use std::path::Path;

fn theme() -> Theme {
    Theme {
        name: "t".into(),
        fg: ColorSpec::Named(NamedColor::White),
        bg: ColorSpec::Named(NamedColor::Reset),
        muted: ColorSpec::Named(NamedColor::DarkGrey),
        accent: ColorSpec::Named(NamedColor::Cyan),
        user: ColorSpec::Named(NamedColor::Cyan),
        assistant: ColorSpec::Named(NamedColor::Green),
        thinking: ColorSpec::Named(NamedColor::DarkGrey),
        tool: ColorSpec::Named(NamedColor::Yellow),
        error: ColorSpec::Named(NamedColor::Red),
    }
}

#[test]
fn footer_powerline_has_distinct_coloured_segments_when_256_colours_available() {
    let mut t = Transcript::default();
    t.usage_total.input_tokens = 2048;
    t.usage_total.output_tokens = 512;
    t.usage_total.cost_usd = 0.1234;
    let git = GitStatus {
        branch: "feat/powerline".into(),
        staged: 2,
        modified: 1,
    };
    let line = t.footer_powerline(
        &theme(),
        "claude-test",
        Path::new("/tmp/here"),
        Some(&git),
        RouteMode::Auto,
        Some(200_000),
        Some(256),
    );

    let coloured_segments: Vec<_> = line
        .spans
        .iter()
        .filter_map(|span| span.style.map(|style| (style.fg, style.bg)))
        .filter(|(_, bg)| *bg != crossterm::style::Color::Reset)
        .collect();
    let distinct_pairs: BTreeSet<_> = coloured_segments.iter().copied().collect();

    assert!(coloured_segments.len() >= 5, "got {coloured_segments:?}");
    assert!(distinct_pairs.len() >= 5, "got {distinct_pairs:?}");
}

#[test]
fn footer_powerline_basic_segments() {
    let mut t = Transcript::default();
    t.usage_total.input_tokens = 100;
    t.usage_total.output_tokens = 50;
    t.usage_total.cost_usd = 0.0123;
    let line = t.footer_powerline(
        &theme(),
        "claude-test",
        Path::new("/tmp/here"),
        None,
        RouteMode::Static,
        Some(200_000),
        Some(8),
    );
    let joined: String = line.spans.iter().map(|s| s.text.clone()).collect();
    assert!(joined.contains("claude-test"));
    assert!(joined.contains("/tmp/here"));
    // No git segment.
    assert!(!joined.contains("git:"));
    assert!(joined.contains("$0.0123"));
    assert!(joined.contains("route:static"));
    assert!(joined.contains("ctx:0%"));
    // Powerline arrows separate every visible segment.
    assert!(joined.matches('▶').count() >= 3);
}

#[test]
fn footer_powerline_includes_git_segment_when_status_provided() {
    let t = Transcript::default();
    let g = GitStatus {
        branch: "trunk".into(),
        staged: 1,
        modified: 0,
    };
    let line = t.footer_powerline(
        &theme(),
        "m",
        Path::new("/tmp"),
        Some(&g),
        RouteMode::Static,
        None,
        Some(8),
    );
    let joined: String = line.spans.iter().map(|s| s.text.clone()).collect();
    assert!(joined.contains("git: trunk ●1+0"));
    // No context_window → no ctx segment.
    assert!(!joined.contains("ctx:"));
}

#[test]
fn footer_powerline_ctx_caps_at_one_hundred() {
    let mut t = Transcript::default();
    t.usage_total.input_tokens = 999_999_999;
    let line = t.footer_powerline(
        &theme(),
        "m",
        Path::new("/tmp"),
        None,
        RouteMode::Static,
        Some(1_000),
        Some(8),
    );
    let joined: String = line.spans.iter().map(|s| s.text.clone()).collect();
    assert!(joined.contains("ctx:100%"));
}

#[test]
fn footer_powerline_ctx_segment_skipped_when_window_zero() {
    let t = Transcript::default();
    let line = t.footer_powerline(
        &theme(),
        "m",
        Path::new("/tmp"),
        None,
        RouteMode::Static,
        Some(0),
        Some(8),
    );
    let joined: String = line.spans.iter().map(|s| s.text.clone()).collect();
    assert!(!joined.contains("ctx:"));
}
