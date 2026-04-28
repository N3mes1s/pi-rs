# AGENTS.md — pi-rs

This file is loaded into every `pi` agent session that runs inside
this repo (unless explicitly disabled with `--no-context-files`).
Its job is to encode the conventions a new agent would otherwise
have to relearn from the diff. The "Optimisation lessons" block at
the bottom is mutable — the `pi --evolve` daemon may rewrite it
based on observed outcomes.

## House rules
<!-- pi:keep -->
- Never use `--no-verify` or skip pre-commit hooks.
- Never push to `main` from a dogfood run. Branches are
  `claude/dogfood-<slug>` (one per RFD) or `claude/integrated-features`.
- Never soften an assertion to make a test pass. If the test is
  legitimately wrong, fix it; otherwise fix the code under test.
- The `(0.5, 1.5)` placeholder pricing pair in `crates/pi-ai/data/
  pricing.json` is a forgotten audit; never use it. Pick real
  prices from the provider's public list page.
- API keys never land in committed files. Always env-var; the
  static-musl binary reads them at startup.
<!-- /pi:keep -->

## Where things live
- **RFDs** in `rfd/`, indexed in `rfd/README.md`. New RFDs use
  `0000-template.md` and start at `Status: Discussion`. Status
  bumps to `Implemented` with the commit hash once shipped.
- **Pricing data** in `crates/pi-ai/data/pricing.json` (provenance
  + cache rates). `default_providers()` reads it via
  `include_str!`. Schema-version `2` includes
  `cache_read_cost_per_mtok` / `cache_write_cost_per_mtok`. RFDs
  0009 + 0010.
- **Token + cost roll-up** flows: provider `Usage` event →
  `SessionEntryKind::Usage` (in JSONL) → `pi-stats` ingest →
  `pi --stats json`. RFD 0008 + the runtime persistence follow-up.
  `pi-rs/crates/pi-ai/src/cost.rs::compute_cost` is the canonical
  cost helper (RFD 0010).
- **`task` tool subagents** (RFD 0005) need a `ParentHandle`
  registered around `session.prompt(...)` via
  `task::tool::with_runtime(...)`. Wired in `modes/print.rs` +
  `modes/json.rs`. Adding a third mode? Wire it.
- **Worktree isolation** (RFD 0006) lives at
  `~/.pi/wt/data/<encoded-repo>/<task-id>/`. `pi --worktree`
  honours `PI_WORKTREE_ROOT` for tests.

## Conventions
- Idiomatic Rust: `match` over `if-let-chain`, `?` over
  `.unwrap()`, `tracing::warn!` over `eprintln!` for
  non-fatal errors. Prefer named helpers to inline closures
  > 3 lines.
- Tests pair with source: a new file
  `crates/<x>/src/foo.rs` warrants a new
  `crates/<x>/tests/foo.rs` (or expand the existing one).
  Integration tests skip cleanly when their tool is absent
  (mirror `tests/lsp_real_rust_analyzer.rs`'s
  `which::which("rust-analyzer")` skip pattern).
- For dogfood-scope changes, keep diffs ≤ ~500 lines per
  commit. Multi-file features can land as one merge commit.
- Brace-check before `cargo build`. A missing closing `}`
  cascades through dozens of compile errors and wastes
  iteration budget.
- Static-musl is the production target. `bundled` features
  (rusqlite, etc.) are required so the binary self-contains.

## Current open RFDs
- 0002 — Tier-5 follow-ups (tracking).
- 0004 / 0005 / 0006 — pi-stats / subagents / worktree.
- 0007 — Per-language LSP formatting options.
- 0008 — Populate every Usage field on stream finish.
- 0009 — Audit + calibrate the model pricing table.
- 0010 — Differential cache pricing in `compute_cost`.
- 0011 — *this file*.

## Optimisation lessons
<!-- This block is mutable. The evolve daemon may rewrite it. -->
- Adaptive thinking on Opus 4.7 is much cheaper than legacy
  enabled-mode thinking; the runtime now picks the right shape
  per model id (RFD 0003).
- The pricing table was stale by 2-10× across many providers
  before RFD 0009. Always trust `pricing.json` over baked-in
  literals.
- The `task` tool's `ParentHandle` plumbing is one of the most
  forgotten wiring steps; if a new mode handler can't reach
  subagents, that's almost certainly the gap.
- Pi-rs's `Usage` event historically populated only
  `output_tokens`; full token + cost data didn't surface in
  `pi --stats json` until RFD 0008.
- `--worktree` + `--worktree-mode patch` is the safer reconcile
  default for parallel dogfoods (one branch can't conflict with
  another).
