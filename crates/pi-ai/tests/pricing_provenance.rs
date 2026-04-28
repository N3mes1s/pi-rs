//! Verifies that `crates/pi-ai/data/pricing.json` stays in sync with the
//! registry: every row has a non-empty `sources` array of HTTPS URLs, and
//! every model in `default_providers()` has a corresponding pricing row.

use serde::Deserialize;

const PRICING_JSON: &str = include_str!("../data/pricing.json");

#[derive(Debug, Deserialize)]
struct PricingTable {
    rows: Vec<PricingRow>,
}

#[derive(Debug, Deserialize)]
struct PricingRow {
    provider: String,
    model: String,
    #[allow(dead_code)]
    input_cost_per_mtok: f64,
    #[allow(dead_code)]
    output_cost_per_mtok: f64,
    sources: Vec<String>,
}

fn load() -> PricingTable {
    serde_json::from_str(PRICING_JSON).expect("pricing.json must be valid JSON")
}

#[test]
fn every_row_has_at_least_one_source() {
    let table = load();
    for row in &table.rows {
        assert!(
            !row.sources.is_empty(),
            "{}/{} has no sources",
            row.provider,
            row.model
        );
    }
}

#[test]
fn every_source_url_is_https() {
    let table = load();
    for row in &table.rows {
        for url in &row.sources {
            assert!(
                url.starts_with("https://"),
                "{}/{} source {url} is not https",
                row.provider,
                row.model
            );
        }
    }
}

#[test]
fn every_registry_model_has_a_pricing_row() {
    use pi_ai::auth::AuthStorage;
    use pi_ai::registry::ModelRegistry;

    let table = load();
    let registry = ModelRegistry::new(AuthStorage::in_memory());
    for provider in registry.providers() {
        for model in &provider.models {
            let found = table
                .rows
                .iter()
                .any(|r| r.provider == provider.name && r.model == model.id);
            assert!(
                found,
                "no pricing.json row for {}/{}",
                provider.name, model.id
            );
        }
    }
}
