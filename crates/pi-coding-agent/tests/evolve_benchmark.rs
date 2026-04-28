//! Tests for the benchmark replay harness (G7).
//!
//! Covers case loading from on-disk session JSONL, the Replay trait via
//! a mock backend, and the summary aggregator. Subprocess Replay isn't
//! exercised here — that path is the daemon's (G8) integration target.

use async_trait::async_trait;
use pi_agent_core::{OutcomeSource, SessionEntry, SessionEntryKind, SessionManager};
use pi_ai::Message;
use pi_coding_agent::evolve::{
    load_cases, run_all, summarize, BenchmarkCase, BenchmarkError, Replay, RolloutResult,
};
use std::path::Path;

fn write_session(base: &Path, cwd_slug: &str, session_id: &str, entries: Vec<SessionEntryKind>) {
    let dir = base.join(cwd_slug);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{session_id}.jsonl"));
    let mut buf = String::new();
    for (i, k) in entries.into_iter().enumerate() {
        let entry = SessionEntry {
            id: format!("e{i}"),
            parent_id: if i == 0 {
                None
            } else {
                Some(format!("e{}", i - 1))
            },
            timestamp: 1000 + i as i64,
            kind: k,
        };
        buf.push_str(&serde_json::to_string(&entry).unwrap());
        buf.push('\n');
    }
    std::fs::write(path, buf).unwrap();
}

#[test]
fn load_cases_returns_empty_for_missing_dir() {
    let dir = tempfile::tempdir().unwrap();
    let cases = load_cases(dir.path(), "nonexistent_slug", 10).unwrap();
    assert!(cases.is_empty());
}

#[test]
fn load_cases_extracts_user_prompt_and_outcome() {
    let dir = tempfile::tempdir().unwrap();
    write_session(
        dir.path(),
        "cwd1",
        "abc",
        vec![
            SessionEntryKind::Meta {
                cwd: "/tmp/cwd1".into(),
                provider: "anthropic".into(),
                model: "sonnet".into(),
                title: None,
            },
            SessionEntryKind::User {
                message: Message::user_text("fix the bug in foo.rs"),
            },
            SessionEntryKind::Assistant {
                message: Message::assistant_text("done"),
            },
            SessionEntryKind::Outcome {
                success: true,
                source: OutcomeSource::LlmJudge,
                score: Some(0.91),
                notes: None,
            },
        ],
    );

    let cases = load_cases(dir.path(), "cwd1", 10).unwrap();
    assert_eq!(cases.len(), 1);
    assert_eq!(cases[0].session_id, "abc");
    assert_eq!(cases[0].user_prompt, "fix the bug in foo.rs");
    assert_eq!(cases[0].historical_success, Some(true));
    assert_eq!(cases[0].historical_score, Some(0.91));
}

#[test]
fn load_cases_skips_replay_sourced_outcomes() {
    let dir = tempfile::tempdir().unwrap();
    write_session(
        dir.path(),
        "cwd1",
        "synthetic",
        vec![
            SessionEntryKind::User {
                message: Message::user_text("synthetic prompt"),
            },
            SessionEntryKind::Outcome {
                success: true,
                source: OutcomeSource::Replay,
                score: Some(0.8),
                notes: None,
            },
        ],
    );
    write_session(
        dir.path(),
        "cwd1",
        "real",
        vec![
            SessionEntryKind::User {
                message: Message::user_text("real prompt"),
            },
            SessionEntryKind::Outcome {
                success: true,
                source: OutcomeSource::Heuristic,
                score: Some(0.7),
                notes: None,
            },
        ],
    );
    let cases = load_cases(dir.path(), "cwd1", 10).unwrap();
    assert_eq!(cases.len(), 1, "only the non-replay session counts");
    assert_eq!(cases[0].session_id, "real");
}

#[test]
fn load_cases_skips_sessions_without_outcome() {
    let dir = tempfile::tempdir().unwrap();
    write_session(
        dir.path(),
        "cwd1",
        "abc",
        vec![SessionEntryKind::User {
            message: Message::user_text("never finished"),
        }],
    );
    let cases = load_cases(dir.path(), "cwd1", 10).unwrap();
    assert!(cases.is_empty());
}

#[test]
fn load_cases_skips_sessions_without_user_message() {
    let dir = tempfile::tempdir().unwrap();
    write_session(
        dir.path(),
        "cwd1",
        "abc",
        vec![
            SessionEntryKind::Meta {
                cwd: "/tmp".into(),
                provider: "x".into(),
                model: "y".into(),
                title: None,
            },
            SessionEntryKind::Outcome {
                success: true,
                source: OutcomeSource::Heuristic,
                score: Some(0.5),
                notes: None,
            },
        ],
    );
    let cases = load_cases(dir.path(), "cwd1", 10).unwrap();
    assert!(cases.is_empty());
}

#[test]
fn load_cases_caps_at_max() {
    let dir = tempfile::tempdir().unwrap();
    for id in ["a", "b", "c"] {
        write_session(
            dir.path(),
            "cwd1",
            id,
            vec![
                SessionEntryKind::User {
                    message: Message::user_text(&format!("task {id}")),
                },
                SessionEntryKind::Outcome {
                    success: true,
                    source: OutcomeSource::Heuristic,
                    score: Some(0.5),
                    notes: None,
                },
            ],
        );
    }
    let cases = load_cases(dir.path(), "cwd1", 2).unwrap();
    assert_eq!(cases.len(), 2);
}

#[test]
fn load_cases_compatible_with_session_manager_slug() {
    let base = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let mgr = SessionManager::on_disk(base.path().to_path_buf(), cwd.path().to_path_buf()).unwrap();
    let meta = mgr.create("anthropic", "sonnet").unwrap();
    mgr.append(
        &meta.id,
        SessionEntryKind::User {
            message: Message::user_text("benchmark me"),
        },
    )
    .unwrap();
    mgr.append(
        &meta.id,
        SessionEntryKind::Outcome {
            success: true,
            source: OutcomeSource::Heuristic,
            score: Some(0.8),
            notes: None,
        },
    )
    .unwrap();

    // Mirror SessionManager::cwd_slug.
    let slug = cwd
        .path()
        .display()
        .to_string()
        .replace(['/', '\\', ':'], "_");
    let cases = load_cases(base.path(), &slug, 10).unwrap();
    assert_eq!(cases.len(), 1);
    assert_eq!(cases[0].user_prompt, "benchmark me");
}

// ─── mock Replay + run_all ────────────────────────────────────────────

struct MockReplay {
    score_for: std::sync::Arc<dyn Fn(&BenchmarkCase) -> f32 + Send + Sync>,
}

#[async_trait]
impl Replay for MockReplay {
    async fn run(
        &self,
        case: &BenchmarkCase,
        _agents_md_text: &str,
    ) -> Result<RolloutResult, BenchmarkError> {
        let score = (self.score_for)(case);
        Ok(RolloutResult {
            session_id: case.session_id.clone(),
            success: score > 0.5,
            score,
            tokens_in: 1000,
            tokens_out: 200,
            cost_usd: 0.001,
            duration_ms: 500,
            notes: "mock".into(),
        })
    }
}

fn case(id: &str, prompt: &str) -> BenchmarkCase {
    BenchmarkCase {
        session_id: id.into(),
        user_prompt: prompt.into(),
        historical_success: None,
        historical_score: None,
        trajectory_path: std::path::PathBuf::from("/dev/null"),
    }
}

#[tokio::test]
async fn run_all_returns_one_result_per_case() {
    let cases = vec![case("a", "alpha"), case("b", "beta"), case("c", "gamma")];
    let replay = MockReplay {
        score_for: std::sync::Arc::new(|_| 0.7),
    };
    let results = run_all(&replay, &cases, "AGENTS.md content").await.unwrap();
    assert_eq!(results.len(), 3);
    assert!(results.iter().all(|r| r.success));
}

#[tokio::test]
async fn run_all_errors_on_empty_input() {
    let replay = MockReplay {
        score_for: std::sync::Arc::new(|_| 1.0),
    };
    let err = run_all(&replay, &[], "x").await.unwrap_err();
    assert!(matches!(err, BenchmarkError::NoCases));
}

// ─── summarise ────────────────────────────────────────────────────────

fn rollout(score: f32, tok_in: u64, cost: f32) -> RolloutResult {
    RolloutResult {
        session_id: "s".into(),
        success: score > 0.5,
        score,
        tokens_in: tok_in,
        tokens_out: 100,
        cost_usd: cost,
        duration_ms: 250,
        notes: String::new(),
    }
}

#[test]
fn summary_pass_rate_and_means() {
    let results = vec![
        rollout(0.9, 1000, 0.01),
        rollout(0.8, 2000, 0.02),
        rollout(0.3, 500, 0.005),
        rollout(0.95, 1500, 0.015),
    ];
    let s = summarize(&results);
    assert_eq!(s.n_cases, 4);
    assert!((s.pass_rate - 0.75).abs() < 1e-6);
    assert!((s.mean_score - 0.7375).abs() < 1e-3);
    assert_eq!(s.mean_tokens_in, 1250.0);
    assert!((s.total_cost_usd - 0.05).abs() < 1e-6);
}

#[test]
fn summary_p95_tokens_picks_high_value() {
    let results = vec![
        rollout(0.7, 100, 0.0),
        rollout(0.7, 200, 0.0),
        rollout(0.7, 300, 0.0),
        rollout(0.7, 400, 0.0),
        rollout(0.7, 5000, 0.0),
    ];
    let s = summarize(&results);
    assert_eq!(s.p95_tokens_in, 5000);
}

#[test]
fn summary_handles_empty_input() {
    let s = summarize(&[]);
    assert_eq!(s.n_cases, 0);
    assert_eq!(s.pass_rate, 0.0);
    assert_eq!(s.p95_tokens_in, 0);
}
