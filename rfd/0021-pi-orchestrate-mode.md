# RFD 0021 — `pi --orchestrate` (built-in campaign mode)

- **Status:** Discussion (v0.5)
- **Author:** pi-rs maintainers (drafter: opus-4-7, thinking=high)
- **Created:** 2026-04-29
- **Implemented:** &lt;pending&gt;

## Revision history

| Version | Commit  | Notes |
| ------- | ------- | ----- |
| v0.5    | (this)  | Initial draft. Built-in `pi --orchestrate <campaign.toml>` subcommand. TOML schema, serial+DAG execution, fix-loop, override rules, resume/dry-run/status, persisted state at `~/.pi/orchestrate/<id>/`. 4 implementation milestones. |

## Summary

Every multi-milestone feature we've shipped lately (RFD 0019
campaign, RFD 0019 bugfix follow-up, RFD 0020 v1.1) has used the
same hand-typed `task` orchestrator spec — a 150-200-line text file
in `/tmp/` that boils down to *"dispatch milestone M, run the
reviewer, fix-loop on NEEDS_FIX, merge to main on READY, repeat,
write a report."* That spec is now stable across three campaigns.
Time to bottle it.

This RFD proposes `pi --orchestrate <campaign.toml>`: a built-in
subcommand that reads a declarative TOML campaign file, dispatches
each milestone via the existing `task` tool runtime
(RFD 0005 + RFD 0006), runs a reviewer subagent against each
result, applies a bounded fix-loop on `NEEDS_FIX`, merges
`READY_TO_MERGE` branches into `target_branch`, and writes a
`MERGE-REPORT-<campaign>.md` at the end. Resume, dry-run, and
status subcommands fall out for free once the state is persisted
under `~/.pi/orchestrate/<campaign-id>/`. **Scope: codify a
pattern we already run by hand. Not: invent a new workflow
language, not: become Dagger.**

## Background

### What we keep doing manually today

Three recent campaigns followed an identical pattern with
hand-typed orchestrator specs:

- **RFD 0019 campaign** (autonomous model router, original spec):
  6 milestones, dispatched via the `task` tool to a
  bundled `router-implementer` subagent, reviewed by the
  bundled `code-reviewer` subagent, manually merged on
  `READY_TO_MERGE`, partial report written by hand.
- **RFD 0019 bugfix** (TUI note-cascade): single milestone
  with the same dispatch-review-merge shape, fix-loop on
  reviewer feedback (commit `a30bd56` then `762d70f`).
- **RFD 0020 v1.1** (the in-flight campaign on
  `claude/router-*`): three milestones (M1 `claude/router-static`,
  M2 `claude/router-classifier`, M3 `claude/router-stats`) with
  dependency edges (M1 unlocks M2 and M3), an explicit
  override rule for M1's "missing E2E test" concern that gets
  forwarded to M2's assignment (see
  `/tmp/task-router-orchestrator-v2.txt:90-119`), and a
  `MERGE-REPORT-RFD-0020-v1.1.md` deliverable.

The bespoke specs at `/tmp/task-router-orchestrator.txt` (149
lines) and `/tmp/task-router-orchestrator-v2.txt` (179 lines)
share more than 80% of their text with each other. Diffs are
limited to milestone scope, branch names, and override rules.
Re-typing this for every campaign is wasteful and error-prone
— missing the GPG-unsigned-commit clause, forgetting to skip
the two LSP deadlock tests, and misnaming the report file are
the recurring foot-guns.

### Existing primitives this composes from

Pi-rs already has every building block. `pi --orchestrate` is
a thin coordinator on top of:

1. **`task` tool / subagent runtime (RFD 0005).** Subagent
   definitions live in `~/.pi/agent/agents/*.md` or
   `<repo>/.pi/agents/*.md` with frontmatter
   (`model`, `thinking`, `tools`, `spawns`). `TaskInput` →
   `TaskOutcome` is the contract:
   `crates/pi-coding-agent/src/native/task/executor.rs:21-60`.
   `ParentHandle` plumbing lives in
   `crates/pi-coding-agent/src/native/task/tool.rs` and is
   wired into `modes/print.rs:73-78` and
   `modes/json.rs:50-55`.
2. **Worktree-isolated tasks (RFD 0006).** Subagents can run
   inside `~/.pi/wt/data/<encoded-repo>/<task-id>/`, parent
   tree is never touched. Reconciliation via cherry-pick to
   `pi/task/<id>` branch or unified `.patch`. Code under
   `crates/pi-coding-agent/src/native/worktree/` (`mod.rs`,
   `git.rs`, `reconcile.rs`, `baseline.rs`).
3. **`monitor` tool (RFD 0017).** Long cargo runs and
   `git push` retries surface progress one line at a time
   without blocking the agent loop. Implementation in
   `crates/pi-tools/src/monitor.rs`.
4. **Code-reviewer subagent pattern.** Bundled definition at
   `.pi/agents/code-reviewer.md` (model
   `openai-codex/gpt-5.4`, thinking high, read-only tool
   allowlist). Verdict format ends with the regex-friendly
   line `Merge readiness: READY_TO_MERGE | NEEDS_FIX |
   DO_NOT_MERGE`. Same shape used for the rfd-critic
   (`.pi/agents/rfd-critic.md`, ending in `Verdict: READY |
   NEEDS_REVISION`).
5. **`pi-stats` cost telemetry (RFDs 0004 + 0008 + 0010).**
   Per-session `Usage` events are persisted in JSONL and
   roll up via `pi-stats` ingest. Cost helper:
   `crates/pi-ai/src/cost.rs::compute_cost`. The orchestrator
   reads aggregated session cost out of `pi-stats` for the
   final report.

The auto-approve policy (`crates/pi-coding-agent/src/auto_approve/`)
plus the existing `--auto-approve` flag (`yolo` /`agentic` /
`once`, see `cli.rs:166-174`) is the safety boundary the
orchestrator inherits; it does not replace or duplicate it.

## Open Questions

The drafter is forwarding these instead of guessing.

1. **Schema format: TOML vs YAML?** The RFD currently picks
   **TOML** (justified in §Proposal). Tradeoffs: TOML matches
   `Cargo.toml` / `pricing.json` ergonomics and pi-rs has zero
   YAML deps today; YAML is what GitHub Actions /
   Dagger-Python / Mergify use, so users may pattern-match
   faster. Recommendation: **TOML** for v1, accept a
   `--from-yaml` adapter in v2 if anyone files a request.
2. **Reviewer-as-subagent vs reviewer-as-builtin?** Today
   the reviewer is a Markdown subagent at
   `.pi/agents/code-reviewer.md`. Alternative: bake reviewer
   logic into Rust so the orchestrator depends on a stable
   trait, not free-form Markdown that ships in skill-style.
   Recommendation: **keep reviewer-as-subagent**; the
   override-rule regex sits between "reviewer text output"
   and the orchestrator and absorbs prompt drift. This is the
   pattern that already worked twice.
3. **Should `fix_loop_max` count reviewer rejections that the
   override-rule absorbs as out-of-scope?** Recommendation:
   **no**. Out-of-scope concerns become *forwarded* concerns
   on the dependent milestone's assignment; the fix-loop
   counter only ticks when a concern is "in-scope" (the
   implementer must change the code under review).

## Research landscape

> Drafter could not reach an external search backend in this
> session (every provider key returned `missing API key`).
> Claims below are softened to "as of pi-rs maintainers'
> recollection of public docs through 2026-Q1; not freshly
> verified". The rfd-critic step has its own `web_search`
> access and will tighten or strike anything that's stale.

| System | Schema | Dep-graph | Fix-loop | Per-step model | Notes |
| ------ | ------ | --------- | -------- | -------------- | ----- |
| GitHub Actions workflows | YAML | `needs:` per-job | n/a | n/a | Industry baseline. Reusable workflows let you parameterise. The "campaign" analogue is a multi-job workflow with `needs:` edges. |
| Dagger | Code-as-config (Go/Python/TS) | container DAG | n/a | n/a | Pipelines are programs, not docs. Powerful but loses the "spec is reviewable" property we want. |
| Pulumi Automation API | Code | program-level | n/a | n/a | Embedded SDK for orchestrating Pulumi programs from a host process — closest model for "library that *is* the orchestrator", but heavyweight for our 2-3-milestone case. |
| Bazel actions | Starlark | full action DAG | n/a | n/a | DAG mechanics are sound prior art for `depends_on`; we copy the *idea*, not the build-system layering. |
| Bors-NG / Mergify / Kodiak | YAML or per-PR labels | n/a | n/a (bots gate, don't fix) | n/a | ChatOps merge bots solve a *different* problem (merge-queue serialization for human PRs). We borrow the "bot decides READY vs NEEDS_FIX" pattern. Bors-NG itself is end-of-lifed in favour of GitHub's native merge queue per public README; Mergify and Kodiak remain commercially active. |

The genuinely novel-to-pi-rs piece is the **fix-loop +
override-rule** layer: the reviewer's verdict feeds back into
either (a) a new turn of the same milestone with its concerns
re-injected, or (b) the *next* milestone's assignment as
forwarded context. None of the systems above do that because
none of them have a per-step LLM agent that can re-attempt.

The pi-rs-shaped contribution: a **declarative campaign DAG
with reviewer-mediated fix-loops, per-milestone subagents
running in isolated worktrees, persistent resumable state.**
The closest LLM-orchestration analogues (LangGraph, Crew.ai)
are out of scope: they orchestrate prompt graphs, not
git-branch-merge graphs.

## Proposal

### CLI surface

```text
pi --orchestrate <campaign.toml>          # run / resume-if-state-exists
pi --orchestrate-resume <campaign.toml>   # explicit resume; error if no state
pi --orchestrate-dry-run <campaign.toml>  # parse + validate; print plan; no side effects
pi --orchestrate-status <campaign.toml>   # read persisted state; print table
```

All four resolve `<campaign.toml>` to its absolute path and
hash it (`sha256[..16]`) to derive a deterministic
`campaign_id` used in the state-dir layout. The TOML's
`name` field is *display only*; the path-hash is identity.

### Campaign schema (TOML)

TOML wins over YAML for v1 because (a) pi-rs has no YAML
parser in its dep tree today and adding `serde_yaml` for one
file is wasted weight; (b) TOML's `[[milestones]]` array-of-
tables is a precise fit; (c) operators already author
`Cargo.toml` and `pricing.json`. Reverse direction
(YAML→TOML adapter) deferred to v2 if anyone asks.

```toml
# campaign.toml — minimal example; one per RFD campaign
name           = "RFD 0020 v1.1 — autonomous router"
description    = "Three router milestones: static, classifier, stats."
target_branch  = "main"

# Optional global defaults applied to every milestone.
[defaults]
reviewer       = "code-reviewer"     # name of agent in .pi/agents/
fix_loop_max   = 2                   # 0 = no fix loop
push_retry_max = 3                   # transient git-push failures

[[milestones]]
id           = "m1-static"
branch       = "claude/router-static"
depends_on   = []
assignment   = """
Add `pub mod router;` to `crates/pi-agent-core/src/lib.rs`...
(full paragraph; pasted verbatim into the implementer's first turn)
"""
implementer  = "router-implementer"  # required; orchestrator does not default
# reviewer / fix_loop_max inherit from [defaults]

[[milestones]]
id          = "m2-classifier"
branch      = "claude/router-classifier"
depends_on  = ["m1-static"]
implementer = "router-implementer"
assignment  = """..."""

# Override rules: regex match against the reviewer's Concerns
# bullets. First match wins. No match → in-scope (default
# fix-loop behaviour).
[[milestones.override_rules]]
match  = "(?i)integration test|e2e"
verdict = "out-of-scope"
forward_to = "m2-classifier"   # appended to m2-classifier's assignment

[[milestones]]
id          = "m3-stats"
branch      = "claude/router-stats"
depends_on  = ["m1-static"]
implementer = "router-implementer"
assignment  = """..."""
```

#### Field reference

| Field | Required | Default | Notes |
| ----- | -------- | ------- | ----- |
| `name` | yes | — | Display only. |
| `description` | no | empty | Echoed in the report header. |
| `target_branch` | yes | — | Branch to merge milestones into. Always validated against `git remote show origin`; explicit refusal on `main` if `--auto-approve` is unset. |
| `defaults.reviewer` | no | `"code-reviewer"` | Subagent name resolved against `<repo>/.pi/agents/`, then `~/.pi/agent/agents/`. Missing → fatal. |
| `defaults.fix_loop_max` | no | `2` | 0 = strict (NEEDS_FIX always aborts the milestone). |
| `defaults.push_retry_max` | no | `3` | Backoff schedule: 5s, 15s, 60s. |
| `milestones[].id` | yes | — | Unique within the campaign. Used in state-dir + report rows. |
| `milestones[].branch` | yes | — | Implementer pushes here. Must start with `claude/` or `pi/` to satisfy the existing branch-name guard. |
| `milestones[].depends_on` | no | `[]` | Cycle → fatal at validate time. |
| `milestones[].assignment` | yes | — | Pasted into the subagent's first user turn. |
| `milestones[].implementer` | yes | — | Subagent name. No default. |
| `milestones[].reviewer` | no | inherit | Per-milestone override. |
| `milestones[].fix_loop_max` | no | inherit | Per-milestone override. |
| `milestones[].override_rules` | no | `[]` | See execution semantics. |

### Execution semantics

**Dependency graph.** `depends_on` defines a DAG. The
orchestrator builds the topological order at start time. A
milestone becomes *eligible* once every dependency is
`MERGED`. Cycles or references to undefined ids → validation
error during dry-run.

**Concurrency.** Eligible milestones run in parallel up to
`PI_ORCHESTRATE_PARALLEL` (default `2`). Each runs in its own
worktree allocated by the existing RFD 0006 machinery; the
orchestrator passes through `--worktree --worktree-mode patch`
to the subagent's runtime, so no two milestones can corrupt
each other's working tree even on the same branch ancestor.
v1 caps `PI_ORCHESTRATE_PARALLEL ≤ 4` to bound `cargo`
build-cache contention; raised in v2 once we measure.

**Per-milestone state machine.**

```
PENDING → DISPATCHED → REVIEWING → (fix-loop iter ≤ N)
                                  → READY_TO_MERGE → MERGED
                                  → NEEDS_FIX (loop exhausted) → FAILED
                                  → DO_NOT_MERGE → FAILED
                                  → OVERRIDE_FORWARDED → MERGED
```

Each transition is appended to
`~/.pi/orchestrate/<campaign-id>/state.jsonl` (one event per
line, `serde_json` shape: `{milestone, from, to, ts,
detail}`). The current snapshot is reconstructed by replaying
the log, so partial writes can never corrupt state.

**Override rules.** When a milestone's reviewer returns
`NEEDS_FIX`, its `override_rules` are matched (in declared
order) against each `Concerns` bullet in the verdict. First
matching rule wins:

- `verdict = "in-scope"` → counts toward `fix_loop_max`,
  re-dispatch implementer with the concern appended.
- `verdict = "out-of-scope"` → require `forward_to = <id>`;
  append the concern verbatim to that milestone's
  assignment (only legal if the target hasn't been
  dispatched yet); does **not** count toward `fix_loop_max`.

If every concern is matched out-of-scope and zero in-scope
concerns remain, the milestone is treated as
`READY_TO_MERGE` (the verdict is "spent" by forwarding).

**Retry policy.**

- *Implementer/reviewer LLM crash* (provider 5xx, network
  error): up to 2 automatic retries with 30s backoff before
  marking the milestone `FAILED` with reason
  `dispatch_error`.
- *`git push` failure*: `push_retry_max` retries with the
  backoff schedule above.
- *Merge conflict*: never auto-resolve. State machine moves
  to `CONFLICT_ABORT`, the milestone is parked at
  `READY_TO_MERGE`, and the campaign continues with
  remaining independent milestones; report flags this as
  manual-merge-required.

**Cancellation.** `Ctrl-C` once → graceful: finish
in-flight milestones, then exit (state is durable so resume
works). `Ctrl-C` twice → hard kill; in-flight subagent
runtimes are aborted, their worktrees are *not* cleaned up
(left for human inspection), state log is closed cleanly.

### Output: `MERGE-REPORT-<campaign>.md`

Written to `<repo>/MERGE-REPORT-<slug-of-name>.md` at the end
of every run (success, partial, or aborted). Schema:

```markdown
# MERGE-REPORT — <name>

- **Campaign id:** sha256[..16]
- **Started / Ended:** ISO-8601 timestamps
- **Total cost:** $X.XX (sum of pi-stats per-session cost)
- **Total tokens:** in / out / cache_read / cache_write

## Milestones

| id | branch | status | iters | reviewer verdict | overrides | LOC | tests pass/fail |
| -- | ------ | ------ | ----- | ---------------- | --------- | --- | --------------- |

## Override decisions
- m1-static, concern "...", forwarded to m2-classifier.

## Final test sweep
`cargo test --workspace --target ... -- --skip lsp_real_rust_analyzer --skip lsp_write_tool_real_rust_analyzer` → N passed / M failed / K skipped

## Deviations
- bullet (e.g. "M3 conflict-aborted, manual merge required").
```

The report is **idempotent**: re-running a partially-completed
campaign overwrites the report with the latest snapshot. The
state.jsonl is the source of truth; the report is a render of
it.

### Persisted state layout

```
~/.pi/orchestrate/
  <campaign-id>/                        # sha256[..16] of canonicalised campaign.toml path
    spec.toml                           # copy of the campaign at first launch (immutable)
    state.jsonl                         # append-only event log
    milestones/
      <id>/
        implementer.session.jsonl       # symlink into ~/.pi/sessions/<uuid>.jsonl
        reviewer.session.jsonl          # ditto, one per fix-loop iteration: reviewer.0.jsonl, reviewer.1.jsonl
        verdict.0.md … verdict.N.md     # raw reviewer outputs
        diff.patch                      # cumulative diff at last review
```

Resume reconstructs the in-memory plan by replaying
`state.jsonl`; if `spec.toml` content has changed since the
first launch (sha mismatch), resume aborts with
`E_SPEC_DRIFT`. Use `--orchestrate-status` to inspect.

### Integration with existing pi-rs

- **Task runtime.** The orchestrator drives subagents through
  the same `TaskInput` → `TaskOutcome` API
  (`crates/pi-coding-agent/src/native/task/executor.rs`).
  No new spawn channel; the orchestrator is just another
  `ParentHandle` wired the way `modes/print.rs:73-78` already
  wires one. The third entry point that needs this wiring is
  the new `modes/orchestrate.rs` (see implementation plan).
- **Worktrees.** Existing reconcile machinery is reused as
  is. The orchestrator never touches `~/.pi/wt/` directly; it
  passes `--worktree --worktree-mode patch
  --worktree-id <campaign-id>--<milestone-id>` to the
  subagent CLI and lets RFD 0006 do the rest.
- **`--auto-approve` interaction.** The orchestrator's own
  invocation honours `--auto-approve`. Each subagent spawned
  by the orchestrator inherits that level via existing
  parent-context propagation
  (`crates/pi-coding-agent/src/auto_approve/`). The
  campaign schema does **not** override approval mode; if
  the user wants `yolo` for the whole campaign they pass
  `--auto-approve yolo` on the `pi --orchestrate` invocation
  exactly like any other pi run. This avoids a second axis of
  trust that would have to be audited separately.
- **Cost telemetry.** Each subagent session's JSONL is
  ingested by `pi-stats` (RFD 0004) on the next `pi --stats`
  call as today. The orchestrator's report queries pi-stats
  for the per-`session_id` cost roll-up rather than
  recomputing it; that keeps `cost.rs::compute_cost`
  (RFD 0010) the single canonical entry point.
- **GPG.** The orchestrator's commit (the merge commit) and
  the implementer subagents' commits inherit the user's
  `git config`. We do *not* set
  `commit.gpgsign=false` automatically — that's a per-host
  decision the user owns.

## Test plan

### Unit tests

- `crates/pi-coding-agent/tests/orchestrate_schema.rs`
  - parse minimum-valid campaign
  - reject unknown fields (warn-and-continue OR fail; v1
    chooses fail)
  - reject cyclic `depends_on`
  - reject `branch` outside `claude/*` or `pi/*` allowlist
  - reject duplicate milestone ids
  - reject `forward_to` pointing at a missing or already-
    dispatched milestone
- `crates/pi-coding-agent/tests/orchestrate_state.rs`
  - state.jsonl replay reconstructs identical in-memory plan
  - mid-write crash (truncated last line) is dropped, prior
    state preserved
  - spec.toml drift between runs returns `E_SPEC_DRIFT`
- `crates/pi-coding-agent/tests/orchestrate_override.rs`
  - regex match on Concerns bullet, forward to dependent
  - first-match-wins ordering
  - all-out-of-scope verdict promotes to READY_TO_MERGE
  - in-scope concern increments fix-loop counter

### Integration smoke campaign

`crates/pi-coding-agent/tests/orchestrate_smoke.rs`: a
minimal 2-milestone campaign that adds a `// touched by
orchestrate-smoke` comment to two unrelated files, reviewed
by a stub `code-reviewer` (mocked LLM that always emits
`READY_TO_MERGE`), merged into a fresh test branch in a
`tempfile::TempDir` repo. Verifies:

- both worktrees are created and cleaned up
- merge commits land on the target branch in the expected
  order
- `state.jsonl` has the expected event sequence
- `MERGE-REPORT-<slug>.md` exists and parses

### Failure injection

- **Reviewer 4xx:** stub LLM returns 401 once, then
  `READY_TO_MERGE`. Orchestrator retries; milestone merges.
- **Reviewer crash exhausting retries:** stub returns 500 ×
  3. Milestone marked `FAILED`; campaign continues with
  independent milestones; report flags the failure.
- **Push failure:** mock `git push` returning non-zero
  twice, succeeding on the third. Backoff observable in log.
- **Merge conflict:** two milestones touching the same line.
  First merges; second hits conflict; state moves to
  `CONFLICT_ABORT`; campaign exits with a non-zero code; the
  report flags manual-merge-required for that milestone.
- **Mid-flight Ctrl-C:** SIGINT during a dispatched
  milestone; verify state.jsonl is closed cleanly and a
  subsequent `--orchestrate-resume` picks up where it
  stopped.

### Real-world dogfood

The first production campaign run with `pi --orchestrate` is
**RFD 0021 itself** (we eat our dog food): once M1 (schema
parser + dry-run) is in, M2-M4 ship via a `campaign.toml`
that points at this very RFD. The campaign report from that
run becomes the validation artifact.

## Out of scope (v1)

### Deferred to v2

- **YAML adapter** (the `--from-yaml` shim).
- **Per-milestone `auto_approve` overrides.** Currently
  inherited from the parent invocation, single axis.
- **Cross-repo orchestration.** v1 is single-repo. A campaign
  spanning two pi-rs adjacent crates (e.g. `pi-rs` +
  `oh-my-pi`) is a v2 problem.
- **Webhook notifications** (Slack / Discord on milestone
  events). Easy to bolt on once `state.jsonl` is the bus.
- **`PI_ORCHESTRATE_PARALLEL > 4`.** Need cargo-cache
  measurement first.
- **Replan on failure.** v1 fails the campaign on
  `FAILED`/`CONFLICT_ABORT` for that milestone; v2 may add a
  "skip and continue" or "auto-rebase + retry" mode.
- **Reviewer ensembling** (two reviewers, majority verdict).
  Plausible if we observe single-reviewer false positives,
  not before.

### Architecturally rejected

- **Workflow language with conditionals / loops** (à la
  Argo/Tekton). Campaigns are plans, not programs; the
  override-rule regex is the only escape hatch we want.
- **Cron / schedule.** This is not CI. `pi --orchestrate`
  is invoked when a human starts a campaign.
- **Web UI / TUI dashboard.** `pi --orchestrate-status` plus
  `tail -f ~/.pi/orchestrate/<id>/state.jsonl` is the v1
  interface; further UI without first-hand pain is
  premature.
- **Replacing `code-reviewer.md` with a built-in.** See
  Open Question #2.
- **Auto-creation of subagent definitions from prompts.**
  Users author `.pi/agents/*.md` themselves.
- **Bidirectional dependencies / partial-order constraints
  beyond DAG.** A DAG covered every campaign we've run.

## Implementation plan

| M  | Branch                          | Scope                                                                                                            | LOC est. | Dogfood spend |
| -- | ------------------------------- | ---------------------------------------------------------------------------------------------------------------- | -------- | ------------- |
| M1 | `claude/orchestrate-schema`     | TOML schema parser, `--orchestrate-dry-run`, validation errors. New crate `pi-orchestrate` (lib only). State-dir layout defined but not written. | ~700     | $1            |
| M2 | `claude/orchestrate-serial`     | Serial executor: dispatch → review → fix-loop → merge → report for `depends_on`-respecting linear walk (i.e. parallel = 1 forced). `modes/orchestrate.rs` entry point. State.jsonl read/write. Override-rule engine. | ~1200    | $3            |
| M3 | `claude/orchestrate-parallel`   | Lift the parallel = 1 cap; concurrency up to `PI_ORCHESTRATE_PARALLEL`. Worktree-per-milestone wiring. Push retry, conflict-abort. | ~500     | $2            |
| M4 | `claude/orchestrate-resume`     | `--orchestrate-resume`, `--orchestrate-status`. Drift detection. `Ctrl-C`-twice handling.                                                                       | ~400     | $1            |

**Total LOC: ~2800.** **Total dogfood spend: ~$7.** (For
comparison, RFD 0020 v1.1 budgeted $15 across three
milestones with no infra reuse; this is a thinner band
because most of the heavy lifting — task runtime, worktree
reconciler, monitor — is RFD-paid-for already.)

The first three milestones (schema, serial, parallel) are
shipped via the *existing* hand-typed orchestrator pattern —
RFD 0021 is allowed to be implemented by the same RFD-019-
style spec it replaces. M4 (resume + status) is the first
milestone shipped via `pi --orchestrate` itself, dogfooding
the M1-M3 deliverable.

## Revision history

(See top of file.)
