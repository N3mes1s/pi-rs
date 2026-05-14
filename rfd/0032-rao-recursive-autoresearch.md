# RFD 0032 — RAO: Recursive Agent Optimization for autoresearch

- **Status:** Implemented
- **Author:** pi-rs maintainers (drafter: claude-opus-4-5, autoresearch session)
- **Created:** 2026-05-13
- **Implemented:** 2026-05-13 (commit `23ecc06`) — all seven implementation-plan rows landed; `crates/pi-coding-agent/tests/autoresearch_recursive.rs` covers the delegation-bonus formula + RunEntry schema with 9 passing tests. NOTE: `23ecc06` also accidentally bundled ~125k lines of unrelated cruft (`.review-target-archive/`, `.tmp-*`, `.fuse-probe-*`, `.probe-*` dirs); a separate cleanup commit should strip those — RAO source itself is correct and isolated under `crates/pi-coding-agent/src/autoresearch/`.
- **References:**
  - [RAO paper](https://apga.github.io/RAO/) — Gandhi et al., arXiv 2605.06639
  - RFD 0005 — Subagents and the `task` tool
  - RFD 0025 — `pi --halo` autonomous loop

## Summary

Extend the autoresearch loop with **Recursive Agent Optimization (RAO)**:
the experiment agent can delegate sub-experiments to child agents running in
parallel, aggregate their results using a delegation bonus, and let the loop
learn which sub-problems benefit from fan-out vs. single-agent execution.

## Background

The current autoresearch loop (`init_experiment` / `run_experiment` /
`log_experiment`) runs a single benchmark command per iteration and keeps
or discards based on whether the primary metric improved.  It is inherently
single-threaded at the experiment level.

The RAO paper (Gandhi et al., 2026) shows that training a model to use
*recursive* sub-agents yields:
- Larger effective search breadth (parallel explorations)
- Divide-and-conquer on complex optimization targets
- A self-induced curriculum (sub-agents get simpler sub-problems)
- 2–5× wall-clock speedup on parallelisable tasks

The pi-rs autoresearch loop already has the necessary substrate:
- `task` tool (RFD 0005) — spawn child agents with isolated context
- `run_experiment` — run benchmarks and parse `METRIC` lines
- `log_experiment` — record outcomes with the `asi` free-form dict

What's missing is:
1. A way for the orchestrating agent to **fan out** experiments to children
2. A way to **aggregate** children's metric results with a delegation bonus
3. **Depth tracking** so the log records the recursion structure

## Proposal

### New tool: `run_experiment_recursive`

A new tool alongside `run_experiment` that:

1. **Splits** the experiment into N independent sub-experiments (the caller
   specifies the split)
2. **Spawns** one child agent per sub-experiment via the `task` tool
   machinery (respecting `max_concurrency` from settings)
3. **Aggregates** results:
   - `primary_metric = parent_run_metric`  (the parent still runs its own
     benchmark to get a ground truth; this is the "local node reward")
   - `delegation_bonus = λ × mean(child_success_rate)` where
     `child_success_rate` is the fraction of children whose primary metric
     improved over their own baseline
   - `composite_metric = primary_metric − direction_sign × delegation_bonus × scale`
4. **Returns** a structured summary containing the child outcomes plus the
   composite metric, which the agent passes to `log_experiment`

The delegation bonus `λ` defaults to `0.4` (matching the RAO paper's
best-performing setting) but is caller-configurable.

### Schema extension to `log_experiment`

Add optional fields to `RunEntry` (JSONL log):
- `depth: u32` — delegation depth (0 = top-level, 1 = first child, …)
- `delegation_bonus: f64 | null` — computed bonus before `keep`/`discard`
- `child_run_ids: [u32]` — cross-references to child run entries

These are `skip_serializing_if = "Option::is_none"` so the log stays
backward-compatible.

### New `autoresearch` sub-agent definition

A bundled sub-agent `autoresearch-worker` that:
- System prompt: "You are a benchmark runner. Run the given benchmark
  command, output METRIC lines, and return the result."
- Tools: `bash` only (no `task`, no `write`, no `read` — just execute)
- `spawns: null` (no further fan-out)

This agent is intentionally narrow: it runs a single benchmark and
returns the raw metric, keeping child context windows lean.

### SKILL.md update

Add a "RAO: recursive experiments" section to the autoresearch skill:
- When to delegate: large search spaces, independent sub-problems,
  long-running benchmarks that can be parallelised
- When NOT to delegate: already fast benchmarks (< 5s), sequential
  dependencies between sub-tasks, small search spaces
- Delegation bonus formula for `log_experiment`

### Depth-level inverse-frequency weighting (tracking only)

The RAO paper applies gradient weighting during RL training.  In the
autoresearch loop there is no gradient — the agent is the policy.
We track depth in the JSONL log so a future `pi-stats` aggregation or
`pi --halo` cycle can apply the weighting concept when summarising
outcomes across a recursive session.

## Implementation plan

| Step | File(s) | What |
|------|---------|------|
| 1 | `crates/pi-coding-agent/src/autoresearch/log.rs` | Add `depth`, `delegation_bonus`, `child_run_ids` to `RunEntry` |
| 2 | `crates/pi-coding-agent/src/autoresearch/tools.rs` | New `RunExperimentRecursiveTool` |
| 3 | `crates/pi-coding-agent/src/autoresearch/mod.rs` | Export new tool |
| 4 | `crates/pi-coding-agent/src/startup.rs` | Register new tool |
| 5 | `crates/pi-coding-agent/agents/autoresearch-worker.md` | Bundled sub-agent |
| 6 | `crates/pi-coding-agent/skills/autoresearch-create/SKILL.md` | RAO guidance section |
| 7 | `crates/pi-coding-agent/tests/autoresearch_recursive.rs` | Tests |

## Non-goals (v1)

- RL training of the delegation policy (RAO trains a model; we prompt one)
- Multi-level recursion beyond depth 1 in v1 (children cannot fan out further)
- Automatic splitting of search spaces (caller specifies sub-experiments)
- Gradient-based depth weighting (tracked in log but not applied)

## Risks

- **Token cost**: each child agent call adds tokens.  Mitigated by the
  narrow `autoresearch-worker` sub-agent and `max_concurrency` cap.
- **Noise amplification**: if the benchmark is noisy, parallel children
  make it noisier.  The delegation bonus smooths this by averaging across
  children.
- **Circular spawning**: the `autoresearch-worker` sub-agent has
  `spawns: null`, preventing infinite recursion.
