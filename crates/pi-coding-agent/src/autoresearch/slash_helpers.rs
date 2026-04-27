//! Pure helper functions used by the `/autoresearch` slash-command handler.
//!
//! These are free functions that operate on the filesystem only (no agent
//! session required), making them straightforward to unit-test.

use std::path::Path;

use crate::autoresearch::session::{MetricDirection, Session, SessionConfig};

// ── slash action types ────────────────────────────────────────────────────────

/// The sub-command understood by `/autoresearch`.
#[derive(Debug, Clone, PartialEq)]
pub enum AutoresearchAction {
    /// `/autoresearch <text>` — enter or resume the experiment loop.
    Start { text: String },
    /// `/autoresearch off` — suspend the loop; preserve logs.
    Off,
    /// `/autoresearch clear` — delete all autoresearch artefacts in `cwd`.
    Clear,
    /// `/autoresearch export` — render the dashboard table to an HTML file.
    Export,
}

/// Parse the argument string that follows `/autoresearch`.
pub fn parse_action(args: &str) -> AutoresearchAction {
    match args.trim() {
        "off" => AutoresearchAction::Off,
        "clear" => AutoresearchAction::Clear,
        "export" => AutoresearchAction::Export,
        other => AutoresearchAction::Start {
            text: other.to_string(),
        },
    }
}

// ── clear ─────────────────────────────────────────────────────────────────────

/// Delete `autoresearch.{jsonl,md,config.json}` in `cwd`.
///
/// Missing files are silently ignored.  Returns the list of paths that were
/// actually removed.
pub fn clear_artefacts(cwd: &Path) -> Vec<std::path::PathBuf> {
    let names = [
        "autoresearch.jsonl",
        "autoresearch.md",
        "autoresearch.config.json",
    ];
    let mut removed = Vec::new();
    for name in &names {
        let p = cwd.join(name);
        if p.exists() {
            if std::fs::remove_file(&p).is_ok() {
                removed.push(p);
            }
        }
    }
    removed
}

// ── start / resume ────────────────────────────────────────────────────────────

/// Ensure a [`Session`] exists in `cwd` for the given experiment text.
///
/// * If `<cwd>/autoresearch.config.json` already exists, loads and returns it
///   (resume path).
/// * Otherwise creates a new [`Session`] with a default [`SessionConfig`]
///   derived from `text` (the raw argument after `/autoresearch`), writes the
///   config and md header to disk, and returns it.
///
/// The "default" config uses `Lower` direction and treats `text` as the
/// experiment name.
pub fn ensure_session(cwd: &Path, text: &str) -> std::io::Result<Session> {
    let config_path = cwd.join("autoresearch.config.json");
    if config_path.exists() {
        return Session::load(cwd);
    }

    // Bootstrap a minimal session from the text argument.
    let config = SessionConfig {
        name: text.trim().to_string(),
        metric: "metric".to_string(),
        unit: "".to_string(),
        direction: MetricDirection::Lower,
        max_iterations: None,
        working_dir: None,
    };
    let session = Session::new(cwd, config);
    session.save_config()?;
    session.save_md()?;
    Ok(session)
}

// ── export ────────────────────────────────────────────────────────────────────

/// Render the autoresearch dashboard table and write it to
/// `<cwd>/autoresearch-dashboard.html`.
///
/// Returns the path written on success, or an error string on failure.
pub fn export_dashboard(cwd: &Path) -> Result<std::path::PathBuf, String> {
    // Load session — we need the config for the DashboardState.
    let session = Session::load(cwd).map_err(|e| format!("cannot load session: {e}"))?;

    // Load JSONL log entries — upstream-format (run entries).
    let log = crate::autoresearch::log::JsonlLog::new(session.jsonl_path());
    let runs = log.read_runs().map_err(|e| format!("cannot read log: {e}"))?;

    // Build run rows: (description, metric, kept).
    let mut run_rows: Vec<(String, f64, bool)> = Vec::new();
    let mut sample_values: Vec<f64> = Vec::new();

    for r in &runs {
        let kept = r.status == crate::autoresearch::log::RunStatus::Keep;
        run_rows.push((r.description.clone(), r.metric, kept));
        sample_values.push(r.metric);
    }

    // Baseline = first run's metric (the canonical "before any change" sample).
    let baseline = sample_values.first().copied().unwrap_or(0.0);

    let current_best = match session.config.direction {
        MetricDirection::Lower => sample_values
            .iter()
            .copied()
            .fold(f64::INFINITY, f64::min),
        MetricDirection::Higher => sample_values
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max),
    };
    let current_best = if current_best.is_infinite() {
        baseline
    } else {
        current_best
    };

    let confidence = crate::autoresearch::confidence::compute(
        &sample_values,
        baseline,
        session.config.direction,
    );

    let kept_count = run_rows.iter().filter(|(_, _, k)| *k).count();

    let state = crate::autoresearch::dashboard::DashboardState {
        session_name: session.config.name.clone(),
        runs: run_rows.len(),
        kept: kept_count,
        metric_name: session.config.metric.clone(),
        baseline,
        current_best,
        direction: session.config.direction,
        confidence,
    };

    let table = crate::autoresearch::dashboard::render_table(&state, &run_rows);

    // Wrap in minimal HTML.
    let html = format!(
        "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\">\
         <title>autoresearch: {name}</title></head>\
         <body><pre>{body}</pre></body></html>\n",
        name = html_escape(&session.config.name),
        body = html_escape(&table),
    );

    let out_path = cwd.join("autoresearch-dashboard.html");
    std::fs::write(&out_path, html).map_err(|e| format!("cannot write HTML: {e}"))?;
    Ok(out_path)
}

// ── tiny HTML escaper ─────────────────────────────────────────────────────────

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
