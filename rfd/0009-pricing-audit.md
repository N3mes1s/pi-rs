# RFD 0009 — Audit + calibrate the model pricing table

- **Status:** Discussion
- **Author:** pi-rs maintainers
- **Created:** 2026-04-28
- **Implemented:** &lt;pending&gt;

## Summary

`crates/pi-ai/src/registry.rs::default_providers` carries 24 model
rows, each with `(input_cost_per_mtok, output_cost_per_mtok)` baked
in. Eight of those rows still hold the original placeholder pair
`(0.5, 1.5)` — Cerebras, Groq, xAI, DeepSeek (×2), Mistral, Z.ai, and
Kimi/Moonshot — which means cost tracking for any of those providers
is silently wrong. Even rows we *think* are right (OpenAI o3-pro at
$60/$240, Google `gemini-2.5-pro` at $1.25/$5.0) deserve a re-check
because providers reprice.

This RFD ships:

1. A **pricing audit** that drives the existing `web_search` tool
   (parallel.ai backend) against each row and records what the
   public list price says.
2. A `crates/pi-ai/data/pricing.json` provenance file pinning each
   row to a verified-at timestamp + source URL — so future audits
   are diffable and CI can fail loud when a row drifts.
3. An updated `default_providers()` populated from the audit.
4. A unit test that ensures (a) every row has a non-zero `input_cost
   _per_mtok` *and* `output_cost_per_mtok`, and (b) every row in the
   registry has a corresponding `pricing.json` entry.

## Background

Pre-conditions in place:

- `pi_tools::web_search::WebSearchTool` already supports parallel.ai
  (`WebSearchProvider::Parallel`, env var `PARALLEL_API_KEY`,
  endpoint `https://api.parallel.ai/alpha/search`). The Anthropic
  backend now also works after the
  `claude-3-5-sonnet-latest` → `claude-haiku-4-5-20251001` fix
  (commit `7d0d63e`).
- `Usage.cost_usd` flows from `compute_cost(model, &UsageAcc)`
  (RFD 0008) through `SessionEntry::Usage` (commit `a551928`) into
  pi-stats's SQLite. Whatever `default_providers()` says about per-
  MTok pricing is the *only* knob between a real spend and what
  `pi --stats json` shows.

The 24 rows + their current values, for reference:

| provider | id | in $/MTok | out $/MTok |
|---|---|---|---|
| anthropic | `claude-opus-4-7` | 15.00 | 75.00 |
| anthropic | `claude-sonnet-4-6` | 3.00 | 15.00 |
| anthropic | `claude-haiku-4-5-20251001` | 0.80 | 4.00 |
| openai | `gpt-4o`, `gpt-4o-mini`, `o1`, `o1-mini`, `o3-mini`, `o3`, `o3-pro`, `o4-mini`, `gpt-5`, `gpt-5-mini`, `gpt-5-nano` | various | various |
| google | `gemini-2.5-pro`, `gemini-2.5-flash` | 1.25 / 0.075 | 5.00 / 0.30 |
| cerebras / groq / xai / deepseek×2 / mistral / zai / kimi | various | **0.50** | **1.50** |

Eight of those last-row entries are demonstrably untrustworthy.

## Proposal

### 1. Audit driver — a one-shot `pi-pricing-audit` binary

A small binary (`crates/pi-ai/src/bin/pricing_audit.rs`) that:

```rust
fn main() -> Result<()> {
    let registry = ModelRegistry::new(AuthStorage::in_memory());
    let mut report = Vec::new();
    for provider in registry.providers() {
        for model in &provider.models {
            let q = format!(
                "{} {} list price input output dollars per million tokens 2026",
                provider.name, model.id
            );
            let hits = web_search_blocking(&q, WebSearchProvider::Parallel)?;
            report.push(audit_one(provider, model, &hits));
        }
    }
    write_audit_json(&report, &"crates/pi-ai/data/pricing.json")?;
    Ok(())
}
```

We do **not** ship a tool that calls live LLMs to interpret the
search hits. The audit returns the raw search blobs; updating the
table is a manual review step. The dogfood loop here uses pi (with
parallel.ai web_search) as the interpretive layer once, produces a
PR, and the PR is reviewed by a human.

### 2. `crates/pi-ai/data/pricing.json`

```jsonc
{
  "schema_version": 1,
  "audited_at":     "2026-04-28T00:00:00Z",
  "default_provider": "parallel",
  "rows": [
    {
      "provider":  "anthropic",
      "model":     "claude-opus-4-7",
      "input_cost_per_mtok":  15.00,
      "output_cost_per_mtok": 75.00,
      "verified_at":          "2026-04-28",
      "sources":  [
        "https://www.anthropic.com/pricing",
        "https://platform.claude.com/docs/en/about-claude/models/whats-new-claude-4-7"
      ]
    },
    /* one entry per row in default_providers() */
  ]
}
```

The file lives under version control. Future audits diff the file,
not the registry.

### 3. `default_providers()` consumes `pricing.json` at compile time

```rust
// registry.rs (sketch)
const PRICING_JSON: &str = include_str!("../data/pricing.json");

fn pricing_lookup(provider: &str, model: &str) -> Option<(f64, f64)> {
    static PRICING: once_cell::sync::Lazy<PricingTable> =
        once_cell::sync::Lazy::new(|| serde_json::from_str(PRICING_JSON).unwrap());
    PRICING.row(provider, model).map(|r| (r.input_cost_per_mtok, r.output_cost_per_mtok))
}

fn m(provider: &str, id: &str, alias: Option<&str>, ctx: u32, out: u32,
     thinking: bool, vision: bool) -> ModelInfo {
    let (in_cost, out_cost) = pricing_lookup(provider, id)
        .expect("every model must have a pricing row");
    ModelInfo { /* ... */ input_cost_per_mtok: in_cost, output_cost_per_mtok: out_cost }
}
```

The `(f64, f64)` arguments at the call site go away; the pricing
data lives in one file and is the single source of truth.

### 4. Tests

```rust
// registry.rs::tests
#[test]
fn every_model_has_non_zero_pricing() {
    let r = ModelRegistry::new(AuthStorage::in_memory());
    for p in r.providers() {
        for m in &p.models {
            assert!(m.input_cost_per_mtok  > 0.0, "{}/{} input zero", p.name, m.id);
            assert!(m.output_cost_per_mtok > 0.0, "{}/{} output zero", p.name, m.id);
        }
    }
}

#[test]
fn every_model_has_a_pricing_row_with_provenance() {
    let r = ModelRegistry::new(AuthStorage::in_memory());
    let table: PricingTable = serde_json::from_str(PRICING_JSON).unwrap();
    for p in r.providers() {
        for m in &p.models {
            let row = table.row(&p.name, &m.id)
                .unwrap_or_else(|| panic!("no pricing row for {}/{}", p.name, m.id));
            assert!(!row.sources.is_empty(),
                    "{}/{} pricing row has no source URLs", p.name, m.id);
        }
    }
}

#[test]
fn no_row_uses_the_legacy_placeholder_pair() {
    // Catches the (0.5, 1.5) "I forgot to fill this in" pattern.
    let r = ModelRegistry::new(AuthStorage::in_memory());
    for p in r.providers() {
        for m in &p.models {
            let placeholder = (m.input_cost_per_mtok - 0.5).abs() < 1e-9
                && (m.output_cost_per_mtok - 1.5).abs() < 1e-9;
            assert!(!placeholder, "{}/{} still has the (0.5, 1.5) placeholder",
                    p.name, m.id);
        }
    }
}
```

## Test plan

1. **Unit tests above** — three new tests in `registry.rs::tests`.
2. **`tests/pricing_provenance.rs`** — load `pricing.json`, assert
   it parses, every row has a `sources` array of length ≥ 1, and
   every URL is a well-formed HTTPS URL.
3. **End-to-end (gated on `PARALLEL_API_KEY`)** — run the audit
   binary, eyeball the resulting `pricing.json`, regenerate
   `default_providers()`, run the full pi-ai test suite. Skip when
   the key isn't set.

## Out of scope

- **Differential cache pricing** — Anthropic's 10 % / 125 % for
  cache_read / cache_write. RFD 0010 (defers cleanly off this one
  once `pricing.json` exists).
- **Tiered context-window pricing** — Anthropic Sonnet at $3 / $15
  for ≤200K and $6 / $22.50 for &gt;200K, Gemini at $1.25 / $5 for
  ≤200K and $2.50 / $10 for &gt;200K. The schema can grow a
  `tiers: [...]` field; v1 only stores the headline.
- **Per-region adjustments** for Bedrock, Vertex, Azure. The
  registry doesn't track regions today.

## Open questions

- **Should the audit run as a CI cron?** Lean yes (weekly), but
  needs a CI-side `PARALLEL_API_KEY`. Park as a follow-up.
- **What about provider rows that don't list a public price (e.g.
  enterprise-only Cerebras tiers)?** Mark them as
  `verified: "estimate"` in `pricing.json` and ship a fallback like
  `(input * 0.0, output * 0.0)` so they show up in stats as $0
  rather than fabricated. Decided.
