//! Benchmark replay harness (G7).
//!
//! Pulls outcome-labelled past sessions for the current cwd, extracts the
//! user's original prompt as a benchmark case, and replays each case
//! against a candidate AGENTS.md by spawning `pi --print`. Scores the
//! resulting session with the trajectory judge and aggregates.
//!
//! The actual subprocess invocation is behind a [`Replay`] trait so the
//! evolution daemon (G8) can plug in `SubprocessReplay` while tests can
//! pass a deterministic `MockReplay`.
//!
//! Synthetic flag: every Outcome entry produced by replay is tagged with
//! `OutcomeSource::Replay` (already wired in [`pi_agent_core::OutcomeSource`])
//! so future benchmark loads exclude them — prevents the loop from
//! self-reinforcing on its own outputs.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

use pi_agent_core::{OutcomeSource, SessionEntry, SessionEntryKind};
use pi_ai::Message;

/// One past session distilled into a benchmark target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkCase {
    pub session_id: String,
    /// First user message text — the original task. Replay uses this as
    /// the prompt to the candidate AGENTS.md.
    pub user_prompt: String,
    /// Historical verdict, if any, recorded for this session.
    pub historical_success: Option<bool>,
    pub historical_score: Option<f32>,
    /// Source path of the trajectory file (for traceability).
    pub trajectory_path: PathBuf,
}

/// Result of replaying one case against a candidate AGENTS.md.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RolloutResult {
    pub session_id: String,
    pub success: bool,
    pub score: f32,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cost_usd: f32,
    pub duration_ms: u64,
    /// Free-form note from the judge or fallback (e.g. "tests passed",
    /// "agent looped on src/x.rs").
    pub notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkSummary {
    pub n_cases: usize,
    pub pass_rate: f32,
    pub mean_score: f32,
    pub mean_tokens_in: f32,
    pub mean_tokens_out: f32,
    pub p95_tokens_in: u64,
    pub total_cost_usd: f32,
    pub mean_duration_ms: f32,
}

#[derive(Debug, thiserror::Error)]
pub enum BenchmarkError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("replay backend: {0}")]
    Replay(String),
    #[error("no benchmark cases available")]
    NoCases,
}

/// Load up to `max_cases` benchmark cases from the on-disk session
/// directory for the cwd-slug. Filters:
///
/// - Must have at least one User entry (extract as prompt).
/// - Must have an Outcome entry whose source is NOT `Replay` (don't
///   benchmark against synthetic data).
/// - Sorted most-recent first.
///
/// `sessions_root` is typically `~/.pi/agent/sessions`.
/// `cwd_slug` is `SessionManager::cwd_slug(cwd)` — the per-cwd subdir.
pub fn load_cases(
    sessions_root: &Path,
    cwd_slug: &str,
    max_cases: usize,
) -> Result<Vec<BenchmarkCase>, BenchmarkError> {
    let dir = sessions_root.join(cwd_slug);
    let Ok(read) = std::fs::read_dir(&dir) else {
        return Ok(Vec::new());
    };

    let mut cases: Vec<(i64, BenchmarkCase)> = Vec::new();
    for ent in read.flatten() {
        let path = ent.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(txt) = std::fs::read_to_string(&path) else {
            continue;
        };
        let entries: Vec<SessionEntry> = txt
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        let Some(case) = case_from_entries(&path, &entries) else {
            continue;
        };
        let last_ts = entries.last().map(|e| e.timestamp).unwrap_or(0);
        cases.push((last_ts, case));
    }

    cases.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(cases.into_iter().take(max_cases).map(|(_, c)| c).collect())
}

fn case_from_entries(path: &Path, entries: &[SessionEntry]) -> Option<BenchmarkCase> {
    let mut user_prompt: Option<String> = None;
    let mut last_outcome: Option<(bool, Option<f32>, OutcomeSource)> = None;

    for e in entries {
        match &e.kind {
            SessionEntryKind::User { message } if user_prompt.is_none() => {
                user_prompt = Some(message.text());
            }
            SessionEntryKind::Outcome {
                success,
                source,
                score,
                ..
            } => {
                last_outcome = Some((*success, *score, *source));
            }
            _ => {}
        }
    }

    let prompt = user_prompt?;
    let (success, score, source) = last_outcome?;
    if matches!(source, OutcomeSource::Replay) {
        return None; // never benchmark against synthetic outcomes
    }
    let session_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    Some(BenchmarkCase {
        session_id,
        user_prompt: prompt,
        historical_success: Some(success),
        historical_score: score,
        trajectory_path: path.to_path_buf(),
    })
}

/// Replay backend. The default subprocess impl spawns
/// `pi --print --no-session` with the candidate AGENTS.md staged in a
/// fresh tempdir as cwd; tests use a mock.
#[async_trait]
pub trait Replay: Send + Sync {
    async fn run(
        &self,
        case: &BenchmarkCase,
        agents_md_text: &str,
    ) -> Result<RolloutResult, BenchmarkError>;
}

/// Run all cases against `agents_md_text` and return per-case results.
/// Sequential by default — caller can parallelise with futures::join_all
/// if budget allows. (We keep it sequential here so cost-cap enforcement
/// in the daemon (G8) is straightforward.)
pub async fn run_all<R: Replay>(
    replay: &R,
    cases: &[BenchmarkCase],
    agents_md_text: &str,
) -> Result<Vec<RolloutResult>, BenchmarkError> {
    if cases.is_empty() {
        return Err(BenchmarkError::NoCases);
    }
    let mut out = Vec::with_capacity(cases.len());
    for case in cases {
        out.push(replay.run(case, agents_md_text).await?);
    }
    Ok(out)
}

/// Aggregate rollout results into a benchmark summary.
pub fn summarize(results: &[RolloutResult]) -> BenchmarkSummary {
    let n = results.len();
    if n == 0 {
        return BenchmarkSummary {
            n_cases: 0,
            pass_rate: 0.0,
            mean_score: 0.0,
            mean_tokens_in: 0.0,
            mean_tokens_out: 0.0,
            p95_tokens_in: 0,
            total_cost_usd: 0.0,
            mean_duration_ms: 0.0,
        };
    }
    let nf = n as f32;
    let pass_rate = results.iter().filter(|r| r.success).count() as f32 / nf;
    let mean_score = results.iter().map(|r| r.score).sum::<f32>() / nf;
    let mean_tokens_in = results.iter().map(|r| r.tokens_in as f32).sum::<f32>() / nf;
    let mean_tokens_out = results.iter().map(|r| r.tokens_out as f32).sum::<f32>() / nf;
    let mut tok_in: Vec<u64> = results.iter().map(|r| r.tokens_in).collect();
    tok_in.sort_unstable();
    let p95_idx = ((n as f32 * 0.95).ceil() as usize).saturating_sub(1).min(n - 1);
    let p95_tokens_in = tok_in[p95_idx];
    let total_cost_usd = results.iter().map(|r| r.cost_usd).sum();
    let mean_duration_ms =
        results.iter().map(|r| r.duration_ms as f32).sum::<f32>() / nf;
    BenchmarkSummary {
        n_cases: n,
        pass_rate,
        mean_score,
        mean_tokens_in,
        mean_tokens_out,
        p95_tokens_in,
        total_cost_usd,
        mean_duration_ms,
    }
}

// ─── default subprocess Replay (used by the evolve daemon, G8) ─────────

/// Default subprocess-based Replay. Spawns
/// `<pi_binary> --print --session-dir <tmp> --auto-approve <mode> --agents-md <candidate>`
/// with the case's user prompt as positional. Writes the candidate
/// AGENTS.md to a tempfile, lets pi run, then mines the per-rollout
/// session JSONL the child wrote into the tempdir for usage + outcome.
///
/// Usage (`SessionEntryKind::Usage`) entries are summed across the whole
/// session to populate `tokens_in` / `tokens_out` / `cost_usd` faithfully
/// — no more `chars/4` approximation. The most-recent `Outcome` entry
/// (produced by the modes/ exit-hook + trajectory judge) is the score
/// of record; if no outcome was emitted (subprocess crashed before the
/// hook fired, or the judge declined to score), we fall back to the
/// exit-code heuristic so the benchmark can still make forward progress.
///
/// `auto_approve` defaults to "auto-policy" — safe for benchmark replays
/// (denies dangerous calls per policy) but doesn't block on benign ones
/// the way bare Ask does in non-interactive mode.
pub struct SubprocessReplay {
    pub pi_binary: PathBuf,
    pub timeout: Duration,
    pub auto_approve: String,
    /// Optional cwd for the subprocess; defaults to the parent's cwd
    /// (so the agent can see real files).
    pub cwd: Option<PathBuf>,
}

impl SubprocessReplay {
    pub fn new(pi_binary: PathBuf) -> Self {
        Self {
            pi_binary,
            timeout: Duration::from_secs(180),
            auto_approve: "auto-policy".into(),
            cwd: None,
        }
    }
}

#[async_trait]
impl Replay for SubprocessReplay {
    async fn run(
        &self,
        case: &BenchmarkCase,
        agents_md_text: &str,
    ) -> Result<RolloutResult, BenchmarkError> {
        use tokio::io::AsyncReadExt;

        // Stage the candidate AGENTS.md to a tempfile.
        let mut tmpfile = tempfile::Builder::new()
            .prefix("pi-evolve-agents-md-")
            .suffix(".md")
            .tempfile()
            .map_err(BenchmarkError::Io)?;
        std::io::Write::write_all(tmpfile.as_file_mut(), agents_md_text.as_bytes())
            .map_err(BenchmarkError::Io)?;
        let agents_md_path = tmpfile.path().to_path_buf();

        // Per-rollout session dir. Replaces `--no-session`: the child
        // pi process now writes a real JSONL we can mine for Usage +
        // Outcome. Kept inside a TempDir so it auto-cleans on drop.
        let session_root = tempfile::Builder::new()
            .prefix("pi-evolve-session-")
            .tempdir()
            .map_err(BenchmarkError::Io)?;

        let cwd = self.cwd.clone().unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        });

        let started = std::time::Instant::now();
        let mut cmd = tokio::process::Command::new(&self.pi_binary);
        cmd.arg("--print")
            .arg("--session-dir")
            .arg(session_root.path())
            .arg("--auto-approve")
            .arg(&self.auto_approve)
            .arg("--agents-md")
            .arg(&agents_md_path)
            .arg(&case.user_prompt)
            .current_dir(&cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let mut child = cmd.spawn().map_err(BenchmarkError::Io)?;
        let stdout_handle = child.stdout.take();
        let stderr_handle = child.stderr.take();

        let wait = tokio::time::timeout(self.timeout, child.wait()).await;
        let status = match wait {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => return Err(BenchmarkError::Io(e)),
            Err(_) => {
                let _ = child.kill().await;
                return Err(BenchmarkError::Replay(format!(
                    "rollout timed out after {}s",
                    self.timeout.as_secs()
                )));
            }
        };
        let mut stdout_buf = String::new();
        if let Some(mut out) = stdout_handle {
            let _ = out.read_to_string(&mut stdout_buf).await;
        }
        let mut stderr_buf = String::new();
        if let Some(mut err) = stderr_handle {
            let _ = err.read_to_string(&mut stderr_buf).await;
        }

        let duration_ms = started.elapsed().as_millis() as u64;

        let exit_success = status.success();
        let exit_code = status.code().unwrap_or(-1);

        // Mine Usage + Outcome entries from any JSONL the child wrote.
        let mined = mine_session_dir(session_root.path()).unwrap_or_default();

        // Score: prefer judged Outcome score; then Outcome.success;
        // then exit-code fallback (the historical 0.7/0.3).
        let (success, score, notes) = match mined.outcome {
            Some(o) => {
                let score = o.score.unwrap_or(if o.success { 0.7 } else { 0.3 });
                let notes = o.notes.unwrap_or_else(|| {
                    if o.success { "outcome: success".into() } else { "outcome: failure".into() }
                });
                (o.success, score, notes)
            }
            None => {
                let notes = if exit_success {
                    "exit=0 (no Outcome recorded)".to_string()
                } else {
                    format!(
                        "exit={} (no Outcome) stderr={}",
                        exit_code,
                        stderr_buf.chars().take(200).collect::<String>()
                    )
                };
                (
                    exit_success,
                    if exit_success { 0.7 } else { 0.3 },
                    notes,
                )
            }
        };

        // Tokens / cost: prefer summed Usage entries; fall back to the
        // chars/4 approximation only if the child wrote nothing (e.g.
        // it crashed before the first model turn).
        let (tokens_in, tokens_out, cost_usd) = if mined.had_usage {
            (
                mined.tokens_in,
                mined.tokens_out,
                mined.cost_usd as f32,
            )
        } else {
            let approx_in = (case.user_prompt.chars().count() / 4) as u64
                + (agents_md_text.chars().count() / 4) as u64;
            let approx_out = (stdout_buf.chars().count() / 4) as u64;
            (approx_in, approx_out, 0.0)
        };

        Ok(RolloutResult {
            session_id: case.session_id.clone(),
            success,
            score,
            tokens_in,
            tokens_out,
            cost_usd,
            duration_ms,
            notes,
        })
    }
}

/// What we mine out of a SubprocessReplay's per-rollout session dir.
#[derive(Debug, Default, Clone)]
struct MinedSession {
    /// True if at least one Usage entry was found (used to decide
    /// whether to trust the sums or fall back to chars/4).
    had_usage: bool,
    tokens_in: u64,
    tokens_out: u64,
    cost_usd: f64,
    /// Most-recent Outcome entry, if any.
    outcome: Option<MinedOutcome>,
}

#[derive(Debug, Clone)]
struct MinedOutcome {
    success: bool,
    score: Option<f32>,
    notes: Option<String>,
}

/// Walk `dir` recursively for `*.jsonl` files and aggregate Usage +
/// Outcome entries from every one of them. Returns `None` if no JSONL
/// files exist (subprocess crashed too early to materialise a session).
///
/// The session manager places the child's JSONL at
/// `<dir>/<cwd_slug>/<uuid>.jsonl` so we descend one level. We tolerate
/// malformed lines individually rather than failing the whole rollout.
fn mine_session_dir(dir: &Path) -> Option<MinedSession> {
    let mut acc = MinedSession::default();
    let mut latest_outcome_ts: i64 = i64::MIN;
    let mut found_any_jsonl = false;

    for path in walk_jsonl(dir) {
        found_any_jsonl = true;
        let Ok(txt) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in txt.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(entry) = serde_json::from_str::<SessionEntry>(line) else {
                continue;
            };
            match &entry.kind {
                SessionEntryKind::Usage { usage } => {
                    acc.had_usage = true;
                    acc.tokens_in = acc.tokens_in.saturating_add(usage.input_tokens);
                    acc.tokens_out = acc.tokens_out.saturating_add(usage.output_tokens);
                    acc.cost_usd += usage.cost_usd;
                }
                SessionEntryKind::Outcome {
                    success,
                    score,
                    notes,
                    ..
                } => {
                    if entry.timestamp >= latest_outcome_ts {
                        latest_outcome_ts = entry.timestamp;
                        acc.outcome = Some(MinedOutcome {
                            success: *success,
                            score: *score,
                            notes: notes.clone(),
                        });
                    }
                }
                _ => {}
            }
        }
    }

    if !found_any_jsonl {
        return None;
    }
    Some(acc)
}

/// Iterative walker that yields every `*.jsonl` under `root`. Bounded
/// depth (we only descend a handful of subdirectories — the session
/// layout is `<root>/<cwd_slug>/<uuid>.jsonl`) and ignores symlinks to
/// avoid loops.
fn walk_jsonl(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for ent in read.flatten() {
            let p = ent.path();
            let Ok(meta) = ent.metadata() else { continue };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                stack.push(p);
            } else if meta.is_file()
                && p.extension().and_then(|s| s.to_str()) == Some("jsonl")
            {
                out.push(p);
            }
        }
    }
    out
}

// ─── synthesise a User message from a prompt string ────────────────────

/// Helper used by the daemon (G8) when re-feeding a benchmark prompt
/// through pi as if it came from the user.
pub fn user_message(text: &str) -> Message {
    Message::user_text(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_agent_core::SessionEntry;
    use pi_ai::Usage;
    use serde_json::json;
    use std::io::Write;

    /// Helper: write a fake session JSONL with the given entries to
    /// `<dir>/<sub>/<id>.jsonl`, mirroring the layout the real
    /// SessionManager produces.
    fn write_session(dir: &Path, sub: &str, entries: &[serde_json::Value]) -> PathBuf {
        let subdir = dir.join(sub);
        std::fs::create_dir_all(&subdir).unwrap();
        let path = subdir.join(format!("session-{sub}.jsonl"));
        let mut f = std::fs::File::create(&path).unwrap();
        for e in entries {
            writeln!(f, "{}", e.to_string()).unwrap();
        }
        path
    }

    fn entry(ts: i64, kind: serde_json::Value) -> serde_json::Value {
        // SessionEntry is { id, parent_id, timestamp, ...flatten kind }.
        let mut v = json!({
            "id": format!("e-{ts}"),
            "parent_id": null,
            "timestamp": ts,
        });
        if let serde_json::Value::Object(o) = kind {
            for (k, val) in o {
                v[k] = val;
            }
        }
        v
    }

    #[test]
    fn mine_session_dir_returns_none_when_no_jsonl() {
        let tmp = tempfile::tempdir().unwrap();
        // Write a non-jsonl file — should still be ignored.
        std::fs::write(tmp.path().join("not-a-session.txt"), "hi").unwrap();
        assert!(mine_session_dir(tmp.path()).is_none());
    }

    #[test]
    fn mine_session_dir_sums_usage_across_entries() {
        let tmp = tempfile::tempdir().unwrap();
        write_session(
            tmp.path(),
            "_home_user_proj",
            &[
                entry(1, json!({ "kind": "meta", "cwd": "/x", "provider": "p", "model": "m", "title": null })),
                entry(2, json!({ "kind": "usage", "usage": Usage { input_tokens: 100, output_tokens: 30, cost_usd: 0.0025, ..Default::default() }})),
                entry(3, json!({ "kind": "usage", "usage": Usage { input_tokens: 50, output_tokens: 15, cost_usd: 0.0010, ..Default::default() }})),
            ],
        );
        let mined = mine_session_dir(tmp.path()).expect("had a jsonl");
        assert!(mined.had_usage);
        assert_eq!(mined.tokens_in, 150);
        assert_eq!(mined.tokens_out, 45);
        assert!((mined.cost_usd - 0.0035).abs() < 1e-9);
        assert!(mined.outcome.is_none());
    }

    #[test]
    fn mine_session_dir_takes_latest_outcome_by_timestamp() {
        let tmp = tempfile::tempdir().unwrap();
        write_session(
            tmp.path(),
            "slug",
            &[
                entry(10, json!({ "kind": "outcome", "success": false, "source": "heuristic", "score": 0.2, "notes": "older" })),
                entry(20, json!({ "kind": "outcome", "success": true, "source": "llm_judge", "score": 0.91, "notes": "newer" })),
                entry(15, json!({ "kind": "outcome", "success": false, "source": "heuristic", "score": 0.4, "notes": "middle" })),
            ],
        );
        let o = mine_session_dir(tmp.path())
            .expect("dir")
            .outcome
            .expect("outcome");
        assert!(o.success, "newest outcome wins");
        assert_eq!(o.score, Some(0.91));
        assert_eq!(o.notes.as_deref(), Some("newer"));
    }

    #[test]
    fn mine_session_dir_skips_malformed_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let subdir = tmp.path().join("slug");
        std::fs::create_dir_all(&subdir).unwrap();
        let path = subdir.join("s.jsonl");
        let usage = entry(2, json!({ "kind": "usage", "usage": Usage { input_tokens: 7, output_tokens: 0, ..Default::default() }}));
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "{{this is not valid json").unwrap();
        writeln!(f, "").unwrap();
        writeln!(f, "{}", usage).unwrap();
        writeln!(f, "{{\"id\":\"x\",\"parent_id\":null,\"timestamp\":3,\"kind\":\"unknown_variant\"}}").unwrap();
        let mined = mine_session_dir(tmp.path()).expect("dir");
        assert_eq!(mined.tokens_in, 7);
    }

    #[test]
    fn walk_jsonl_descends_one_level_and_filters_extension() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("slug")).unwrap();
        std::fs::write(tmp.path().join("slug").join("a.jsonl"), "").unwrap();
        std::fs::write(tmp.path().join("slug").join("b.txt"), "").unwrap();
        std::fs::write(tmp.path().join("top.jsonl"), "").unwrap();
        let mut found: Vec<_> = walk_jsonl(tmp.path())
            .into_iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        found.sort();
        assert_eq!(found, vec!["a.jsonl".to_string(), "top.jsonl".into()]);
    }

    #[test]
    fn summarize_uses_real_usage_fields() {
        let r = vec![
            RolloutResult {
                session_id: "a".into(),
                success: true,
                score: 0.9,
                tokens_in: 1000,
                tokens_out: 200,
                cost_usd: 0.012,
                duration_ms: 4000,
                notes: "ok".into(),
            },
            RolloutResult {
                session_id: "b".into(),
                success: false,
                score: 0.3,
                tokens_in: 1500,
                tokens_out: 100,
                cost_usd: 0.018,
                duration_ms: 2000,
                notes: "fail".into(),
            },
        ];
        let s = summarize(&r);
        assert_eq!(s.n_cases, 2);
        assert!((s.pass_rate - 0.5).abs() < 1e-6);
        assert!((s.mean_tokens_in - 1250.0).abs() < 1e-6);
        assert!((s.total_cost_usd - 0.030).abs() < 1e-6);
        assert_eq!(s.p95_tokens_in, 1500);
    }

    /// Round-trip a single SessionEntry through serde_json to make sure
    /// the canned JSON shape we use in tests matches the real type — if
    /// the schema drifts (e.g. a field renamed), this test fails fast
    /// instead of the mining tests silently treating entries as
    /// malformed.
    #[test]
    fn canned_entries_round_trip_through_session_entry() {
        let v = entry(
            42,
            json!({ "kind": "usage", "usage": Usage { input_tokens: 1, output_tokens: 2, ..Default::default() }}),
        );
        let _: SessionEntry = serde_json::from_value(v).unwrap();
        let v = entry(7, json!({ "kind": "outcome", "success": true, "source": "heuristic", "score": 0.5, "notes": null }));
        let _: SessionEntry = serde_json::from_value(v).unwrap();
    }
}
