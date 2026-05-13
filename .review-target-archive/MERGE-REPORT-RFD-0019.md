# Merge Report — RFD 0019 (OpenAI Responses API)

Coordinated landing of three feature branches onto `main`:

1. `claude/responses-registry` → 22fa110
2. `claude/responses-core`     → f88bfa1
3. `claude/responses-tests`    → (final merge commit)

End state: `main` carries the canonical `ApiKind` enum on
`ModelInfo`, the real `/v1/responses` request builder + 14-arm
SSE event router, and the golden-fixture test suite (gated
behind `cfg(rfd_0019_responses)` and inert by default per RFD).

## Order rationale

Picked **registry → core → tests**. The core branch ships a
self-described local `ApiKind` stub + model-id heuristic that
must be superseded by the registry branch's canonical enum.
Landing registry first makes the core merge a pure conflict on
the dispatch site in `provider/openai.rs` (resolved by keeping
the registry-style `match model.api_kind` shape and routing the
`Responses` arm to the real `stream_responses` function from the
new `openai_responses` module). Tests are a clean additive merge.

## Code review

The bundled `code-reviewer` subagent (`gpt-5.4`, thinking high)
**failed to invoke** with a 4xx from the upstream OpenAI API:

```
provider error 400: Function tools with reasoning_effort are not
supported for gpt-5.4 in /v1/chat/completions. Please use
/v1/responses instead.
```

This is precisely the production bug RFD 0019 exists to fix —
gpt-5.4 must dispatch through `/v1/responses`, but the runtime
on `main` (pre-merge) was still routing it through Chat
Completions. The reviewer infra cannot work until this drop
lands. Per the task brief ("Don't block the campaign on
reviewer infra"), reviews were performed by orchestrator
judgement on each branch's diff + commit log:

| Branch                       | Verdict                  | Notes |
|------------------------------|--------------------------|-------|
| `claude/responses-registry`  | READY_TO_MERGE (judged)  | Adds `ApiKind` (Default = ChatCompletions, `#[serde(default)]`), `api_kind` field on `ModelInfo`, `with_responses_api()` helper, marks o3/o3-pro/o4-mini/gpt-5/gpt-5.4/gpt-5-mini/gpt-5-nano as Responses. Updates 17 test ModelInfo literals. Adds dedicated `registry_api_kind.rs` test (3 tests, all passing). Adds `gpt-5.4` row to pricing.json. |
| `claude/responses-core`      | READY_TO_MERGE (judged)  | New 630-line `provider/openai_responses.rs`: request body, message conversion, tool conversion, SSE event router. Local `ApiKind` stub explicitly documented as superseded once registry merges; left in place (still `pub`) to preserve the standalone-build promise — harmless under the registry-branch dispatch. |
| `claude/responses-tests`     | READY_TO_MERGE (judged)  | 4 new test files (132 + 114 + 157 + 37 lines) + 6 SSE / JSON fixtures. All gated behind `#![cfg(rfd_0019_responses)]` so they're inert in the default build (warning-only `unexpected_cfg`). |

## Conflicts resolved

1. **`crates/pi-ai/src/registry.rs`** (registry merge): `main`
   already carried `gpt-5.4` (commit d51de07) without responses
   routing; the branch carried it with `with_responses_api(...)`.
   Mechanical: kept the branch's `with_responses_api(with_cache(...))`
   form — RFD 0019 requires gpt-5.4 to use Responses.
2. **`crates/pi-ai/src/provider/openai.rs`** (core merge):
   - `main` had `match model.api_kind { ChatCompletions => stream_chat_completions, Responses => self.stream_responses (stub) }`.
   - The branch had a heuristic `if pick_api_kind(model) == Responses { return openai_responses::stream_responses(...) }` falling through to Chat Completions.
   - Mechanical resolution: kept the registry-style `match`,
     routed the `Responses` arm to the real
     `super::openai_responses::stream_responses(self, req, model)`,
     dropped the registry branch's `Err(Unsupported(...))` stub.
3. **`crates/pi-agent-core/tests/compaction.rs`** and
   **`compaction_extra.rs`**: post-merge compile errors —
   missing `api_kind` field on `ModelInfo { ... }` test
   literals. The registry branch had updated 17 such literals
   in `pi-ai/tests/` but missed two in sibling crates. Treated
   as a mechanical follow-on of the same migration; added
   `api_kind: Default::default(),` to both. (Surfaced as
   feedback for a follow-up branch — should arguably have been
   caught in the original migration.)

## Test results

`cargo test --workspace` cannot run end-to-end on this VM
because the `lsp_real_rust_analyzer` and
`lsp_write_tool_real_rust_analyzer` test binaries panic in
their setup before `--skip` filters apply (rust-analyzer
toolchain not installed — known environmental flake noted in
the task brief). Disk pressure during incremental linking
also forced a `cargo clean` mid-run.

Validated suites (run with `CARGO_INCREMENTAL=0`):

| Crate          | Result     |
|----------------|------------|
| `pi-ai`        | **PASS**   — every test binary green, including the new `registry_api_kind.rs` (3/3) |
| `pi-agent-core`| **PASS**   — green after the `api_kind` field follow-on |
| `pi-stats`     | **PASS**   |
| `pi-tools`     | **PASS**   |
| `pi-coding-agent` | **NOT RUN** — `lsp_real_rust_analyzer` test binary panics before `--skip` (env flake, unrelated to this drop) |

The new RFD 0019 tests in `pi-ai/tests/openai_responses_*.rs`
and `openai_dispatch_router.rs` remain inert behind the
`cfg(rfd_0019_responses)` gate, as designed by the RFD.

## What did **not** land

- Nothing was abandoned. All three branches merged.
- The reviewer subagent verdicts are unavailable until this
  drop reaches a runtime that can dispatch gpt-5.4 to
  `/v1/responses`.

## Follow-up suggestions (not actioned here)

- Add `crates/pi-agent-core/Cargo.toml` and
  `crates/pi-coding-agent/Cargo.toml` to the registry branch's
  field-migration coverage so future required-field additions
  to `ModelInfo` don't repeat this gap.
- Flip the `cfg(rfd_0019_responses)` gate on (or convert the
  tests to unconditional) once the parser symbols stabilise —
  they already reference public items added by `responses-core`.
- Drop the local `ApiKind` stub + `pick_api_kind` from
  `provider/openai_responses.rs`; it is now dead code shadowed
  by `registry::ApiKind`.
