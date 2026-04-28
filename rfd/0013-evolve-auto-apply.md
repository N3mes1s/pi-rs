# RFD 0013 — Auto-apply the evolve daemon's AGENTS.md mutations

- **Status:** Implemented
- **Author:** pi-rs maintainers
- **Created:** 2026-04-28
- **Implemented:** 91cbe34

## Summary

`pi --evolve dry-run` already builds the mutation prompt and would
call the slow model and benchmark the candidate. RFD 0011 wired it
in as **dry-run only** so a human reviews the diff before merging.
This RFD graduates the loop to **auto-apply** behind the existing
rollback-on-regression guard in `evolve::apply`. The daemon now
runs end-to-end: identify the worst-scoring section in AGENTS.md →
ask the slow model for a rewrite → benchmark candidate vs. current
on a sample of recorded sessions → apply if the candidate's mean
score is at least `MARGIN` (default 0.10) above current → roll back
on the next regression.

## Background

`crates/pi-coding-agent/src/evolve/apply.rs` already implements the
Pareto-front selection + the regression-on-rollback contract. RFD
0011 just left it disabled. With RFD 0012 fixing the judge's false
failures (so the benchmark scores are now trustworthy), the
auto-apply step is safe to turn on.

References:

* `crates/pi-coding-agent/src/evolve/orchestrator.rs::run_once` —
  the top-level driver.
* `crates/pi-coding-agent/src/evolve/benchmark.rs` — replays
  recorded sessions against a candidate AGENTS.md and computes a
  delta-score.
* `crates/pi-coding-agent/src/evolve/apply.rs` — Pareto front +
  rollback bookkeeping.
* `EvolveSettings { enabled, daily_cost_cap_usd,
  min_new_outcomes }` (`crates/pi-agent-core/src/settings.rs`) —
  conservative defaults already in place.

## Proposal

### 1. CLI mode bump

```rust
// crates/pi-coding-agent/src/cli.rs
#[arg(long = "evolve",
      value_parser = clap::builder::PossibleValuesParser::new(
          ["status", "off", "on", "dry-run", "apply"]))]
pub evolve: Option<String>,
```

`apply` runs `orchestrator::run_once` end-to-end (no dry-run guard).
`on` enables the per-cwd daemon. `dry-run` (today's default) keeps
existing behaviour.

### 2. Margin gate

`evolve/apply.rs` grows a free function:

```rust
pub struct ApplyDecision {
    pub apply: bool,
    pub reason: String,
    pub current_mean: f32,
    pub candidate_mean: f32,
    pub margin:        f32,
}

pub fn decide(current: &[f32], candidate: &[f32], min_margin: f32) -> ApplyDecision {
    let cur = mean(current);
    let cand = mean(candidate);
    let margin = cand - cur;
    let apply = margin >= min_margin;
    ApplyDecision {
        apply,
        reason: if apply {
            format!("candidate mean {cand:.3} ≥ current {cur:.3} + margin {min_margin}")
        } else {
            format!("candidate mean {cand:.3} < current {cur:.3} + margin {min_margin}; declined")
        },
        current_mean: cur,
        candidate_mean: cand,
        margin,
    }
}
```

The orchestrator consults `decide()` after the benchmark phase. On
`apply == true`, atomically swap AGENTS.md and append a new entry
to `~/.pi/agent/evolve/history.jsonl` so a future regression can
roll back.

### 3. Rollback contract

After an apply, the daemon watches the next N sessions
(`EvolveSettings::min_new_outcomes`, default 5). If the rolling
mean drops below `pre_apply_mean - min_margin`, the daemon
auto-reverts AGENTS.md to the previous content (also recorded in
`history.jsonl`) and refuses further mutation for 24 h.

```jsonc
// ~/.pi/agent/evolve/history.jsonl
{ "ts": "2026-04-28T...", "action": "apply",   "from_hash": "ab12...", "to_hash": "cd34...",
  "pre_mean": 0.74, "post_mean_estimate": 0.83, "margin": 0.09 }
{ "ts": "2026-04-29T...", "action": "rollback","from_hash": "cd34...", "to_hash": "ab12...",
  "observed_mean": 0.61, "trigger": "rolling 5-session mean dropped" }
```

### 4. Cost cap enforcement

Per-cwd daily $-cap is already in `EvolveSettings`. Plumbing was
in place for the dry-run; for apply we additionally:

* Bail before calling the slow model if today's spend on this cwd
  is ≥ cap.
* Record the apply call's cost in the same daily ledger.

## Test plan

1. **`evolve_apply_decide` unit tests** — eight cases covering:
   margin met / not met / equal / NaN / empty current / empty
   candidate / single-value / many-value distributions.
2. **`evolve_apply_atomic` integration** — write AGENTS.md;
   run `apply::commit(new_body)`; assert AGENTS.md updated
   AND `history.jsonl` has the new entry; crash-mid-write fixture
   (truncate during write) leaves AGENTS.md unchanged.
3. **`evolve_rollback_on_regression` integration** — seed a
   history.jsonl with one apply; feed N=5 sessions whose Outcome
   means are lower; call `rollback::tick()`; assert AGENTS.md
   reverted + a `rollback` entry was appended.
4. **End-to-end (gated)** — `pi --evolve apply` with a fixture
   sessions dir → verify either an apply lands or `decide` says
   no, exit code 0 in both cases.

## Out of scope

- **Cross-machine evolve sync.** Per-cwd local only.
- **A/B-style canary** (run new AGENTS.md on 50 % of new sessions).
  Future RFD; today's contract is "swap, watch, rollback".
- **Multiple parallel candidates.** The Pareto front in `apply.rs`
  already supports it; we apply only the top candidate per tick.
- **UI / TUI surfacing** for proposed mutations. The `--evolve
  apply` invocation is CLI-only; a TUI panel that lets the user
  approve a candidate is RFD 0017.

## Open questions

- **Default `min_margin`.** Lean 0.10 to avoid noise-driven
  apply-rollback churn. Knob in EvolveSettings.
- **Should rollback freeze the cwd for 24 h or until the next
  manual `pi --evolve apply` run?** Lean 24 h with a
  `EvolveSettings::rollback_freeze_hours` knob.
