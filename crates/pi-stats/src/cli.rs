//! `pi --stats <verb>` glue. Verbs: `server`, `sync`, `json`.

use crate::{aggregate, ingest, open_db, server};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy)]
pub enum StatsVerb {
    Server,
    Sync,
    Json,
}

impl StatsVerb {
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        match s {
            "server" | "" => Ok(Self::Server),
            "sync" => Ok(Self::Sync),
            "json" => Ok(Self::Json),
            other => Err(anyhow::anyhow!(
                "unknown --stats verb '{other}' (expected server|sync|json)"
            )),
        }
    }
}

pub struct StatsConfig {
    pub db_path: PathBuf,
    pub sessions_root: PathBuf,
    pub port: u16,
}

impl Default for StatsConfig {
    fn default() -> Self {
        Self {
            db_path: ingest::default_db_path(),
            sessions_root: ingest::default_sessions_root(),
            port: 3847,
        }
    }
}

/// Run a stats verb. Server verb is async (axum); the others are
/// synchronous dispatch returning quickly.
pub async fn run(verb: StatsVerb, cfg: StatsConfig) -> anyhow::Result<()> {
    let mut conn = open_db(&cfg.db_path)?;
    let report = ingest::sync_all(&mut conn, &cfg.sessions_root)?;
    match verb {
        StatsVerb::Sync => {
            println!(
                "pi-stats: scanned {} file(s), inserted {} row(s)",
                report.files, report.rows
            );
            Ok(())
        }
        StatsVerb::Json => {
            let dashboard = aggregate::dashboard(&conn)?;
            let s = serde_json::to_string_pretty(&dashboard)?;
            println!("{s}");
            Ok(())
        }
        StatsVerb::Server => {
            let state = server::AppState::new(conn, cfg.sessions_root);
            server::serve(state, cfg.port).await
        }
    }
}
