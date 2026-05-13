//! Tests for the powerline footer + git-status cache.

use pi_agent_core::RouteMode;
use pi_coding_agent::footer::{format_git, GitStatus, GitStatusCache};
use pi_coding_agent::renderer::Transcript;
use pi_tui::{ColorSpec, NamedColor, Theme};
use std::path::Path;
use std::time::Duration;

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

// ── format_git ──────────────────────────────────────────────────────────────

#[test]
fn format_git_clean_branch_omits_dot_marker() {
    let s = GitStatus {
        branch: "main".into(),
        staged: 0,
        modified: 0,
    };
    assert_eq!(format_git(&s), "git: main");
}

#[test]
fn format_git_dirty_shows_staged_and_modified_counts() {
    let s = GitStatus {
        branch: "feat/x".into(),
        staged: 2,
        modified: 3,
    };
    assert_eq!(format_git(&s), "git: feat/x ●2+3");
}

// ── GitStatusCache ──────────────────────────────────────────────────────────

#[test]
fn git_cache_returns_none_outside_a_repo() {
    let dir = tempfile::tempdir().unwrap();
    let cache = GitStatusCache::default();
    assert!(cache.get(dir.path()).is_none());
}

#[test]
fn git_cache_picks_up_real_repo_state() {
    if which::which("git").is_err() {
        eprintln!("no git binary; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // Initialise a fresh repo with one tracked + one staged file.
    let run = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .output()
            .expect("git ran")
    };
    run(&["init", "-q", "-b", "main"]);
    run(&["config", "user.email", "x@y"]);
    run(&["config", "user.name", "x"]);
    std::fs::write(dir.path().join("a"), "1").unwrap();
    run(&["add", "a"]);
    run(&["commit", "-q", "-m", "init"]);
    // Now: edit a, stage b.
    std::fs::write(dir.path().join("a"), "2").unwrap();
    std::fs::write(dir.path().join("b"), "x").unwrap();
    run(&["add", "b"]);

    let cache = GitStatusCache::default();
    let s = cache.get(dir.path()).expect("repo detected");
    assert_eq!(s.branch, "main");
    assert!(s.staged >= 1, "expected at least one staged file");
    assert!(s.modified >= 1, "expected at least one modified file");
}

#[test]
fn git_cache_serves_repeat_calls_from_memory_within_ttl() {
    let dir = tempfile::tempdir().unwrap();
    // Outside-repo path: status is None either way; we just exercise
    // the caching path. Using a very long ttl so the second call
    // *must* come from cache.
    let cache = GitStatusCache::new(Duration::from_secs(60));
    let _ = cache.get(dir.path());
    let _ = cache.get(dir.path());
    // No assertion beyond "doesn't panic" — coverage of the cache hit
    // branch.
}

#[test]
fn git_cache_invalidate_forces_recompute() {
    let cache = GitStatusCache::new(Duration::from_secs(60));
    let dir = tempfile::tempdir().unwrap();
    let _ = cache.get(dir.path());
    cache.invalidate();
    let _ = cache.get(dir.path());
}

// ── footer_powerline ────────────────────────────────────────────────────────

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
