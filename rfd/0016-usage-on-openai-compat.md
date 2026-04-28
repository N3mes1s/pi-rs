# RFD 0016 — Usage population on `OpenAiCompatProvider`

- **Status:** Implemented
- **Author:** pi-rs maintainers
- **Created:** 2026-04-29
- **Implemented:** 643780f

## Summary

RFD 0015 explicitly listed the OpenAI-compatible umbrella provider —
`OpenAiCompatProvider`, which fronts Cohere, Mistral, Z.ai, Kimi,
Cerebras, Groq, xAI, and DeepSeek — as out of scope. This RFD pays
that debt down by pinning a regression test that the
RFD-0008 / RFD-0015 `UsageAcc` plumbing also fires for every provider
routed through `OpenAiCompatProvider`, so a future refactor that
forks the compat path can't silently regress `total_cost: 0` for
eight production providers.

## Background

`OpenAiCompatProvider` lives in `crates/pi-ai/src/provider/openai.rs`
as a thin newtype around `OpenAiProvider` whose `stream` method
delegates verbatim:

```rust
async fn stream(&self, req: GenerateRequest, model: &ModelInfo)
    -> Result<EventStream> {
    self.0.stream(req, model).await
}
```

Because RFD 0015 (commit `f9a59c8`) threaded `UsageAcc` through
`OpenAiProvider::stream` and wired every closing-chunk `usage.*`
field, the same fix transitively applies to every
`ProviderKind::OpenAiCompat` registry entry — Cohere, Mistral, Z.ai,
Kimi, Cerebras, Groq, xAI, and DeepSeek (per `registry.rs`).

The hazard isn't today's wire-up; it's tomorrow's. The compat path
is a likely candidate for divergence (per-provider quirks in
`reasoning_content` shape, non-standard `usage` keys, etc.). Without
a test that exercises the compat newtype directly, a future patch
could re-introduce the pre-RFD-0008 broken shape for the eight
downstream providers and only get caught in production.

## Proposal

No code change to the provider — the fix is already in place via
delegation. This RFD locks the behaviour in with one new
integration test exercising `OpenAiCompatProvider` (not
`OpenAiProvider`) end-to-end:

* Mock an OpenAI-Chat-Completions-compatible SSE stream whose
  closing chunk carries the full `usage` block from RFD 0015's
  test plan §1 (`prompt_tokens: 1234, completion_tokens: 56,
  prompt_tokens_details.cached_tokens: 100,
  completion_tokens_details.reasoning_tokens: 20`).
* Drive it through `OpenAiCompatProvider::generate` with a
  priced `ModelInfo` (gpt-5-style $1.25 / $10 per Mtok).
* Assert every Usage field is non-zero **and** `cost_usd > 0`.

If a future RFD does fork the compat code path into a dedicated
`openai_compat.rs` module, this test moves with it and continues to
guard the contract.

## Test plan

* New file `crates/pi-ai/tests/openai_compat_stream.rs`:
  `openai_compat_closing_chunk_populates_every_usage_field` —
  asserts `input_tokens=1234, output_tokens=56, cache_read=100,
  reasoning=20, cost_usd > 0` against a `gpt-5`-priced model fed
  through `OpenAiCompatProvider` (not `OpenAiProvider`).
* Existing RFD-0015 OpenAI test (`openai_stream.rs`) continues to
  guard the parent provider.

## Out of scope

* **Per-compat-provider quirks** (Cohere `tool_use` shape, Mistral
  `prefix` flag, etc.) — picked up by future RFDs once a real
  divergence is observed.
* **Forking `openai_compat.rs` into its own module.** Today the
  newtype is two lines; extracting it costs more than it earns. If
  any compat provider ever needs a divergent SSE handler, that's
  the moment to split — not before.

## Open questions

None. The contract is identical to RFD 0015; this RFD just plants
a flag in the ground.
