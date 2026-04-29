//! `pi --stats <verb>` glue. Verbs: `server`, `sync`, `json`, `route-savings`.

use crate::{aggregate, ingest, open_db, server};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy)]
pub enum StatsVerb {
    Server,
    Sync,
    Json,
    RouteSavings,
}

impl StatsVerb {
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        match s {
            "server" | "" => Ok(Self::Server),
            "sync" => Ok(Self::Sync),
            "json" => Ok(Self::Json),
            "route-savings" | "savings" => Ok(Self::RouteSavings),
            other => Err(anyhow::anyhow!(
                "unknown --stats verb '{other}' (expected server|sync|json|route-savings)"
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
        StatsVerb::RouteSavings => {
            let mut savings = aggregate::route_savings(&conn)?;
            savings.sort_by(|a, b| a.route_id.cmp(&b.route_id));

            // Print table header
            println!(
                "{:<15} {:<10} {:<12} {:<14} {:<12} {:<10}",
                "route_id", "turns", "actual_$", "if_sonnet_$", "delta_$", "delta_%"
            );

            let mut total_actual = 0.0_f64;
            let mut total_counterfactual = 0.0_f64;

            // Print each route row
            for row in &savings {
                let delta = row.actual_cost_usd - row.counterfactual_cost_usd;
                let delta_pct = if row.counterfactual_cost_usd > 1e-9 {
                    (delta / row.counterfactual_cost_usd) * 100.0
                } else {
                    0.0
                };
                println!(
                    "{:<15} {:<10} {:<12.4} {:<14.4} {:<12.4} {:<10.1}%",
                    row.route_id,
                    row.turns,
                    row.actual_cost_usd,
                    row.counterfactual_cost_usd,
                    delta,
                    delta_pct
                );
                total_actual += row.actual_cost_usd;
                total_counterfactual += row.counterfactual_cost_usd;
            }

            // Print total row
            let total_delta = total_actual - total_counterfactual;
            let total_delta_pct = if total_counterfactual > 1e-9 {
                (total_delta / total_counterfactual) * 100.0
            } else {
                0.0
            };
            println!(
                "{:<15} {:<10} {:<12.4} {:<14.4} {:<12.4} {:<10.1}%",
                "TOTAL", "", total_actual, total_counterfactual, total_delta, total_delta_pct
            );
            Ok(())
        }
        StatsVerb::Server => {
            let state = server::AppState::new(conn, cfg.sessions_root);
            server::serve(state, cfg.port).await
        }
    }
}
