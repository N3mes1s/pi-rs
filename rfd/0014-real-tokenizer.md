# RFD 0014 — Real tokenizer for `ContextLoad.tokens` (and Usage estimates)

- **Status:** Discussion
- **Author:** pi-rs maintainers
- **Created:** 2026-04-28
- **Implemented:** &lt;pending&gt;

## Summary

`SessionEntryKind::ContextLoad.tokens` is filled by the bytes/4
estimate landed in RFD 0012. That's correct enough for the judge
to rank context size, but wildly wrong on real text (a token is
typically 3.5–4.5 bytes for English, 2 for code-heavy sources,
1 for Asian languages). The pi-stats dashboard's
`total_input_tokens` already comes from the provider's authoritative
count via `Usage`, so this only affects the *estimated* fields:
`ContextLoad.tokens`, the flamegraph block widths, and any
heuristic the evolve loop uses to decide "AGENTS.md too long."

This RFD swaps the estimate for a real BPE-based tokenizer
(`tiktoken-rs` for OpenAI's `cl100k_base`/`o200k_base`; same
encoding works as a close approximation for Claude per Anthropic's
own tooling). One small `pi_ai::tokenizer` module, one workspace
dep.

## Background

* RFD 0012 added `estimate_tokens(s: &str) -> Option<u64> { Some(s.len() / 4) }`.
* Anthropic publishes `claude_tokenize` in the Python SDK; the
  Rust port doesn't. The closest open-source approximation is
  `tiktoken-rs` (the Rust port of OpenAI's tiktoken), which
  ships `cl100k_base` (GPT-4) + `o200k_base` (GPT-4o) BPE merges
  baked in.
* For a 4 KB AGENTS.md the estimate is ~1024 tokens; the real
  count via tiktoken `cl100k_base` is ~1100 (within 8 %). For a
  200 KB diff full of Rust code, bytes/4 says ~50 000 but the
  real tokenizer says ~70 000 — a 40 % undercount.

## Proposal

### 1. Add `tiktoken-rs` workspace dep

```toml
# Cargo.toml [workspace.dependencies]
tiktoken-rs = "0.5"
```

`tiktoken-rs` is pure Rust (no Python), single crate, ~3 MB of
embedded BPE merges, no dynamic libraries.

### 2. New `pi_ai::tokenizer` module

```rust
// crates/pi-ai/src/tokenizer.rs
use std::sync::OnceLock;

/// Tokenizer family. We curate two: `Cl100kBase` covers GPT-4 +
/// approximation for Claude; `O200kBase` is the GPT-4o /
/// reasoning-model encoding. Anthropic-specific tokenizers aren't
/// open-source; cl100k is the closest practical proxy and is what
/// Anthropic's own cookbook examples use for offline estimation.
#[derive(Debug, Clone, Copy)]
pub enum TokenizerKind {
    Cl100kBase,
    O200kBase,
}

impl TokenizerKind {
    pub fn for_model(model_id: &str) -> Self {
        if model_id.starts_with("gpt-4o")
            || model_id.starts_with("o1")
            || model_id.starts_with("o3")
            || model_id.starts_with("o4")
            || model_id.starts_with("gpt-5")
        {
            Self::O200kBase
        } else {
            Self::Cl100kBase
        }
    }
}

pub fn count(kind: TokenizerKind, s: &str) -> u64 {
    static CL100K: OnceLock<tiktoken_rs::CoreBPE> = OnceLock::new();
    static O200K:  OnceLock<tiktoken_rs::CoreBPE> = OnceLock::new();
    let bpe = match kind {
        TokenizerKind::Cl100kBase => CL100K.get_or_init(|| tiktoken_rs::cl100k_base().unwrap()),
        TokenizerKind::O200kBase  => O200K.get_or_init(|| tiktoken_rs::o200k_base().unwrap()),
    };
    bpe.encode_with_special_tokens(s).len() as u64
}

/// Cheap fallback when no concrete model is in scope. Used by
/// `ContextLoad.tokens` when the RuntimeConfig hasn't resolved the
/// model yet.
pub fn count_default(s: &str) -> u64 {
    count(TokenizerKind::Cl100kBase, s)
}
```

### 3. Wire it in

```rust
// crates/pi-agent-core/src/runtime.rs::emit_context_loads
let tokens = pi_ai::tokenizer::count_default(&ctx.content);
self.cfg.session_manager.append(
    &self.id,
    SessionEntryKind::ContextLoad {
        source: ctx.path.display().to_string(),
        bytes: ctx.content.len() as u64,
        tokens: Some(tokens),
    },
)?;
```

```rust
// crates/pi-coding-agent/src/native/trajectory/flamegraph.rs
fn block_width(s: &str, kind: TokenizerKind) -> u64 {
    pi_ai::tokenizer::count(kind, s)
}
```

The model-aware variant (`for_model(model_id).count(s)`) lives on
`Trajectory::build` so per-turn block widths use the right BPE
merges when the session's model is known.

### 4. Drop the old `estimate_tokens`

The bytes/4 helper that RFD 0012 introduced is removed. Any caller
that was using it now goes through `pi_ai::tokenizer::count_default`.

## Test plan

1. **`tokenizer.rs` unit tests** — three known fixtures:
   * empty string → 0 tokens.
   * `"hello world"` → 2 tokens via cl100k (literal expectation
     pinned in test).
   * 4 KB English text → asserts the count is in
     [bytes/5, bytes/3] sanity range.
2. **`tokenizer_o200k_for_modern_openai_models`** — `for_model
   ("gpt-5")` returns `O200kBase`; `for_model("claude-opus-4-7")`
   returns `Cl100kBase`.
3. **`crates/pi-agent-core/tests/runtime_context_load.rs`** —
   extend RFD-0012's test: assert `tokens` is *not* exactly
   `bytes / 4` for a non-trivial fixture (regression guard
   that the real tokenizer is plumbed in).
4. **End-to-end**: re-run RFD-0012's smoke; the JSONL
   `context_load` line should show `tokens: ~1100` (was 1046),
   the flamegraph JSON block widths shift accordingly.

## Out of scope

- **Anthropic-native tokenizer.** Their tokenizer is closed; we
  use cl100k as a 4–8 % accurate proxy. Open follow-up if/when
  Anthropic ships a Rust port.
- **Per-language tokenization** (Asian languages, code-specific
  encodings). The two cl100k/o200k families cover the dominant
  use cases.
- **Token-budget plumbing for tool input/output truncation.** The
  RFD only swaps the estimate fn; runtime budgeting is RFD 0016.
- **Caching the tokenizer state across processes.** `OnceLock` per
  process is enough; the BPE init is ~50 ms cold and free
  thereafter.

## Open questions

- **Should `count` take `&str` or `&[u8]`?** Lean `&str` —
  callers always have UTF-8 text from JSONL or context files.
- **Should pi-stats re-estimate input_tokens via this tokenizer
  when the provider didn't fill the field?** Yes, but only for
  rows whose `input_tokens == 0 && model is known`. Out of scope
  here; tracked under RFD 0015's multi-provider Usage cleanup.
