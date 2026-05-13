//! `/cost` slash command: sync pi-stats then summarise total cost
//! for the current working directory.
//!
//! The work is split into a pure formatter (unit-tested) and a thin
//! async wrapper that drives `pi_stats::ingest::sync_all` +
//! `aggregate::by_folder`. Mirrors `pi --stats sync` followed by a
//! folder filter on the dashboard.

use std::path::Path;

use pi_stats::aggregate::{self, FolderStats};
use pi_stats::{ingest, open_db};

/// Render the inline `/cost` reply. `cwd` is matched against
/// `FolderStats::folder` (which carries the absolute path recorded
/// in each session's `Meta` entry).
pub fn format_cost_report(cwd: &Path, folders: &[FolderStats]) -> String {
    let cwd_str = cwd.to_string_lossy();
    let stats = folders.iter().find(|f| f.folder == cwd_str);
    match stats {
        Some(s) => format!(
            "[/cost] {}: ${:.4} across {} request(s) (in={}, out={})",
            cwd_str, s.cost, s.requests, s.input_tokens, s.output_tokens,
        ),
        None => format!("[/cost] {}: no recorded usage yet", cwd_str),
    }
}

/// Sync the pi-stats DB then build a one-line cost summary for `cwd`.
/// Errors collapse into a human-readable message so the TUI can print
/// the result as a Note block.
pub async fn run_cost_command(cwd: &Path) -> String {
    let cwd = cwd.to_path_buf();
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
        let db_path = ingest::default_db_path();
        let sessions_root = ingest::default_sessions_root();
        let mut conn = open_db(&db_path)?;
        let _ = ingest::sync_all(&mut conn, &sessions_root)?;
        let folders = aggregate::by_folder(&conn)?;
        Ok(format_cost_report(&cwd, &folders))
    })
    .await;
    match res {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => format!("[/cost] error: {e}"),
        Err(e) => format!("[/cost] join error: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fs(folder: &str, requests: u64, cost: f64, i: u64, o: u64) -> FolderStats {
        FolderStats {
            folder: folder.into(),
            requests,
            cost,
            input_tokens: i,
            output_tokens: o,
        }
    }

    #[test]
    fn format_picks_matching_folder() {
        let cwd = PathBuf::from("/home/me/proj");
        let folders = vec![
            fs("/other", 3, 0.10, 100, 50),
            fs("/home/me/proj", 7, 0.4321, 1234, 567),
        ];
        let out = format_cost_report(&cwd, &folders);
        assert!(out.contains("/home/me/proj"), "{out}");
        assert!(out.contains("$0.4321"), "{out}");
        assert!(out.contains("7 request(s)"), "{out}");
        assert!(out.contains("in=1234"), "{out}");
        assert!(out.contains("out=567"), "{out}");
    }

    #[test]
    fn format_handles_missing_folder() {
        let cwd = PathBuf::from("/no/match");
        let folders = vec![fs("/other", 1, 0.01, 10, 5)];
        let out = format_cost_report(&cwd, &folders);
        assert!(out.contains("no recorded usage"), "{out}");
        assert!(out.contains("/no/match"), "{out}");
    }
}
