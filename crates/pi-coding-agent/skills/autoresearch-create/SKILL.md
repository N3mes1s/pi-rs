---
name: autoresearch-create
description: Set up and run an autonomous experiment loop for any optimization target. Gathers what to optimize, then starts the loop immediately. Use when asked to "run autoresearch", "optimize X in a loop", "set up autoresearch for X", or "start experiments".
---

# Autoresearch

Autonomous experiment loop: try ideas, keep what works, discard what doesn't, never stop.

## Tools

- **`init_experiment`** — configure session (name, metric, unit, direction). Call again to re-initialize with a new baseline when the optimization target changes.
- **`run_experiment`** — runs command, times it, captures output.
- **`log_experiment`** — records result. `keep` auto-commits. `discard`/`crash`/`checks_failed` auto-reverts code changes (autoresearch files preserved). Always include secondary `metrics` dict. Dashboard: ctrl+shift+t.
- **`run_experiment_recursive`** — RAO (RFD 0032): runs multiple benchmark variants in parallel and returns a composite metric with delegation bonus. Use when you have independent variants to compare and benchmarks take ≥ 10 s each.

## Setup

1. Ask (or infer): **Goal**, **Command**, **Metric** (+ direction), **Files in scope**, **Constraints**.
2. `git checkout -b autoresearch/<goal>-<date>`
3. Read the source files. Understand the workload deeply before writing anything.
4. Write `autoresearch.md` and `autoresearch.sh` (see below). Commit both.
5. `init_experiment` → run baseline → `log_experiment` → start looping immediately.

### `autoresearch.md`

This is the heart of the session. A fresh agent with no context should be able to read this file and run the loop effectively. Invest time making it excellent.

```markdown
# Autoresearch: <goal>

## Objective
<Specific description of what we're optimizing and the workload.>

## Metrics
- **Primary**: <name> (<unit>, lower/higher is better) — the optimization target
- **Secondary**: <name>, <name>, ... — independent tradeoff monitors

## How to Run
`./autoresearch.sh` — outputs `METRIC name=number` lines.

## Files in Scope
<Every file the agent may modify, with a brief note on what it does.>

## Off Limits
<What must NOT be touched.>

## Constraints
<Hard rules: tests must pass, no new deps, etc.>

## What's Been Tried
<Update this section as experiments accumulate. Note key wins, dead ends,
and architectural insights so the agent doesn't repeat failed approaches.>
```

Update `autoresearch.md` periodically — especially the "What's Been Tried" section — so resuming agents have full context.

### `autoresearch.sh`

Bash script (`set -euo pipefail`) that: pre-checks fast (syntax errors in <1s), runs the benchmark, and outputs structured lines to stdout. Keep the script fast — every second is multiplied by hundreds of runs.

**For fast, noisy benchmarks** (< 5s), run the workload multiple times inside the script and report the median. This produces stable data points and makes the confidence score reliable from the start. Slow workloads (ML training, large builds) don't need this — single runs are fine.

#### Structured output

- `METRIC name=value` — primary metric (must match `init_experiment`'s `metric_name`) and any secondary metrics. Parsed automatically by `run_experiment`.

#### Design the script to inform optimization

The script should output **whatever data helps you make better decisions in the next iteration.** Think about what you'll need to see after each run to know where to focus:

- Phase timings when the workload has distinct stages
- Error counts, failure categories, or test names when checks can fail in different ways
- Memory usage, cache hit rates, or other runtime diagnostics when relevant
- Anything domain-specific that would help localize regressions or identify bottlenecks

The script runs the same code every iteration — but you can **update it during the loop** if you discover you need more signal. Add instrumentation as you learn what matters.

#### Agent-supplied ASI via `log_experiment`

Use `log_experiment`'s `asi` parameter to annotate each run with **whatever would help the next iteration make a better decision.** Free-form key/value pairs — you decide what's worth recording. Don't repeat the description or raw output; capture what you'd lose after a context reset.

**Annotate failures and crashes heavily.** Discarded and crashed runs are reverted — the code changes are gone. The only record that survives is the description and ASI in `autoresearch.jsonl`. If you don't capture what you tried and why it failed, future iterations will waste time re-discovering the same dead ends.

### `autoresearch.config.json` (optional)

JSON config file that lives in the pi session's working directory (`ctx.cwd`). Supported fields:

- **`maxIterations`** (number) — maximum experiments before auto-stopping.
- **`workingDir`** (string) — override the directory for all autoresearch operations: file I/O (`autoresearch.jsonl`, `autoresearch.md`, `autoresearch.sh`, `autoresearch.checks.sh`, `autoresearch.ideas.md`), command execution, and git operations. Supports absolute paths or relative paths (resolved against `ctx.cwd`). The config file itself always stays in `ctx.cwd`. Fails if the directory doesn't exist.

```json
{
  "workingDir": "/path/to/project",
  "maxIterations": 50
}
```

### `autoresearch.checks.sh` (optional)

Bash script (`set -euo pipefail`) for backpressure/correctness checks: tests, types, lint, etc. **Only create this file when the user's constraints require correctness validation** (e.g., "tests must pass", "types must check").

When this file exists:
- Runs automatically after every **passing** benchmark in `run_experiment`.
- If checks fail, `run_experiment` reports it clearly — log as `checks_failed`.
- Its execution time does **NOT** affect the primary metric.
- You cannot `keep` a result when checks have failed.
- Has a separate timeout (default 300s, configurable via `checks_timeout_seconds`).

When this file does **not** exist, everything behaves exactly as before — no changes to the loop.

**Keep output minimal.** Only the last 80 lines of checks output are fed back to the agent on failure. Suppress verbose progress/success output and let only errors through. This keeps context lean and helps the agent pinpoint what broke.

```bash
#!/bin/bash
set -euo pipefail
# Example: run tests and typecheck — suppress success output, only show errors
pnpm test --run --reporter=dot 2>&1 | tail -50
pnpm typecheck 2>&1 | grep -i error || true
```

## Loop Rules

**LOOP FOREVER.** Never ask "should I continue?" — the user expects autonomous work.

- **Primary metric is king.** Improved → `keep`. Worse/equal → `discard`. Secondary metrics rarely affect this.
- **Annotate every run with `asi`.** Record what you learned — not what you did. What would help the next iteration or a fresh agent resuming this session?
- **Watch the confidence score.** After 3+ runs, `log_experiment` reports a confidence score (best improvement as a multiple of the session noise floor). ≥2.0× means the improvement is likely real. <1.0× means it's within noise — consider re-running to confirm before keeping. The score is advisory — it never auto-discards.
- **Simpler is better.** Removing code for equal perf = keep. Ugly complexity for tiny gain = probably discard.
- **Don't thrash.** Repeatedly reverting the same idea? Try something structurally different.
- **Crashes:** fix if trivial, otherwise log and move on. Don't over-invest.
- **Think longer when stuck.** Re-read source files, study the profiling data, reason about what the CPU is actually doing. The best ideas come from deep understanding, not from trying random variations.
- **Resuming:** if `autoresearch.md` exists, read it + git log, continue looping.

**NEVER STOP.** The user may be away for hours. Keep going until interrupted.

## Ideas Backlog

When you discover complex but promising optimizations that you won't pursue right now, **append them as bullets to `autoresearch.ideas.md`**. Don't let good ideas get lost.

On resume (context limit, crash), check `autoresearch.ideas.md` — prune stale/tried entries, experiment with the rest. When all paths are exhausted, delete the file and write a final summary.

## RAO: Recursive Experiment Fan-Out (RFD 0032)

Inspired by [Recursive Agent Optimization](https://apga.github.io/RAO/). When you have **multiple independent variants** to benchmark simultaneously, use `run_experiment_recursive` instead of (or alongside) `run_experiment`.

### When to use recursive fan-out

✅ Use `run_experiment_recursive` when:
- You have 2–8 independent code variants to compare (e.g. different compiler flags, different algorithm implementations, different config values)
- Each benchmark takes ≥ 10 s (parallelism pays; < 5 s is noise-dominated anyway)
- The variants are independent (no shared mutable state, no build conflicts)

❌ Do NOT use it when:
- Benchmarks are fast (< 5 s each) — just run them sequentially
- Sub-experiments depend on each other's output
- You only have one variant to try — use `run_experiment`
- The benchmark is already running N parallel processes internally

### How to use it

```python
# 1. Run the parent benchmark + N variants in parallel
result = run_experiment_recursive(
    parent_command="./autoresearch.sh",
    parent_baseline=1620.0,          # last kept metric value
    sub_experiments=[
        {"id": "opt-a", "command": "VARIANT=a ./autoresearch.sh", "baseline": 1620.0},
        {"id": "opt-b", "command": "VARIANT=b ./autoresearch.sh", "baseline": 1620.0},
        {"id": "opt-c", "command": "VARIANT=c ./autoresearch.sh", "baseline": 1620.0},
    ],
    lambda=0.4,        # delegation bonus weight (default 0.4)
    direction="lower", # or "higher"
    max_concurrency=4,
)

# 2. Log using the composite metric (parent + bonus).
# Use the commit_before from result.display.
log_experiment(
    commit=<commit_before from result>,
    metric=result.composite_metric,   # the adjusted value
    status="keep" if improved else "discard",
    description="...",
    metrics={"delegation_bonus": result.delegation_bonus, "parent_metric": result.parent_metric},
    asi={
        "child_outcomes": result.child_outcomes,  # which variants improved
        "mean_child_success": result.mean_child_success,
    }
)
```

### The delegation bonus formula

```
composite = parent_metric − λ × mean(child_success) × scale
```

- `child_success = 1.0` if that child's metric improved over its baseline, else `0.0`
- `mean(child_success)` = fraction of children that improved
- `λ = 0.4` by default (matches RAO paper best setting)
- `scale` = |parent_baseline − parent_metric| (keeps the bonus proportional)
- For direction=higher: `composite = parent_metric + bonus`

**Why it works**: if most variants improved, the composite is better than the raw parent metric, so the agent is rewarded for good delegation. If most variants failed, the bonus is small, and the composite accurately reflects the parent's standalone result.

### Depth tracking

`log_experiment` accepts `depth` (default 0 = top-level) in its `asi` dict to record recursion depth. Top-level experiments use `depth=0`; experiments that are themselves running sub-experiments set `depth=1`. The JSONL log records `delegationBonus` and `childRunIds` for post-hoc analysis.

## User Messages During Experiments

If the user sends a message while an experiment is running, finish the current `run_experiment` + `log_experiment` cycle first, then incorporate their feedback in the next iteration. Don't abandon a running experiment.
