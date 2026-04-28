# RFD 0021 — `pi --orchestrate` (built-in campaign mode)

- **Status:** Discussion (v1.1)
- **Author:** pi-rs maintainers (drafter: opus-4-7, thinking=high)
- **Created:** 2026-04-29
- **Implemented:** &lt;pending&gt;

## Revision history

| Version | Commit   | Notes |
| ------- | -------- | ----- |
| v0.5    | 675f109  | Initial draft. Built-in `pi --orchestrate <campaign.toml>` subcommand. TOML schema, serial+DAG execution, fix-loop, override rules, resume/dry-run/status, persisted state at `~/.pi/orchestrate/<id>/`. 4 implementation milestones. |
| v1.0    | 8ea4327  | Applied first critique pass (gpt-5.4 xhigh). Corrected primitives gap (`task` `isolated` flag is a no-op today; bundled-agent loading returns empty; `auto-approve` modes are `ask / auto-policy / auto-judge / yolo`; no repo-global branch-name guard; `pi-stats` has no per-session-id query surface). Split state machine (`MERGE_PENDING` separated from review verdict; `BLOCKED_ON_CONFLICT` / `BLOCKED_ON_REVIEW_STALE` as terminals). Tightened override rules (unmatched concern text = in-scope; `forward_to` only legal to `PENDING` milestones, validated up-front). Added serialised merge queue against parent-HEAD drift. Demoted `pi-orchestrate` from a new crate to a module under `pi-coding-agent`. Cost from session-JSONL `Usage` sums in v1, pi-stats integration deferred. Misc citation tightening. |
| v1.1    | (this)   | Applied second critique pass (gpt-5.4 xhigh). Concretised the merge queue's "review snapshot" as the persisted tuple `(reviewed_branch_sha, reviewed_target_head_sha)`. Reformulated the validator rule as "`forward_to` MUST be a strict descendant of the source milestone in the dependency DAG" instead of an underspecified scheduler-theorem-prover. Added an **Operator recovery** subsection with concrete `--orchestrate-reset <milestone-id>` and `--orchestrate-re-review <milestone-id>` flows so `BLOCKED_ON_*` terminals are not dead-ends. Reworked the reviewer-parser contract into explicit structured-mode vs fallback-mode rules (heading + ≥1 bullet → structured; prose between bullets captured as implicit-in-scope chunks; missing heading or zero bullets → fallback full-redispatch). Picked **cherry-pick** as the single v1 merge primitive (matching the worktree reconciler). Demoted `PI_ORCHESTRATE_PARALLEL ≤ 4` from spec contract to implementation cap. Purged stale `CONFLICT_ABORT` references. Fixed the report's cost-source wording. Citation cleanup: `reconcile.rs` paths normalised to `crates/pi-coding-agent/src/native/worktree/reconcile.rs:124-163`. Added two tests (parser prose-between-bullets; forwarded-concern dedup on resume). |

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

Pi-rs has most of the building blocks. Two pieces are still
to be wired for orchestrate v1: direct worktree execution
inside the subagent dispatch path, and first-class shipping/
discovery of stock reviewer agents. With those, orchestrate
is a thin coordinator on top of:

1. **`task` tool / subagent runtime (RFD 0005).** Subagent
   definitions live in `~/.pi/agent/agents/*.md` or
   `<repo>/.pi/agents/*.md` with frontmatter
   (`model`, `thinking`, `tools`, `spawns`). `TaskInput` →
   `TaskOutcome` is the contract:
   `crates/pi-coding-agent/src/native/task/executor.rs:21-60`.
   `ParentHandle` plumbing lives in
   `crates/pi-coding-agent/src/native/task/tool.rs` and is
   wired into `modes/print.rs:73-78` and
   `modes/json.rs:50-55`. **Caveat:** the `task` tool's
   `isolated: true` JSON-schema flag is currently a no-op
   (`crates/pi-coding-agent/src/native/task/tool.rs:60-61,92-96`);
   it logs a `tracing::warn!` and runs in the parent's tree.
   Discovery precedence is `Project > User > Bundled`
   (`crates/pi-coding-agent/src/native/task/discovery.rs:46-72`),
   and `load_bundled()` returns empty today
   (`discovery.rs:68-72`, "reserved for `include_dir!`").
   Orchestrate v1 wires both gaps: it calls the executor
   directly with a worktree-aware `RuntimeConfig` (no
   `isolated:true` JSON detour), and ships the stock
   `code-reviewer` definition via `include_dir!` so any
   campaign that omits `defaults.reviewer` resolves cleanly
   without a project-local copy.
2. **Worktree-isolated tasks (RFD 0006).** Reconcile
   machinery under
   `crates/pi-coding-agent/src/native/worktree/` (`mod.rs`,
   `git.rs`, `reconcile.rs`, `baseline.rs`). The
   reconciler already classifies a base-HEAD-moved subagent
   commit as a `ConflictedBranch` outcome
   (`crates/pi-coding-agent/src/native/worktree/reconcile.rs:124-163`),
   which orchestrate's merge queue (§Execution semantics)
   consumes directly.
3. **`monitor` tool (RFD 0017).** Long cargo runs and
   `git push` retries surface progress one line at a time
   without blocking the agent loop. Implementation in
   `crates/pi-tools/src/monitor.rs`.
4. **Code-reviewer subagent pattern.** Definition at
   `.pi/agents/code-reviewer.md` (model
   `openai-codex/gpt-5.4`, thinking high, read-only tool
   allowlist). Verdict format ends with the regex-friendly
   line `Merge readiness: READY_TO_MERGE | NEEDS_FIX |
   DO_NOT_MERGE`. Same shape for the rfd-critic
   (`.pi/agents/rfd-critic.md`, ending in `Verdict: READY |
   NEEDS_REVISION`). Orchestrate v1 ships the
   `code-reviewer` definition as bundled (via
   `include_dir!` on `crates/pi-coding-agent/agents/`) so
   discovery falls back to it when neither user nor project
   provides one.
5. **`pi-stats` cost telemetry (RFDs 0004 + 0008 + 0010).**
   Per-session `Usage` events are persisted in JSONL. Cost
   helper: `crates/pi-ai/src/cost.rs::compute_cost`.
   `pi-stats`'s public aggregation today is
   folder/model/recent-message
   (`crates/pi-stats/src/aggregate.rs`); there is no
   per-session-id query yet. Orchestrate v1 therefore sums
   `Usage` entries directly from the session JSONLs it
   already records under
   `~/.pi/orchestrate/<id>/milestones/<id>/`. A pi-stats
   session-id query surface is deferred (Out of scope §v2).

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
| Bors-NG / Mergify / Kodiak | YAML or per-PR labels | n/a | n/a (bots gate, don't fix) | n/a | ChatOps merge bots solve a *different* problem (merge-queue serialization for human PRs). We borrow the "bot decides READY vs NEEDS_FIX" pattern. The drafter could not freshly verify Bors-NG's current upstream status in this session; treat the historical claim that it was wound down in favour of GitHub's native merge queue as drafter recollection only, not citation-grade. Mergify and Kodiak are commercially active per their public sites as of this writing. |

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
| `defaults.reviewer` | no | `"code-reviewer"` | Subagent name resolved with the existing discovery precedence `Project > User > Bundled` (`native/task/discovery.rs:46-72`). Orchestrate v1 ships `code-reviewer.md` as bundled (via `include_dir!` on `crates/pi-coding-agent/agents/`) so the default resolves cleanly without a project-local copy. Missing → fatal at validate time. |
| `defaults.fix_loop_max` | no | `2` | 0 = strict (NEEDS_FIX always aborts the milestone). |
| `defaults.push_retry_max` | no | `3` | Backoff schedule: 5s, 15s, 60s. |
| `milestones[].id` | yes | — | Unique within the campaign. Used in state-dir + report rows. |
| `milestones[].branch` | yes | — | Implementer pushes here. By repository convention the prefix is `claude/` or `pi/`; orchestrate v1 emits a *warning* (not a hard rejection) on other prefixes, since pi-rs has no repo-global branch-name guard today. |
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
error during dry-run. **Validation rule for forwarding:**
every `override_rules[].forward_to` target MUST be a
**strict descendant** of the source milestone in the
dependency DAG (i.e. there exists a non-empty
`depends_on` path from `forward_to` back to the source).
Sibling, ancestor, or unrelated targets are rejected at
validation time. This is the entire static check —
nothing more clever. Runtime then enforces the additional
invariant that the target must be in `PENDING` at review
time; if not (e.g. the descendant has already been
dispatched because it had multiple roots and another root
unlocked it first), forwarding fails with
`E_FORWARD_TARGET_NOT_PENDING` and the concern falls back
to in-scope.

**Concurrency.** Eligible milestones run in parallel up to
`PI_ORCHESTRATE_PARALLEL` (default `2`, implementation cap
`4` for v1 to bound `cargo` build-cache contention; the
cap is an internal safety knob, not part of the user-facing
campaign contract, and will be revisited once we measure).
Each milestone runs in its own git worktree via the
RFD 0006 reconciler. Orchestrate v1 does not route through
the `task` tool's `isolated:true` JSON flag (which is
currently a no-op,
`native/task/tool.rs:60-61,92-96`); it constructs the
worktree with `native/worktree/{git,reconcile}.rs`
directly and points the subagent's `RuntimeConfig` at it.

**Per-milestone state machine.** State is split between
*review disposition* and *merge progress*:

```
PENDING
  → DISPATCHED                  (implementer started)
  → REVIEWED                    (reviewer returned a verdict)

REVIEWED + verdict=READY_TO_MERGE
  → MERGE_PENDING               (queued in the global merge queue)
  → MERGED                      (target_branch fast-forwarded / cherry-picked)
  | BLOCKED_ON_CONFLICT         (cherry-pick or merge conflict)
  | BLOCKED_ON_REVIEW_STALE     (target HEAD moved since reviewer snapshot)

REVIEWED + verdict=NEEDS_FIX
  → DISPATCHED (iter+1)         (in-scope concerns, fix-loop counter ticks)
  | FAILED                      (fix_loop_max exhausted)

REVIEWED + verdict=DO_NOT_MERGE
  → FAILED

REVIEWED + every concern forwarded out-of-scope (no remaining in-scope)
  → MERGE_PENDING               (forwarding events recorded in detail; not a state)

dispatch error (LLM crash beyond retries)
  → FAILED with reason=dispatch_error
```

`MERGED`, `FAILED`, `BLOCKED_ON_CONFLICT`, and
`BLOCKED_ON_REVIEW_STALE` are terminal. Forwarding is an
*event* in `state.jsonl`, not a milestone state — the milestone
that produced the forward is otherwise treated like any other
review disposition.

Each transition is appended to
`~/.pi/orchestrate/<campaign-id>/state.jsonl` (one event per
line, `serde_json` shape: `{milestone, from, to, ts, detail}`).
Forwarded concerns appear as
`{milestone, kind: "forward", target, concern, ts}`. The
current snapshot is reconstructed by replaying the log so
partial writes can never corrupt state; resume drops a
truncated final line.

**Merge queue.** `MERGE_PENDING` milestones are processed by
a single-threaded merge worker (one merge at a time, even
with `PI_ORCHESTRATE_PARALLEL > 1`) so the target branch
mutates serially. The merge primitive in v1 is **`git
cherry-pick`** (matching the reconciler's behaviour for
branch reconciliation under RFD 0006); v2 may add explicit
`--no-ff` merge commits as a campaign-level option.

**Review snapshot.** When a reviewer returns
`READY_TO_MERGE`, the orchestrator persists the tuple
`{reviewed_branch_sha, reviewed_target_head_sha}` in the
`state.jsonl` event for that transition. `reviewed_branch_sha`
is `git rev-parse <milestones[].branch>` immediately before
the reviewer was dispatched; `reviewed_target_head_sha` is
`git rev-parse <target_branch>` at the same moment. Both are
load-bearing data: the cherry-pick uses
`reviewed_branch_sha` (so a post-review rogue push to the
branch cannot sneak unreviewed commits in), and the
staleness check uses `reviewed_target_head_sha`.

At dequeue time the worker compares a fresh
`git rev-parse <target_branch>` against
`reviewed_target_head_sha`:

- equal → cherry-pick `reviewed_branch_sha`. On success,
  transition to `MERGED`. On conflict, transition to
  `BLOCKED_ON_CONFLICT`.
- different → transition to `BLOCKED_ON_REVIEW_STALE`. v1
  does not auto-rebase or auto-re-review; the campaign
  flags this as manual-resolution-required and continues
  with milestones whose dependencies are still satisfiable.
  Recovery: see §Operator recovery.

**Override rules.** When a milestone's reviewer returns
`NEEDS_FIX`, the orchestrator runs the **reviewer parser**
(below) to produce a list of structured concerns. Each
concern is matched against `override_rules` in declared
order; first match wins:

- `verdict = "in-scope"` → counts toward `fix_loop_max`,
  re-dispatch implementer with the concern appended.
- `verdict = "out-of-scope"` requires `forward_to = <id>`.
  At runtime this is legal only if `<id>` is in `PENDING`;
  if `<id>` is already in `DISPATCHED` or later, the
  forwarding **fails** with `E_FORWARD_TARGET_NOT_PENDING`,
  the milestone is treated as if the rule did not match
  (fall through to in-scope or to "no match → in-scope"
  default), and the failure is recorded in `state.jsonl`
  with full diagnostic detail. Successful forwards do not
  count toward `fix_loop_max`. Forwarded concern text is
  appended to the target's assignment with a stable header
  (`<!-- forwarded from <source-id> @ <ts> -->`); resume
  uses that header for **dedup** so a re-played event log
  cannot duplicate the same forward twice.
- No matching rule → defaults to in-scope; the concern is
  appended verbatim to the implementer's next turn.

A milestone is only promoted to `MERGE_PENDING` (after a
forward-only verdict) if the parser produced at least one
structured concern *and* every concern was successfully
forwarded. A `NEEDS_FIX` verdict whose parser output is
empty falls back as described in the parser contract.

**Reviewer parser contract.** Two modes, deterministic by
shape of the verdict text:

*Structured mode* (preferred):

1. The `## Concerns` heading is present (case-sensitive,
   regex `^## Concerns\s*$`).
2. At least one bullet line (`^- ` or `^\* `) appears
   before the section terminator. The terminator is the
   first of: a blank line followed by another `## ` heading,
   the mandatory `Merge readiness:` final line, or EOF.
3. Inside the section the parser walks lines linearly. Each
   bullet line opens a new structured concern. Continuation
   lines (indented by ≥2 spaces or starting on the next
   non-empty line that is not itself a bullet) are folded
   into the current concern's body. Free-form prose lines
   that appear *between* recognised bullets (i.e. not
   indented continuation, not themselves bullets) are
   captured as **implicit in-scope concerns** — one per
   prose chunk, never matched against `override_rules`,
   always treated as in-scope and appended to the
   implementer's next turn.
4. The `Merge readiness:` final line must appear in the
   verdict text and match
   `^Merge readiness:\s*(READY_TO_MERGE|NEEDS_FIX|DO_NOT_MERGE)\s*$`.

*Fallback mode* triggers iff:
- the `## Concerns` heading is missing, **or**
- the heading is present but produces zero bullets, **or**
- the `Merge readiness:` final line is missing or
  un-parseable.

In fallback mode the orchestrator does not attempt to
extract concerns. The entire reviewer text becomes the
implementer's next-turn context, the fix-loop counter
ticks, no `override_rules` are evaluated, and the milestone
re-enters `DISPATCHED`. This avoids silently dropping
concerns the reviewer wrote in prose and protects against
reviewer prompt drift.

**Retry policy.**

- *Implementer/reviewer LLM crash* (provider 5xx, network
  error): up to 2 automatic retries with 30s backoff before
  marking the milestone `FAILED` with
  `reason=dispatch_error`.
- *`git push` failure*: `push_retry_max` retries with
  backoff schedule 5s, 15s, 60s.
- *Merge conflict*: never auto-resolve; transition to
  `BLOCKED_ON_CONFLICT`. The campaign continues with
  milestones whose dependencies remain satisfiable
  (`MERGED` ancestors).
- *Review-stale*: see merge queue above
  (`BLOCKED_ON_REVIEW_STALE`).

**Cancellation.** `Ctrl-C` once → graceful: finish
in-flight milestones, then exit (state is durable so resume
works). `Ctrl-C` twice → hard kill; in-flight subagent
runtimes are aborted and their worktrees are *not* cleaned
up (left for human inspection). Resume from a hard-killed
campaign restarts each `DISPATCHED` milestone from scratch
on the same branch (new commit on top of whatever the
subagent had pushed before the kill); the previous session
JSONL is preserved with a `.aborted` suffix for cost
accounting.

**Exit codes.**

- `0` — every non-`FAILED` milestone reached `MERGED`.
- `2` — at least one milestone in `FAILED` (fix-loop or
  dispatch error).
- `3` — at least one milestone in `BLOCKED_ON_CONFLICT` or
  `BLOCKED_ON_REVIEW_STALE` (manual-resolution-required).
- `4` — schema validation failed (dry-run or pre-flight).
- `5` — `E_SPEC_DRIFT` on resume.
- `130` — terminated by SIGINT.

### Operator recovery

`BLOCKED_ON_CONFLICT` and `BLOCKED_ON_REVIEW_STALE` are
terminal in the per-milestone state machine, but the
campaign itself can be repaired without starting a new
`campaign-id`. Two operator commands handle the common
cases:

```text
pi --orchestrate-reset <campaign.toml> --milestone <id>
pi --orchestrate-re-review <campaign.toml> --milestone <id>
```

- **`--orchestrate-reset`** moves a milestone in
  `BLOCKED_ON_CONFLICT`, `BLOCKED_ON_REVIEW_STALE`, or
  `FAILED` back to `PENDING`. It deletes the milestone's
  `verdict.*.md`, records a `{milestone, kind: "reset",
  reason}` event in `state.jsonl`, and leaves the branch
  alone (the operator is expected to have fixed the branch
  out-of-band, e.g. `git rebase` for `BLOCKED_ON_REVIEW_STALE`
  or manual conflict resolution for `BLOCKED_ON_CONFLICT`).
  A subsequent `--orchestrate-resume` re-dispatches the
  milestone from `PENDING`.
- **`--orchestrate-re-review`** is the lighter-weight path
  for `BLOCKED_ON_REVIEW_STALE` only: it skips re-dispatching
  the implementer, reuses the existing branch HEAD, and
  re-runs the reviewer against the new `target_branch` HEAD.
  On `READY_TO_MERGE` it re-records the snapshot tuple
  `(reviewed_branch_sha, reviewed_target_head_sha)` and
  re-enters `MERGE_PENDING`. Refused for
  `BLOCKED_ON_CONFLICT` (where the branch itself needs
  human intervention).

Both commands are no-ops if the campaign-id has no
existing state, and both are idempotent under repeated
invocation. They are also the only sanctioned way to mutate
a terminal state; everything else routes through normal
state-machine transitions.

### Output: `MERGE-REPORT-<campaign>.md`

Written to `<repo>/MERGE-REPORT-<slug-of-name>.md` at the end
of every run (success, partial, or aborted). Schema:

```markdown
# MERGE-REPORT — <name>

- **Campaign id:** sha256[..16]
- **Started / Ended:** ISO-8601 timestamps
- **Total cost:** $X.XX (sum of per-session `Usage` entries replayed from recorded session JSONLs via `crates/pi-ai/src/cost.rs::compute_cost`)
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
  <campaign-id>/                        # sha256[..16] of std::fs::canonicalize(campaign.toml) — symlinks resolved
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
first launch (sha mismatch on the file body, not the path),
resume aborts with `E_SPEC_DRIFT` and exit code `5`. Use
`--orchestrate-status` to inspect.

The schema deserialiser uses `#[serde(deny_unknown_fields)]`
on every TOML struct: a typo in a campaign field is a
validation failure, not a silent drop. Test
`orchestrate_schema::reject_unknown_field` enforces this.

### Integration with existing pi-rs

- **Task runtime.** The orchestrator drives subagents
  through the same `TaskInput` → `TaskOutcome` API
  (`crates/pi-coding-agent/src/native/task/executor.rs`)
  but calls the executor **directly** rather than going
  through the `task` tool's JSON surface. This sidesteps
  the `isolated:true` no-op and gives orchestrate a stable
  Rust call site. No new spawn channel; the parent of these
  subagent runtimes is the `modes/orchestrate.rs` entry
  point (the third entry alongside `modes/print.rs` and
  `modes/json.rs`), which installs its own `ParentHandle`
  the same way `modes/print.rs:73-78` does.
- **Worktrees.** Existing reconcile machinery is reused as
  is. The orchestrator allocates one worktree per milestone
  via `native/worktree/{git,reconcile}.rs` directly (not
  via the `task` tool's `isolated` flag, which is a no-op
  today). The `ConflictedBranch` reconcile outcome
  (`crates/pi-coding-agent/src/native/worktree/reconcile.rs:124-163`)
  is the merge-queue's `BLOCKED_ON_CONFLICT` trigger. Worktree paths under
  `~/.pi/wt/data/<encoded-repo>/<campaign-id>--<milestone-id>/`
  match the existing layout.
- **`--auto-approve` interaction.** The orchestrator's own
  invocation honours `--auto-approve`. Each subagent spawned
  by the orchestrator inherits that level via existing
  parent-context propagation
  (`crates/pi-coding-agent/src/auto_approve/`). The valid
  modes are `ask` / `auto-policy` (alias `auto`) /
  `auto-judge` / `yolo`
  (`crates/pi-coding-agent/src/auto_approve/mod.rs:69-71`,
  `cli.rs:166-174`). The campaign schema does **not**
  override approval mode; if the user wants `yolo` for the
  whole campaign they pass `--auto-approve yolo` on the
  `pi --orchestrate` invocation exactly like any other pi
  run. This avoids a second axis of trust that would have to
  be audited separately.
- **Cost telemetry.** Each subagent session's JSONL is
  recorded by orchestrate under
  `~/.pi/orchestrate/<id>/milestones/<id>/`. The report
  computes cost by replaying those JSONLs and summing
  `Usage` entries via
  `crates/pi-ai/src/cost.rs::compute_cost` (RFD 0010 — the
  single canonical cost helper). `pi-stats` ingest still
  picks the JSONLs up on the next `pi --stats` run as today;
  a per-session-id query surface in `pi-stats` is deferred
  (§Out of scope, Deferred to v2).
- **GPG.** The orchestrator's commit (the merge commit) and
  the implementer subagents' commits inherit the user's
  `git config`. We do *not* set
  `commit.gpgsign=false` automatically — that's a per-host
  decision the user owns.

## Test plan

### Unit tests

- `crates/pi-coding-agent/tests/orchestrate_schema.rs`
  - parse minimum-valid campaign
  - reject unknown fields (`#[serde(deny_unknown_fields)]`)
  - reject cyclic `depends_on`
  - warn (not reject) on branch prefix outside `claude/*`
    or `pi/*`
  - reject duplicate milestone ids
  - reject `forward_to` pointing at a missing milestone
  - reject `forward_to` whose target is not a strict
    descendant of the source in the dependency DAG
    (sibling, ancestor, unrelated → all rejected)
- `crates/pi-coding-agent/tests/orchestrate_state.rs`
  - state.jsonl replay reconstructs identical in-memory plan
  - mid-write crash (truncated last line) is dropped, prior
    state preserved
  - spec.toml drift between runs returns `E_SPEC_DRIFT`
    with exit code 5
  - resume after a successful forward does not duplicate
    the forwarded concern in the target's assignment
    (header-based dedup)
- `crates/pi-coding-agent/tests/orchestrate_override.rs`
  - regex match on Concerns bullet, forward to dependent
  - first-match-wins ordering
  - `forward_to` to non-`PENDING` target →
    `E_FORWARD_TARGET_NOT_PENDING`, falls through to
    in-scope, fix-loop counter ticks
  - all-out-of-scope verdict (with at least one structured
    concern) promotes to `MERGE_PENDING`
  - `NEEDS_FIX` verdict that produces zero structured
    concerns (parser fallback mode) is treated as
    full-redispatch
  - in-scope concern increments fix-loop counter
- `crates/pi-coding-agent/tests/orchestrate_parser.rs`
  - structured mode: bullets only
  - structured mode: bullets with prose interleaved →
    prose chunks become implicit-in-scope concerns and are
    *not* matched against `override_rules`
  - fallback mode: missing `## Concerns` heading
  - fallback mode: heading present but zero bullets
  - fallback mode: missing `Merge readiness:` line
- `crates/pi-coding-agent/tests/orchestrate_merge_queue.rs`
  - serial merging under `PI_ORCHESTRATE_PARALLEL=4`
  - `reviewed_target_head_sha` mismatch at dequeue →
    `BLOCKED_ON_REVIEW_STALE`
  - cherry-pick conflict → `BLOCKED_ON_CONFLICT`
  - `--orchestrate-reset` moves a `BLOCKED_ON_CONFLICT`
    milestone back to `PENDING` and the next resume
    re-dispatches it
  - `--orchestrate-re-review` against
    `BLOCKED_ON_REVIEW_STALE` re-records the snapshot tuple
    and merges on a fresh `READY_TO_MERGE`

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
  `BLOCKED_ON_CONFLICT`; campaign exits with code `3`; the
  report flags manual-merge-required for that milestone.
  Operator recovery via `--orchestrate-reset <id>` + new
  campaign turn (see §Operator recovery).
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
  `FAILED` / `BLOCKED_ON_CONFLICT` /
  `BLOCKED_ON_REVIEW_STALE` for that milestone (operator
  recovery is manual via the §Operator recovery flow); v2
  may add a "skip and continue" or "auto-rebase + retry"
  mode.
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

| M  | Branch                          | Scope                                                                                                                                                                                                                              | LOC est. | Dogfood spend |
| -- | ------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------- | ------------- |
| M1 | `claude/orchestrate-schema`     | TOML schema parser (`#[serde(deny_unknown_fields)]`), validator, `--orchestrate-dry-run`. Module under `crates/pi-coding-agent/src/orchestrate/`. State-dir layout defined but not written. Bundled `code-reviewer.md` shipped via `include_dir!` on `crates/pi-coding-agent/agents/`. | ~700     | $1            |
| M2 | `claude/orchestrate-serial`     | Serial executor (`PI_ORCHESTRATE_PARALLEL=1` forced): dispatch → review → fix-loop → merge → report on the topological walk. `modes/orchestrate.rs` entry point, direct executor call (not via `task` tool JSON). State.jsonl read/write. Override-rule engine with the strict reviewer parser. Per-milestone session JSONL recording + `compute_cost` rollup for the report. | ~1300    | $3            |
| M3 | `claude/orchestrate-parallel`   | Lift the parallel-1 cap up to `PI_ORCHESTRATE_PARALLEL` (≤4). Worktree-per-milestone wiring against `native/worktree/{git,reconcile}.rs` directly. Single-threaded merge queue with target-HEAD-drift detection. Push retry, conflict-abort, review-stale handling. | ~600     | $2            |
| M4 | `claude/orchestrate-resume`     | `--orchestrate-resume`, `--orchestrate-status`. Drift detection (`E_SPEC_DRIFT`). `Ctrl-C`-twice handling and `.aborted` session preservation. Exit-code matrix.                                                                                                                                                                                                                            | ~400     | $1            |

**Total LOC: ~3000.** **Total dogfood spend: ~$7.** (For
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
