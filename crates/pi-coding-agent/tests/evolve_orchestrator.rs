//! Tests for the tick orchestrator (G8 part 2).
//!
//! Provider integration is the same plumbing as auto_approve::judge —
//! covered there. Here we exercise the orchestration shape end-to-end
//! using a mock Replay backend, covering the gates, baseline benchmark,
//! candidate apply path, rollback monitor, and skip reasons.

use async_trait::async_trait;
use pi_agent_core::{OutcomeSource, SessionEntry, SessionEntryKind};
use pi_ai::{AuthStorage, Message, ModelRegistry};
use pi_coding_agent::evolve::{
    check_rollback, run_tick, BenchmarkCase, BenchmarkError, Replay, RolloutResult, TickInputs,
    TickReport,
};
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};

// ─── helpers ──────────────────────────────────────────────────────────

fn write_session_jsonl(
    sessions_root: &Path,
    cwd_slug: &str,
    session_id: &str,
    user_text: &str,
    success: bool,
) {
    let dir = sessions_root.join(cwd_slug);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{session_id}.jsonl"));
    let entries = vec![
        SessionEntry {
            id: "e0".into(),
            parent_id: None,
            timestamp: 1000,
            kind: SessionEntryKind::Meta {
                cwd: "/x".into(),
                provider: "anthropic".into(),
                model: "sonnet".into(),
                title: None,
            },
        },
        SessionEntry {
            id: "e1".into(),
            parent_id: Some("e0".into()),
            timestamp: 1001,
            kind: SessionEntryKind::User {
                message: Message::user_text(user_text),
            },
        },
        SessionEntry {
            id: "e2".into(),
            parent_id: Some("e1".into()),
            timestamp: 1002,
            kind: SessionEntryKind::Outcome {
                success,
                source: OutcomeSource::Heuristic,
                score: Some(if success { 0.85 } else { 0.2 }),
                notes: None,
            },
        },
    ];
    let mut buf = String::new();
    for e in &entries {
        buf.push_str(&serde_json::to_string(e).unwrap());
        buf.push('\n');
    }
    std::fs::write(path, buf).unwrap();
}

fn write_agents_md(cwd: &Path, body: &str) -> std::path::PathBuf {
    let p = cwd.join("AGENTS.md");
    std::fs::write(&p, body).unwrap();
    p
}

fn cwd_slug(p: &Path) -> String {
    p.display().to_string().replace(['/', '\\', ':'], "_")
}

// Mock Replay that returns successive scores from a list (cycles).
struct MockReplay {
    scores: Vec<f32>,
    counter: AtomicU32,
}

#[async_trait]
impl Replay for MockReplay {
    async fn run(
        &self,
        case: &BenchmarkCase,
        _agents_md_text: &str,
    ) -> Result<RolloutResult, BenchmarkError> {
        let i = self.counter.fetch_add(1, Ordering::SeqCst) as usize;
        let score = self.scores[i % self.scores.len()];
        Ok(RolloutResult {
            session_id: case.session_id.clone(),
            success: score > 0.5,
            score,
            tokens_in: 1000,
            tokens_out: 200,
            cost_usd: 0.0,
            duration_ms: 100,
            notes: format!("mock score={score}"),
        })
    }
}

fn settings_with_low_thresholds() -> pi_agent_core::Settings {
    let mut s = pi_agent_core::Settings::default();
    s.evolve.min_samples = 1;
    s.evolve.benchmark_size = 5;
    s.evolve.generations_per_tick = 0; // no slow-model calls in tests
    s.evolve.min_hours_between_ticks = 0;
    s.evolve.min_new_outcomes_to_retick = 0;
    s.evolve.daily_cost_cap_usd = 100.0;
    s
}

// ─── skip-path tests (don't need a real model) ─────────────────────────

#[tokio::test]
async fn tick_skipped_when_disabled() {
    let cwd = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let mut settings = settings_with_low_thresholds();
    settings.evolve.enabled = false;
    let auth = AuthStorage::in_memory();
    let registry = ModelRegistry::new(auth.clone());
    let agents_md = write_agents_md(cwd.path(), "## Stuff\nx\n");
    write_session_jsonl(sessions.path(), &cwd_slug(cwd.path()), "s1", "fix it", true);

    let inputs = TickInputs {
        cwd: cwd.path(),
        sessions_root: sessions.path(),
        agents_md_path: agents_md,
        settings: &settings,
        registry: &registry,
        auth: &auth,
    };
    let mock = MockReplay {
        scores: vec![0.9],
        counter: AtomicU32::new(0),
    };
    match run_tick(inputs, &mock).await.unwrap() {
        TickReport::Skipped(_) => {}
        TickReport::Ran { .. } => panic!("expected skip"),
    }
}

#[tokio::test]
async fn tick_skipped_when_no_agents_md() {
    let cwd = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let settings = settings_with_low_thresholds();
    let auth = AuthStorage::in_memory();
    let registry = ModelRegistry::new(auth.clone());
    write_session_jsonl(sessions.path(), &cwd_slug(cwd.path()), "s1", "fix it", true);
    let inputs = TickInputs {
        cwd: cwd.path(),
        sessions_root: sessions.path(),
        agents_md_path: cwd.path().join("AGENTS.md"), // doesn't exist
        settings: &settings,
        registry: &registry,
        auth: &auth,
    };
    let mock = MockReplay {
        scores: vec![0.9],
        counter: AtomicU32::new(0),
    };
    match run_tick(inputs, &mock).await.unwrap() {
        TickReport::Skipped(_) => {}
        _ => panic!("expected skip"),
    }
}

#[tokio::test]
async fn tick_skipped_when_no_samples() {
    let cwd = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let mut settings = settings_with_low_thresholds();
    settings.evolve.min_samples = 100; // need way more than we have
    let auth = AuthStorage::in_memory();
    let registry = ModelRegistry::new(auth.clone());
    let agents_md = write_agents_md(cwd.path(), "## A\nb\n");
    write_session_jsonl(sessions.path(), &cwd_slug(cwd.path()), "s1", "fix it", true);
    let inputs = TickInputs {
        cwd: cwd.path(),
        sessions_root: sessions.path(),
        agents_md_path: agents_md,
        settings: &settings,
        registry: &registry,
        auth: &auth,
    };
    let mock = MockReplay {
        scores: vec![0.9],
        counter: AtomicU32::new(0),
    };
    match run_tick(inputs, &mock).await.unwrap() {
        TickReport::Skipped(_) => {}
        _ => panic!("expected skip"),
    }
}

#[tokio::test]
async fn tick_runs_baseline_when_zero_generations_configured() {
    // generations_per_tick = 0 means we just measure the baseline.
    // No slow-model calls, no apply.
    let cwd = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let settings = settings_with_low_thresholds();
    let auth = AuthStorage::in_memory();
    let registry = ModelRegistry::new(auth.clone());
    let agents_md = write_agents_md(cwd.path(), "# Title\n\n## Tools\nuse cargo\n");
    write_session_jsonl(
        sessions.path(),
        &cwd_slug(cwd.path()),
        "s1",
        "fix bug",
        true,
    );

    let inputs = TickInputs {
        cwd: cwd.path(),
        sessions_root: sessions.path(),
        agents_md_path: agents_md.clone(),
        settings: &settings,
        registry: &registry,
        auth: &auth,
    };
    let mock = MockReplay {
        scores: vec![0.9],
        counter: AtomicU32::new(0),
    };
    let report = run_tick(inputs, &mock).await.unwrap();
    match report {
        TickReport::Ran {
            baseline,
            generations,
            applied_hash,
        } => {
            assert!(baseline.pass_rate > 0.0);
            assert_eq!(generations.len(), 1, "baseline only");
            assert!(applied_hash.is_none(), "no candidates → no apply");
        }
        TickReport::Skipped(why) => panic!("expected ran, got skip: {why:?}"),
    }
    // AGENTS.md unchanged.
    assert_eq!(
        std::fs::read_to_string(&agents_md).unwrap(),
        "# Title\n\n## Tools\nuse cargo\n"
    );
}

// ─── rollback monitor ─────────────────────────────────────────────────

#[test]
fn check_rollback_does_nothing_when_no_pending_apply() {
    let cwd = tempfile::tempdir().unwrap();
    let agents_md = cwd.path().join("AGENTS.md");
    std::fs::write(&agents_md, "current").unwrap();
    let did = check_rollback(cwd.path(), &agents_md, &[true; 5], 5, 0.15).unwrap();
    assert!(!did);
}

#[test]
fn check_rollback_restores_on_regression() {
    use pi_coding_agent::evolve::{backup_and_apply, PendingApply};
    let cwd = tempfile::tempdir().unwrap();
    let agents_md = cwd.path().join("AGENTS.md");
    std::fs::write(&agents_md, "v1").unwrap();
    let backup = backup_and_apply(cwd.path(), &agents_md, "v2", "h_baseline").unwrap();
    let pending = PendingApply {
        applied_hash: "h_candidate".into(),
        previous_hash: "h_baseline".into(),
        backup_path: backup,
        baseline_pass_rate: 0.9,
        applied_at_ms: 0,
        outcomes_seen_at_apply: 10,
    };
    pending.save(cwd.path()).unwrap();

    // 10 sessions post-apply, all failures → pass_rate 0 vs baseline 0.9.
    let did = check_rollback(cwd.path(), &agents_md, &[false; 10], 10, 0.15).unwrap();
    assert!(did, "should rollback on clear regression");
    assert_eq!(std::fs::read_to_string(&agents_md).unwrap(), "v1");
    // PendingApply cleared, hash poisoned.
    assert!(PendingApply::load(cwd.path()).is_none());
    assert!(pi_coding_agent::evolve::is_poisoned(
        cwd.path(),
        "h_candidate"
    ));
}

#[test]
fn check_rollback_holds_when_window_short() {
    use pi_coding_agent::evolve::{backup_and_apply, PendingApply};
    let cwd = tempfile::tempdir().unwrap();
    let agents_md = cwd.path().join("AGENTS.md");
    std::fs::write(&agents_md, "v1").unwrap();
    let backup = backup_and_apply(cwd.path(), &agents_md, "v2", "h").unwrap();
    PendingApply {
        applied_hash: "hc".into(),
        previous_hash: "h".into(),
        backup_path: backup,
        baseline_pass_rate: 0.9,
        applied_at_ms: 0,
        outcomes_seen_at_apply: 0,
    }
    .save(cwd.path())
    .unwrap();

    let did = check_rollback(cwd.path(), &agents_md, &[false; 3], 10, 0.15).unwrap();
    assert!(!did, "window not full yet");
    assert_eq!(std::fs::read_to_string(&agents_md).unwrap(), "v2");
}
