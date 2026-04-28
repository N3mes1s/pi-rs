# RFD 0015 — Replicate Usage population to OpenAI / Google / Bedrock

- **Status:** Discussion
- **Author:** pi-rs maintainers
- **Created:** 2026-04-28
- **Implemented:** &lt;pending&gt;

## Summary

RFD 0008 fixed the Anthropic provider's `Usage` event to populate
every field (input, output, cache_read, cache_write, reasoning,
cost). RFD 0010 then made `compute_cost` cache-aware. The other
three streaming providers — OpenAI, Google Gemini, and Bedrock —
still emit the broken pre-RFD-0008 shape (only `output_tokens`,
zero everything else). Any user running `pi --provider openai` or
`gpt-5*` sees `total_cost: 0` in `pi --stats json` for the same
reason Anthropic did.

This RFD applies the same `UsageAcc`-threaded-through-stream::unfold
pattern to the three remaining providers, populates their cache
fields where applicable (OpenAI + Gemini do report
`cached_tokens`), and reuses the `compute_cost` helper extracted
into `pi_ai::cost` by RFD 0010.

## Background

Provider-specific shapes:

* **OpenAI** — closing `[DONE]`-terminated SSE stream's last
  non-DONE chunk has `usage: { prompt_tokens, completion_tokens,
  prompt_tokens_details: { cached_tokens },
  completion_tokens_details: { reasoning_tokens } }`. The
  request body must carry `stream_options: { include_usage: true }`
  for the field to appear. `provider/openai.rs` already sends that
  flag; we just don't read every field.
* **Google Gemini** — every SSE chunk has a `usageMetadata` block
  whose final value is the cumulative count:
  `{ promptTokenCount, candidatesTokenCount,
  cachedContentTokenCount, thoughtsTokenCount }`. We currently
  emit one Usage event but only `output_tokens` (= candidates).
* **Bedrock** — the Anthropic-on-Bedrock SKU mirrors the Anthropic
  shape exactly; we can lift the RFD-0008 implementation
  verbatim, swapping only the URL + auth.

## Proposal

### 1. Move `UsageAcc` into `pi_ai::cost`

Today `UsageAcc` lives in `provider/anthropic.rs` (per RFD 0008).
RFD 0010 already moved `compute_cost` into `cost.rs`; this RFD
moves `UsageAcc` next to it so all four providers share one
struct:

```rust
// crates/pi-ai/src/cost.rs
#[derive(Default, Clone, Copy, Debug)]
pub struct UsageAcc {
    pub input_tokens:    u64,
    pub cache_read_tok:  u64,
    pub cache_write_tok: u64,
    pub output_tokens:   u64,
    pub reasoning_tok:   u64,
}

impl UsageAcc {
    pub fn into_usage(self, model: &ModelInfo) -> Usage {
        Usage {
            input_tokens:       self.input_tokens,
            output_tokens:      self.output_tokens,
            cache_read_tokens:  self.cache_read_tok,
            cache_write_tokens: self.cache_write_tok,
            reasoning_tokens:   self.reasoning_tok,
            cost_usd:           compute_cost(model, &self),
        }
    }
}
```

### 2. OpenAI

```rust
// crates/pi-ai/src/provider/openai.rs (sketch)
"data" branch in the unfold loop:
    if data.get("choices").and_then(|c| c.as_array()).map(|a| a.is_empty()).unwrap_or(false) {
        // Closing chunk with usage.
        if let Some(u) = data.get("usage") {
            usage_running.input_tokens =
                u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            usage_running.output_tokens =
                u.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            usage_running.cache_read_tok = u
                .get("prompt_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            usage_running.reasoning_tok = u
                .get("completion_tokens_details")
                .and_then(|d| d.get("reasoning_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            return Some((
                Ok(StreamEvent::new(StreamEventKind::Usage {
                    usage: usage_running.into_usage(&model_owned),
                })),
                (es, acc, false, usage_running),
            ));
        }
    }
```

### 3. Google Gemini

```rust
// crates/pi-ai/src/provider/google.rs (sketch)
// Every chunk carries cumulative usageMetadata; the last one wins.
if let Some(meta) = data.get("usageMetadata") {
    usage_running.input_tokens =
        meta.get("promptTokenCount").and_then(|v| v.as_u64()).unwrap_or(0);
    usage_running.output_tokens =
        meta.get("candidatesTokenCount").and_then(|v| v.as_u64()).unwrap_or(0);
    usage_running.cache_read_tok = meta
        .get("cachedContentTokenCount")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    usage_running.reasoning_tok = meta
        .get("thoughtsTokenCount")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    // Don't emit until the *terminal* chunk; check finishReason.
    // (Detailed fan-out + emission rules in §3 of test plan.)
}
```

Gemini emits one Usage event per stream — matching Anthropic and
OpenAI's contract.

### 4. Bedrock

The Anthropic-on-Bedrock streaming format is identical to direct
Anthropic. The RFD-0008 anthropic.rs implementation moves into a
shared helper:

```rust
// crates/pi-ai/src/provider/anthropic_stream.rs (new shared module)
pub(super) fn handle_anthropic_event(
    etype: &str,
    data: &Value,
    usage_running: &mut UsageAcc,
) -> Option<StreamEventKind>;
```

`anthropic.rs` and `bedrock.rs` both call it from their unfold
loops. No duplicated parsing.

## Test plan

1. **OpenAI streaming Usage** —
   `crates/pi-ai/tests/openai_stream.rs` extension: feed a
   recorded SSE flow with a non-trivial closing-chunk `usage`
   block (`prompt_tokens: 1234, completion_tokens: 56,
   prompt_tokens_details.cached_tokens: 100,
   completion_tokens_details.reasoning_tokens: 20`); assert the
   emitted Usage has every field set + cost > 0 against a known
   `ModelInfo` (gpt-5 at $1.25 / $10).
2. **Google streaming Usage** —
   `crates/pi-ai/tests/google_stream.rs` extension: feed three
   chunks where each carries cumulative `usageMetadata`; assert
   one (and only one) Usage event with the final cumulative
   values.
3. **Bedrock parity** —
   `crates/pi-ai/tests/bedrock_extra.rs` extension: same SSE
   fixture as Anthropic; assert byte-for-byte same Usage shape.
4. **`UsageAcc::into_usage` round-trip** —
   `crates/pi-ai/tests/anthropic_cost.rs`: assert
   `UsageAcc { input: 1M, output: 1M, cache_read: 1M, ... }
   .into_usage(&opus_4_7)` matches the expected dollar number
   computed via `compute_cost` directly.
5. **End-to-end (gated on each provider's key)** —
   `pi -p "<short>" --provider openai --model gpt-5` then
   `pi --stats json` shows `total_input_tokens > 0` and
   `total_cost > 0`. Skip when `OPENAI_API_KEY` etc. unset.

## Out of scope

- **Cohere / Mistral / Z.ai / Kimi.** Those providers go through
  `OpenAiCompatProvider`, which is its own SSE handler. RFD 0018
  picks that up.
- **Re-deriving cost downstream when `Usage.cost_usd` was zero.**
  Once this RFD lands, every supported streaming provider fills
  the field; there's no need for a fallback re-deriver.
- **Tokenizer-based estimation when the provider returned zero.**
  RFD 0014 is the right place for that.

## Open questions

- **Should we emit a second Usage event mid-stream as the running
  totals update?** No — every consumer (judge, stats, runtime
  persistence) wants exactly one Usage line per assistant turn.
  Stick with the closing-event-only contract from RFD 0008.
- **Bedrock's "anthropic.claude-…" model id format vs. the
  `anthropic/claude-opus-4-7` id used to look up rates** — already
  handled by `pricing_lookup` (RFD 0009 schema) which keys on
  `(provider, model)` tuples.
