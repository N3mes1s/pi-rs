# RFD 0010 — Differential cache pricing in `compute_cost`

- **Status:** Implemented
- **Author:** pi-rs maintainers
- **Created:** 2026-04-28
- **Implemented:** 1c1fb41

## Summary

Today `compute_cost(model, &UsageAcc)` (RFD 0008) bills every input
token at the same flat `input_cost_per_mtok` rate, including
`cache_read_tokens` and `cache_write_tokens`. The major providers
charge differential rates for cache traffic — Anthropic at 10 %
(read) / 125 % (write), OpenAI at 50 % (read), Google at 25 %
(read) — so pi's `pi --stats json` overstates spend the moment a
session uses prompt caching. This RFD extends `pricing.json` and
`ModelInfo` with optional explicit cache rates and reroutes
`compute_cost` to use them.

Defaults stay equivalent to today (no surprise), so a model whose
pricing row doesn't specify cache rates still bills cache traffic at
the input rate. The win is per-row opt-in — every Anthropic model
declares its 0.10× / 1.25× multipliers; OpenAI and Google add their
own where applicable.

## Background

Live evidence the gap is real. Yesterday's smoke run:

```
pi -p "say exactly: real-prices-now"
pi --stats sync && pi --stats json
→ input_tokens: 5105, cache_read: 0, cost: $0.025775
```

The 5105 input tokens were all freshly-written; cost matched the
`5105 / 1_000_000 × $5` calculation. Now run a second turn against
the same conversation: the system prompt + earlier turns get
`cache_read` hits, and the row in pi-stats charges them at $5 /
MTok instead of $0.50 / MTok. For long-running interactive sessions
this is a 10× over-attribution on the cached input.

Reference rates (post-RFD-0009 audit):

| Provider  | cache_read | cache_write |
|-----------|------------|-------------|
| Anthropic | 0.10× input | 1.25× input (5-min ephemeral); 2× input (1-hour) |
| OpenAI    | 0.50× input (`cached_input` on supported models) | n/a |
| Google    | 0.25× input (Gemini "context cache") | n/a |
| Bedrock   | mirrors Anthropic on the Anthropic SKUs | — |

(The "1-hour cache write" tier at 2× input is a v2 concern; v1 ships
the 5-min ephemeral 1.25× rate, which is what every existing
Anthropic SDK example uses. Tracked in Open questions.)

## Proposal

### 1. Extend `pricing.json` schema

```jsonc
{
  "schema_version": 2,
  "audited_at":     "2026-04-28T00:00:00Z",
  "rows": [
    {
      "provider":   "anthropic",
      "model":      "claude-opus-4-7",
      "input_cost_per_mtok":         5.0,
      "output_cost_per_mtok":        25.0,
      "cache_read_cost_per_mtok":    0.50,    // = 0.10 × input
      "cache_write_cost_per_mtok":   6.25,    // = 1.25 × input
      "verified": "confirmed",
      "verified_at": "2026-04-28",
      "sources": [ "https://platform.claude.com/docs/en/about-claude/pricing" ]
    }
  ]
}
```

Both new fields are optional. Pricing rows that omit them inherit
the input rate (today's behaviour). `schema_version` bumps to `2` so
older clients can detect the field expansion.

### 2. Extend `ModelInfo`

```rust
// crates/pi-ai/src/registry.rs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub provider:                String,
    pub id:                      String,
    pub alias:                   Option<String>,
    pub context_window:          u32,
    pub max_output_tokens:       u32,
    pub supports_thinking:       bool,
    pub supports_tools:          bool,
    pub supports_vision:         bool,
    pub input_cost_per_mtok:     f64,
    pub output_cost_per_mtok:    f64,
    /// Per-million tokens for `cache_read_input_tokens`. Falls back
    /// to `input_cost_per_mtok` when `None`. RFD 0010.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_cost_per_mtok:  Option<f64>,
    /// Per-million tokens for `cache_creation_input_tokens` (a.k.a.
    /// "cache write"). Falls back to `input_cost_per_mtok` when
    /// `None`. RFD 0010.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_cost_per_mtok: Option<f64>,
}
```

The `m()` constructor in `default_providers()` reads the optional
columns from `pricing.json` (which post-RFD-0009 is loaded via
`include_str!`). Rows without cache pricing keep the current call
shape; rows with it get the override.

### 3. Rewire `compute_cost`

```rust
// crates/pi-ai/src/provider/anthropic.rs
fn compute_cost(model: &ModelInfo, u: &UsageAcc) -> f64 {
    let cache_read_rate  = model.cache_read_cost_per_mtok
        .unwrap_or(model.input_cost_per_mtok);
    let cache_write_rate = model.cache_write_cost_per_mtok
        .unwrap_or(model.input_cost_per_mtok);
    let fresh_input  = u.input_tokens;
    let cached_in    = u.cache_read_tok;
    let cached_write = u.cache_write_tok;
    let out_tok      = u.output_tokens + u.reasoning_tok;
    let in_dollars =
        (fresh_input  as f64 / 1_000_000.0) * model.input_cost_per_mtok
      + (cached_in    as f64 / 1_000_000.0) * cache_read_rate
      + (cached_write as f64 / 1_000_000.0) * cache_write_rate;
    let out_dollars =
        (out_tok      as f64 / 1_000_000.0) * model.output_cost_per_mtok;
    in_dollars + out_dollars
}
```

The previous fold-everything-into-input behaviour was wrong: it
double-billed reads and undercharged writes. The RFD's "Cost
computation helper" subsection in 0008 explicitly deferred this to
RFD 0010, which we're now landing.

### 4. Update Anthropic rows in `pricing.json`

Pi (the audit dogfood) populates the cache rates per the public
table:

| model                          | cache_read | cache_write |
|--------------------------------|------------|-------------|
| `claude-opus-4-7`              | 0.50       | 6.25        |
| `claude-sonnet-4-6`            | 0.30       | 3.75        |
| `claude-haiku-4-5-20251001`    | 0.10       | 1.25        |
| Bedrock mirrors of the above   | same       | same        |

OpenAI rows that support prompt caching (gpt-4o, gpt-4o-mini, o1,
o3, o4-mini, gpt-5*) get `cache_read_cost_per_mtok = input × 0.5`.
OpenAI doesn't bill a separate write rate, so leave
`cache_write_cost_per_mtok` as `None` (falls back to input).

Gemini 2.5 Pro / Flash: `cache_read_cost_per_mtok = input × 0.25`.

Other providers (Cerebras, Groq, xAI, DeepSeek, Mistral, Z.ai,
Kimi/Moonshot) don't expose prompt caching today; leave both fields
`None`.

## Test plan

1. **`tests/anthropic_cost.rs`** (extend existing) — three new asserts:
   - With `cache_read_cost_per_mtok = Some(0.50)` and 1M cache_read
     tokens against Opus 4.7 (input 5.0), cost = $0.50.
   - With `cache_write_cost_per_mtok = Some(6.25)` and 1M
     cache_write tokens, cost = $6.25.
   - When both fields are `None`, cache_read/cache_write tokens
     bill at input rate (regression guard for the RFD-0008
     fallback path).
2. **`tests/pricing_provenance.rs`** (extend existing) — assert
   that for every Anthropic row the cache fields are populated
   (mandatory for that provider going forward).
3. **End-to-end**: `pi -p "<short>"` twice in one session; the
   second turn's `Usage` line records non-zero `cache_read_tokens`;
   `pi --stats sync && pi --stats json` shows
   `total_cache_read_tokens > 0` and the cost reflects the 0.10×
   discount.

## Out of scope

- **1-hour ephemeral cache write tier** (Anthropic 2× input).
  Today the Usage event doesn't distinguish 5-min vs 1-hour; punt
  to a follow-up RFD that grows `UsageAcc` with a third
  `cache_write_long` field.
- **OpenAI cached output / structured-output discounts.** Same
  schema can hold them; v1 only ships cache_read/write.
- **Per-region multipliers** (Bedrock, Vertex, Azure). Already
  out-of-scope per RFD 0009.
- **Tiered context-window pricing**
  (Anthropic >200K = 2× input). v1 honours the lower tier; tiered
  cost-by-token-position is RFD 0011.

## Open questions

- **Should the schema express cache rates as multipliers
  (`cache_read_multiplier: 0.10`) instead of absolute dollars
  (`cache_read_cost_per_mtok: 0.50`)?** Multipliers stay correct
  when the input rate moves; absolutes are easier to read at a
  glance and self-document. Lean absolute, with a unit test that
  asserts Anthropic rows satisfy `0.09 ≤ read/input ≤ 0.11`.
- **Should `compute_cost` move out of `provider/anthropic.rs` and
  into `crates/pi-ai/src/cost.rs` so OpenAI / Google / Bedrock can
  reuse it once they get their RFD-0008-style Usage population?**
  Yes; do it as part of this RFD's prep so the new code lives in
  one place from day one.
