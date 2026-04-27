You are working inside the pi-rs Cargo workspace at /home/user/Playground/pi-rs.

Coverage on the testable surface dropped to 83.87% lines / 79.17%
functions after adding 5 new providers, OAuth subscriptions, and
the pi-autoresearch native port. Push it back to ≥ 90% on both.

Step 1. Run `bash scripts/coverage.sh 2>&1 | tail -50`. List every
module below 90% lines OR 90% functions.

Step 2. For each module, add ONLY new test files
(`<module>_extra.rs` or `<module>_extra2.rs` if `_extra` already
exists). Do NOT modify production source.

Likely targets (verify with the report):
- `pi-ai/src/provider/azure.rs`         — happy-path + builder
- `pi-ai/src/provider/bedrock.rs`       — content_blocks_to_anthropic delegation
- `pi-ai/src/provider/google.rs`        — message_to_google_parts variants
- `pi-ai/src/oauth.rs`                  — endpoints_for_provider all aliases
- `pi-coding-agent/src/autoresearch/log.rs`
   — Init/Run/Result/Hook/Stop round-trip via append→read_all
- `pi-coding-agent/src/autoresearch/session.rs`
   — Session::load (round-trip via save/load), missing-config Err,
     md_path / jsonl_path / config_path return the right files
- `pi-coding-agent/src/autoresearch/tools.rs`
   — InitExperimentTool's spec(), RunExperimentTool's metric
     parsing failures (no METRIC line), LogExperimentTool with
     kept=false reverts (use a tempdir + git init for the test)
- `pi-coding-agent/src/autoresearch/confidence.rs`
   — even-length median, all-equal samples (MAD=0 → Insufficient or
     Green per impl), negative direction (Higher with all-decreasing)
- `pi-coding-agent/src/autoresearch/dashboard.rs`
   — empty runs table renders header only, large numbers don't
     panic, percent edge case where current_best==baseline → 0.0%
- `pi-coding-agent/src/autoresearch/hooks.rs`
   — extra: hook missing executable bit handled, hook with no .hooks
     dir is None, hook stdout capped at exactly 8192 bytes

Iterate: build clean, all tests pass, then
`bash scripts/coverage.sh 2>&1 | tail -3` should show ≥ 90%/90%.

When done output: DONE.
