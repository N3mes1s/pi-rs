# Merge Campaign Report

Date: 2026-04-28T08:00:09Z
Repo: pi-rs
Operator: orchestrator agent


## Reviewer subagent failure (campaign-wide)

The bundled `code-reviewer` subagent (gpt-5.4, OpenAI) failed on
first invocation with HTTP 400:

> Unsupported parameter: 'max_tokens' is not supported with this
> model. Use 'max_completion_tokens' instead.

This is a pi-rs runtime bug in the OpenAI provider's request
shape for the gpt-5.4 family (reasoning-only models). It is
independent of any branch in this campaign.

Per the operator's hard rules ("if invocation fails with ... 4xx,
surface the error ... skip the reviewer step and merge based on
commit message alone if necessary; flag this in the report"),
the orchestrator proceeds without per-branch LLM review. Each
branch's diff was inspected manually before merge.

## claude/bugfix-ask-headless
verdict: PASS (manual review; reviewer offline)
merge: MERGED fe959e6
summary: Registers `AskTool` only when `effective_mode() == Interactive`; mirrors how `approve` / `judge` are wired. Adds two registry-presence tests. Small, surgical, idiomatic.

## claude/bugfix-stats-requests
verdict: PASS (manual review)
merge: MERGED 9924052
summary: Adds `total_sessions` / per-model `sessions` (COUNT DISTINCT session_file). HTML dashboard column added. Tests updated. Clean column-index reshuffle.

## claude/bugfix-flame-cost
verdict: PASS (manual review)
merge: MERGED c9e0dd0
summary: Per-Usage cost attribution for multi-round turns. Tracks the most recent assistant_text index and assigns each Usage's cost_usd to it. Two new tests covering multi-round and no-usage cases.

## claude/bugfix-judge-pricing
verdict: PASS (manual review)
merge: MERGED 800a99a
summary: Judge prompt now includes `<system_prompt_size>` so the smol judge knows when an answer is plausibly grounded in baked-in context (AGENTS.md, pricing). Added rule + builder method + plumbed bytes from runtime config. New test for the size block.

## claude/quartet-validate
verdict: PASS (manual review; doc + artifacts only)
merge: MERGED b21d79e
summary: Adds VALIDATION.md empirical run report for RFD 0011–0013 plus supporting artifacts (stats.json, flamegraph.json, runs.log, evolve.txt). No code changes.

## claude/quartet-docs
verdict: PASS (manual review; README rewrite)
merge: MERGED 0b3b653
summary: Top-level README rewritten as feature walkthrough. No code touched.

## claude/quartet-realtask
verdict: PASS (manual review)
merge: MERGED 6f8f2f9
summary: New `/cost` slash command. Pure formatter in `slash_cost.rs` + async wrapper that drives `pi_stats::ingest::sync_all` + `by_folder`. Wired in both interactive TUI and line-mode handlers. Builtin registered. Two unit tests + integration test.

## claude/quartet-debt
verdict: PASS (manual review)
merge: MERGED df8ce7a
summary: RFD 0016 + a regression test pinning that OpenAiCompat (Cohere, DeepSeek, Groq, etc.) inherits the RFD-0015 UsageAcc plumbing through its delegating stream method.

## claude/quartet-monitor
verdict: PASS (manual review of code; large but coherent)
merge: SKIPPED — CONFLICT on rfd/README.md (RFD 0016 entry already in main from claude/quartet-debt; this branch also rewrites rfd/README.md). Per hard rules: `git merge --abort`, do not resolve.
summary: RFD 0017 native `monitor` tool: streaming-event background command runner with batching, volume guard, per-session caps; runtime hook injects events into the next turn. ~1.4k lines incl. 5 tests. Conflict is purely the RFD index ordering — re-spin the branch on top of current main and the merge will be clean.


## Final test run (`cargo test --workspace --no-fail-fast`)

- **Tests passed**: 1112
- **Tests failed**: 0
- **Tests ignored**: 1 (a doc-test in pi-coding-agent)
- **2 test binaries SIGKILL'd**: `lsp_real_rust_analyzer` and
  `lsp_write_tool_real_rust_analyzer`. Cause: this sandbox has the
  `rust-analyzer` rustup proxy on `$PATH` but no actual `rust-analyzer`
  component installed, so every invocation prints "Unknown binary
  'rust-analyzer'" and the test process is killed by the runner. This
  is an environmental issue, not a regression from any merged branch
  (these tests are flagged in AGENTS.md as needing an LSP skip
  pattern).

## Campaign summary

- **Total merged**: 8 of 9
- **Skipped**:
  - `claude/quartet-monitor` — CONFLICT on `rfd/README.md` against
    the just-merged `claude/quartet-debt` (both branches add their
    own RFD entries and rewrite the table). Aborted per hard-rules.
    Re-run after rebasing the branch on current main and the merge
    is mechanical.
- **Reviewer subagent**: not used (gpt-5.4 OpenAI request shape
  rejected by the API; ironically that very fix is in the
  unmerged `claude/quartet-monitor` branch in
  `crates/pi-ai/src/provider/openai.rs`).
- **Test result**: 1112 passed, 0 failed, 1 ignored, 2 binaries
  killed due to missing `rust-analyzer` (environmental).

