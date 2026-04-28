# RFD 0008 — Populate every `Usage` field on stream finish

- **Status:** Discussion
- **Author:** pi-rs maintainers
- **Created:** 2026-04-27
- **Implemented:** &lt;pending&gt;

## Summary

The `pi_ai::message::Usage` struct has six fields (`input_tokens`,
`output_tokens`, `cache_read_tokens`, `cache_write_tokens`,
`reasoning_tokens`, `cost_usd`) but the streaming providers fill in
**only `output_tokens`** before emitting the `StreamEventKind::Usage`
event. Every downstream consumer — the agent runtime that writes
`SessionEntry::Usage` to JSONL, the trajectory recorder, the brand-new
`pi-stats` dashboard (RFD 0004) — therefore sees zero cost and zero
input tokens for every request.

This RFD wires up: (a) the *capture* (parse the SSE / JSON correctly
to read `input_tokens`, cache deltas, and reasoning tokens) and (b)
the *cost computation* (multiply by the model's per-MTok prices, which
are already on `ModelInfo`), so a fresh `pi --stats json` after this
lands shows real numbers.

## Background

Pi-stats (RFD 0004) lit this up. After running 6 sessions through
the integrated branch, `pi --stats json` returned:

```
"total_input_tokens":      0,
"total_output_tokens":     0,   ← actually a few hundred but rounded
"total_cache_read_tokens": 0,
"total_cost":              0.0
```

Source — `crates/pi-ai/src/provider/anthropic.rs:298-310`:

```rust
"message_delta" => {
    if let Some(usage) = data.get("usage") {
        let u = Usage {
            output_tokens: usage.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            ..Default::default()                    // ← input/cache/cost all left at 0
        };
        return Some(/* emit */);
    }
    ...
}
```

Same shape in `provider/openai.rs`, `provider/google.rs`,
`provider/bedrock.rs`. Cost has never been computed anywhere.

`ModelInfo` already carries the rates:
```rust
pub input_cost_per_mtok:  f64,    // dollars per million input tokens
pub output_cost_per_mtok: f64,
```
(See `crates/pi-ai/src/registry.rs:19-20`.)

## Proposal

### 1. Capture every field at the right SSE event

Anthropic's SSE protocol splits the usage data:

* `event: message_start` — the first frame carries
  `data.message.usage.{input_tokens, cache_creation_input_tokens,
  cache_read_input_tokens}`. **Fixed at message start.**
* `event: message_delta` — the closing frame's
  `data.usage.output_tokens` is the *running total* of output
  tokens. (Not a per-event delta — Anthropic re-sends the cumulative
  total so a late-arriving subscriber gets it right.)

We currently ignore `message_start.usage`. The fix is to handle it:

```rust
// anthropic.rs — new branch in the existing `match etype { ... }`
"message_start" => {
    if let Some(u) = data
        .get("message")
        .and_then(|m| m.get("usage"))
    {
        // Stash the start-of-stream counts in `acc`'s third slot so
        // the closing message_delta can fold them into one Usage.
        usage_running.input_tokens     = u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        usage_running.cache_read_tok   = u.get("cache_read_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        usage_running.cache_write_tok  = u.get("cache_creation_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
    }
    return Some((Ok(StreamEvent::new(StreamEventKind::MessageStart)), (es, acc, false)));
}
```

`usage_running` is a new field threaded through the `unfold` state
tuple alongside `(event_stream, tool_inputs, done)`. Today the tuple is
3-element; this RFD bumps it to 4 with a `UsageAcc` struct:

```rust
#[derive(Default, Clone, Copy)]
struct UsageAcc {
    input_tokens:    u64,
    cache_read_tok:  u64,
    cache_write_tok: u64,
    output_tokens:   u64,
    reasoning_tok:   u64,
}
```

The closing `message_delta` branch reads the running totals back:

```rust
"message_delta" => {
    if let Some(u) = data.get("usage") {
        usage_running.output_tokens = u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(usage_running.output_tokens);
    }
    let final_usage = Usage {
        input_tokens:       usage_running.input_tokens,
        output_tokens:      usage_running.output_tokens,
        cache_read_tokens:  usage_running.cache_read_tok,
        cache_write_tokens: usage_running.cache_write_tok,
        reasoning_tokens:   usage_running.reasoning_tok,
        cost_usd:           compute_cost(model, &usage_running),
    };
    return Some((
        Ok(StreamEvent::new(StreamEventKind::Usage { usage: final_usage })),
        (es, acc, false),
    ));
}
```

### 2. Cost computation helper

```rust
// anthropic.rs (and re-used in openai.rs etc. via a copy — providers
// stay independent on purpose; the helper is small)
fn compute_cost(model: &ModelInfo, u: &UsageAcc) -> f64 {
    let in_tok    = u.input_tokens + u.cache_read_tok + u.cache_write_tok;
    let out_tok   = u.output_tokens + u.reasoning_tok;
    (in_tok  as f64 / 1_000_000.0) * model.input_cost_per_mtok
  + (out_tok as f64 / 1_000_000.0) * model.output_cost_per_mtok
}
```

For Anthropic we conventionally bill cache_read at 10 % and
cache_write at 125 % of input rate, but **we punt on differential
pricing**: this RFD treats every input class the same. Differential
cache pricing is RFD 0009. The default still beats `0.0`.

### 3. OpenAI / Google / Bedrock — same shape

OpenAI's stream emits one final `usage` block in the closing chunk
when `stream_options.include_usage = true`. We currently send that
flag (provider/openai.rs around the request body); we just don't
read every field. Same fix as Anthropic: thread a `UsageAcc`, read
`prompt_tokens`, `completion_tokens`, `prompt_tokens_details
.cached_tokens`, `completion_tokens_details.reasoning_tokens`.

Google / Bedrock follow the same shape with their own field names
(`promptTokenCount` / `candidatesTokenCount` for Google;
`inputTokens` / `outputTokens` / `cacheReadInputTokens` for Bedrock).

### 4. Reasoning tokens

OpenAI reasoning models (`o1`, `o3`, `gpt-5*`) and Anthropic
extended-thinking responses both emit a separate `reasoning_tokens`
counter. Today we drop them. Add the field-read in each provider's
`message_delta`-equivalent branch.

For Anthropic specifically, the reasoning tokens are *included in*
`output_tokens` already (per spec), so we don't double-count. The
field stays at 0 for Anthropic and is populated only for OpenAI's
reasoning family. Document this behaviour in `Usage`'s doc comment.

## Test plan

1. **`tests/anthropic_stream.rs`** — extend the existing fake-SSE
   fixture (it already replays a recorded `message_start` →
   `content_block_*` → `message_delta` → `message_stop` flow) to
   include realistic `usage` blocks at both ends. Assert the emitted
   `Usage` has `input_tokens > 0`, `output_tokens > 0`, and
   `cost_usd > 0` after the helper applies.
2. **`tests/anthropic_more.rs`** — add a unit test for `compute_cost`
   that proves: 1M input + 1M output tokens → `input_cost_per_mtok +
   output_cost_per_mtok`; cache_read tokens count toward input.
3. **`tests/openai_stream.rs`** — same extension for OpenAI's
   `[DONE]`-terminated SSE stream.
4. **`crates/pi-stats/tests/ingest_session_jsonl.rs`** — add a
   fixture session JSONL whose `Usage` line carries a non-zero
   `cost_usd` and assert the row in `messages` has the same value.
5. **End-to-end (gated on `ANTHROPIC_API_KEY`)**: a single
   `pi -p "say hi"` followed by `pi --stats json` shows
   `total_cost > 0.0` and `total_input_tokens > 0`. Skip when the
   key isn't set.

## Out of scope

- **Differential cache pricing** — Anthropic charges 10 % / 125 % for
  read / write. RFD 0009.
- **Per-message cost stamping** — today only `Usage` events carry
  cost; the `Message`'s metadata doesn't. Could be useful for
  per-tool-call accounting. RFD 0010.
- **Provider-side cost adjustments** (Bedrock surge pricing, Vertex
  zone multipliers). The static cost-per-MTok in `ModelInfo` is
  intentionally a flat estimate.
- **Streaming usage updates** — Anthropic emits `output_tokens` once
  at the end. We do not surface intermediate "tokens-so-far"
  estimates; the runtime reports usage exactly once per turn. v1
  keeps that semantic.

## Open questions

- **Should `cost_usd` be re-derivable downstream from the token
  counts + the model id?** Yes, in principle (the static prices
  table is already in `ModelInfo`). But shipping the cost in the
  Usage event makes the `pi-stats` ingest a one-pass operation.
  Decision: keep cost in the Usage event; consumers that want to
  re-price can do so post-hoc.
- **Unify the four provider implementations into one helper?** They
  diverge on field names and on whether usage is at message_start
  or message_delta. Lean no — copy is honest, abstractions hide.
