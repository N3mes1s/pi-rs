# RFD 0003 — Adaptive thinking (Opus 4.7+)

- **Status:** Implemented
- **Author:** pi-rs maintainers
- **Created:** 2026-04-27
- **Implemented:** 770f8b3

## Summary

Teach the anthropic provider to emit the new "adaptive" thinking
request shape for Opus 4.5/4.6/4.7 and Sonnet 4.6, while preserving
the legacy `budget_tokens`-based shape for Haiku 4.5 and any older
Claude models. Selection is driven by a single
`uses_adaptive_thinking()` predicate keyed on the model id.

## Background

Anthropic's newer Claude generations (Opus 4.5, 4.6, 4.7 and Sonnet
4.6) replaced the legacy extended-thinking API
(`{thinking: {type: "enabled", budget_tokens: N}}`) with an *adaptive*
shape: `{thinking: {type: "adaptive"}, output_config: {effort: "low"
| "medium" | "high"}}`. The model decides how many thinking tokens to
spend; the caller only picks an effort tier.

Before this fix, pi-rs always sent the legacy shape. On Opus 4.7 the
provider returned `HTTP 400 invalid request: thinking.budget_tokens`
and — separately — pi's agent loop hung indefinitely instead of
surfacing the error and exiting the turn. The combination made
`pi --thinking medium --model claude-opus-4-7 …` look like a freeze.

## Proposal

Add a small `uses_adaptive_thinking(model_id: &str) -> bool` predicate
in `crates/pi-ai/src/provider/anthropic.rs` that returns `true` for
Opus 4.5/4.6/4.7 and Sonnet 4.6 (substring match on the canonical
model ids) and `false` otherwise. The request builder branches on it:
adaptive models get `thinking: {type: "adaptive"}` plus
`output_config: {effort: <low|medium|high>}` derived from the
`--thinking` CLI flag; legacy models keep the existing
`thinking: {type: "enabled", budget_tokens: N}` block.

The legacy path is preserved verbatim — Haiku 4.5 and any older
Claude variants are unaffected. `--thinking off` is still a no-op on
both paths (no `thinking` / `output_config` fields emitted).

## Test plan

Three new unit tests in `crates/pi-ai/src/provider/anthropic.rs`:

- `adaptive_thinking_model_detection` — exhaustively checks the
  predicate across the supported model id matrix.
- `thinking_fields_adaptive_for_opus_4_7` — builds a request for
  `claude-opus-4-7` with `--thinking medium` and asserts the JSON
  contains `thinking.type == "adaptive"` and
  `output_config.effort == "medium"`, with no `budget_tokens`.
- `thinking_fields_legacy_for_haiku` — same exercise on
  `claude-haiku-4-5`, asserting the legacy
  `{type: "enabled", budget_tokens: N}` shape is preserved and that
  `output_config` is absent.

Live smoke test (manual, requires API key):

    pi --thinking medium --model claude-opus-4-7 -p "say exactly: ..."

The command must return a normal completion (no HTTP 400, no hang).

## Out of scope

- The agent-loop bug where a stream that fails with HTTP 400 from the
  provider hangs the pi process instead of surfacing the error and
  ending the turn. The adaptive-thinking fix removes the immediate
  trigger, but the underlying loop bug is real and should be tracked
  in a follow-up RFD.
- Provider-side feature detection (e.g. asking the API which shape a
  model expects). The static predicate is sufficient until Anthropic
  ships a discovery endpoint.

## Open questions

- The adaptive API also accepts `effort: "max"`. pi-rs's `--thinking`
  CLI flag is currently `off | low | medium | high`; bumping that
  enum to expose `max` is a separate, user-facing change and is
  deliberately not done here.
