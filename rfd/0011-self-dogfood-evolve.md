# RFD 0011 — Self-dogfood pi-rs with AGENTS.md + evolve + flamegraph

- **Status:** Discussion
- **Author:** pi-rs maintainers
- **Created:** 2026-04-28
- **Implemented:** &lt;pending&gt;

## Summary

Pi-rs already ships every piece of infrastructure for a
self-improving agent: AGENTS.md discovery, the trajectory recorder
with `Outcome` entries (success/failure scoring), the
`pi --flamegraph` token viewer, the `pi --evolve` daemon that mutates
AGENTS.md by H2 section based on observed outcomes, and the
autoresearch native extension. Every dogfood run we've launched in
this repo turned all of that **off** via `--no-context-files
--no-session`. This RFD flips the defaults the other way: pi-rs
becomes its own first-class subject.

Concretely:
1. Land a starter `/home/user/pi-rs/AGENTS.md` capturing the
   conventions our dogfood loop has been quietly enforcing.
2. Stop passing `--no-context-files` / `--no-session` in the
   `/tmp/pi-dogfood*/launch.sh` scripts; switch to
   `--session-dir /tmp/pi-dogfood/sessions/`.
3. Wire the pi binary's `--evolve dry-run` into the supervisor's
   post-batch step so AGENTS.md candidates surface as PR comments
   (not auto-applied yet).
4. Run `pi --flamegraph` after each dogfood and link the HTML in the
   PR description so we can spot token waste.
5. Hook in `autoresearch` for the open-question RFDs that need
   external evidence (e.g. RFD 0009's pricing table — that exact
   loop already happened, just unstructured).

## Background

Inventory of what's built (file paths, current state):

* `crates/pi-agent-core/src/context.rs::discover_context_files` —
  walks cwd → ancestors → `agent_dir()/AGENTS.md` for any `AGENTS.md`
  / `CLAUDE.md`, sorts by depth, joins. **Wired via the runtime;
  consumed by the prompt assembler unless `--no-context-files`.**
* `crates/pi-agent-core/src/session.rs::SessionEntryKind::Outcome
  { success, source: OutcomeSource, score, notes }` — appended at
  session end by `pi_coding_agent::native::trajectory::finalize_for_runtime`.
  Sources: `Explicit` (user `:up`/`:down`), `Heuristic` (git/test/lint
  signals), `LlmJudge` (smol-model rating), `Replay` (synthetic).
* `crates/pi-coding-agent/src/native/trajectory/judge.rs` — the
  `LlmJudge` flow: at session close, run the parent's last user
  prompt + final reply through a smol model and ask "did the agent
  do what was asked?". Emits the Outcome.
* `crates/pi-coding-agent/src/native/trajectory/flamegraph.rs` —
  `pi --flamegraph <session-or-path>` renders a single HTML file
  with one frame per turn, height = output token count, colour =
  cost. Pure offline, no API calls.
* `crates/pi-coding-agent/src/evolve/{agents_md,mutate,benchmark,
  orchestrator,tick,apply}.rs` — the G5–G9 loop. `agents_md.rs`
  parses by H2 with `<!-- pi:keep -->` immutability markers.
  `mutate.rs` runs a slow-model rewrite of one section given a
  trajectory excerpt as evidence. `benchmark.rs` replays a recorded
  session against the candidate and computes a delta-score.
  `apply.rs` keeps a Pareto front and auto-applies the winner with
  a rollback-on-regression guard. `tick.rs` is the per-cwd
  single-instance lock + cost-cap-per-day driver. `orchestrator.rs`
  ties them together.
* `crates/pi-coding-agent/src/autoresearch/*` — three native tools
  (`init_experiment`, `run_experiment`, `log_experiment`) plus a
  per-session "research dashboard" + slash command. Designed for the
  exact "audit the world for facts I don't have in my training
  cutoff" loop we ran ad-hoc in RFDs 0009 and 0010.

What's *missing* is exactly two things:
1. **The AGENTS.md doesn't exist yet.** Pi has been writing into
   the void.
2. **Our dogfood launch wrappers turn all of this off.** Today:
   ```
   pi --no-context-files --no-session --provider anthropic ...
   ```
   We're benchmarking pi-rs's *cold-start* behaviour repeatedly,
   discarding the trajectory data, and skipping every evolution
   opportunity.

## Proposal

### 1. Land a starter `AGENTS.md` at the repo root

It documents the conventions every dogfood task spec has been
quietly relying on — the same rules we keep typing into the prompt:

```markdown
# AGENTS.md — pi-rs

This file is loaded into every `pi` agent session that runs inside
this repo (unless explicitly disabled with `--no-context-files`).
Its job is to encode the conventions a new agent would otherwise
have to relearn from the diff.

## House rules
<!-- pi:keep -->
- Never use `--no-verify` or skip pre-commit hooks.
- Never push to `main` from a dogfood run.
- Never soften an assertion to make a test pass; if the test is
  legitimately wrong, fix it; otherwise fix the code.
- The `(0.5, 1.5)` placeholder pricing pair is the symptom of a
  forgotten audit; never use it.
<!-- /pi:keep -->

## Where things live
- RFDs are in `rfd/`, indexed in `rfd/README.md`. New RFDs use the
  `0000-template.md` skeleton and start at `Status: Discussion`.
- Pricing data is in `crates/pi-ai/data/pricing.json` (provenance
  + cache rates). `default_providers()` reads it via
  `include_str!`. RFDs 0009 + 0010.
- Token + cost roll-up flows: provider's `Usage` event →
  `SessionEntryKind::Usage` in JSONL → `pi-stats` ingest →
  `pi --stats json`. RFDs 0008 + the runtime persistence
  follow-up.
- The `task` tool needs a `ParentHandle` registered around
  `session.prompt(...)` (see `modes/print.rs`,
  `modes/json.rs`). Adding a third mode? Wire it.

## Conventions
- Idiomatic Rust: `match` over `if let chain`, `?` over
  `.unwrap()`, `tracing::warn!` over `eprintln!` for non-fatal
  surface.
- Tests pair-with-source: a new file in `crates/<x>/src/foo.rs`
  warrants a new `crates/<x>/tests/foo.rs` (or expand the
  existing one).
- For dogfood-scope changes, keep diffs ≤ ~500 lines per commit.
  Big merges are fine when they bundle a multi-file feature.

## Current open RFDs
- 0002 — Tier-5 follow-ups (tracking).
- 0004/0005/0006 — pi-stats / subagents / worktree (Discussion;
  implementations on `claude/dogfood-*`).
- 0007 / 0008 / 0009 / 0010 — LSP formatting opts / Usage
  population / pricing audit / differential cache pricing.
- 0011 — *this file*.

## Optimisation lessons (from past sessions)
<!-- This block is mutable. The evolve daemon may rewrite it. -->
- Adaptive thinking on Opus 4.7 is much cheaper than legacy
  enabled-mode; the runtime now picks the right shape per model.
- The pricing table was stale by 2-10× across many providers;
  audit before trusting `pi --stats json` numbers.
- The `task` tool's `ParentHandle` plumbing is one of the most
  forgotten wiring steps; see if a new mode handler missed it.
```

The `<!-- pi:keep -->` block at the top is exactly why those markers
exist — to protect the safety-critical rules from any evolve
mutation. Everything below "Optimisation lessons" is fair game for
the daemon to rewrite based on observed outcomes.

### 2. Update `/tmp/pi-dogfood*/launch.sh`

Drop the `--no-context-files --no-session` pair. Add an explicit
`--session-dir` so all dogfood sessions land under one persistent
tree we can later flamegraph + replay:

```bash
exec $PI_BIN \
  --session-dir /tmp/pi-dogfood/sessions \
  --provider anthropic --model claude-opus-4-7 \
  --thinking medium \
  --auto-approve yolo \
  --json \
  -p "$prompt"
```

The `pi-stats` ingest already handles per-cwd subdirectories; the
trajectory finalizer attaches an `Outcome` at session end (via
`finalize_for_runtime`) so each dogfood produces a scored row.

### 3. Surface the evolve daemon's proposals — dry-run only

Add to the supervisor's per-batch step:

```bash
$PI_BIN --evolve dry-run > /tmp/pi-dogfood/agents-md-proposals.diff
```

Per the existing `--evolve dry-run` semantic, this:
* Runs one tick of `orchestrator::run_once`.
* Picks the lowest-scoring section in AGENTS.md from the last N
  outcomes.
* Asks the slow model for a rewrite.
* Computes the candidate's expected delta-score via a small
  benchmark replay.
* **Prints the unified diff to stdout instead of applying.**

The diff lands in the next PR description so a human can audit
before any auto-apply lands.

### 4. Wire `pi --flamegraph` into supervisor reports

After each dogfood completes:

```bash
session_id=$(jq -r '.session_id' < <(head -1 /tmp/pi-dogfood/run.jsonl))
$PI_BIN --flamegraph "$session_id" > /tmp/pi-dogfood/flame-${slug}.html
```

Attach the path in the supervisor's status update. Big bars =
expensive turns. Cross-reference with the `Outcome.notes` to spot
"long thinking turn that didn't help" patterns.

### 5. Use autoresearch for the audit RFDs going forward

RFDs 0009 + 0010 had pi run a freeform `web_search` over each row.
That's exactly what `autoresearch` formalises: a session-pinned
experiment with hypothesis → evidence → verdict. Switch the next
audit RFD (0012 — OpenAI/Google/Bedrock provider Usage population)
to use `autoresearch` so the provenance file gets a structured
audit trail rather than a freeform search history.

## Test plan

1. **AGENTS.md existence test** — a workspace test that asserts
   `/AGENTS.md` exists at repo root and contains the literal
   `<!-- pi:keep -->` marker (so an accidental delete fails CI).
2. **Trajectory finalize test** — extend
   `crates/pi-coding-agent/tests/trajectory_recorder.rs` to assert
   a session run with `--no-session=false` ends with exactly one
   `SessionEntryKind::Outcome` line.
3. **Evolve dry-run smoke** — gated on `ANTHROPIC_API_KEY`:
   `pi --evolve dry-run` against a small fixture session set
   produces a valid unified diff (`diff --git`/`+++`/`---` headers
   present).
4. **Flamegraph smoke** — `pi --flamegraph <fixture-session>`
   produces an HTML file > 1 KB whose `<svg>` contains at least
   one `<rect>` per assistant turn.
5. **End-to-end (manual)** — run RFD 0012 (next dogfood) under the
   new launcher; verify (a) AGENTS.md was loaded into the prompt
   (`pi --json` events should show context_load entries), (b) the
   session JSONL has an Outcome, (c) `pi --flamegraph` produces the
   HTML, (d) `pi --evolve dry-run` proposes a non-empty diff.

## Out of scope

- **Auto-applying evolve mutations.** v1 is dry-run only. The
  human reviews the proposed AGENTS.md diff before merging. RFD
  0013 will graduate to auto-apply behind the existing rollback-
  on-regression guard.
- **Cross-repo dogfooding.** The evolve daemon scopes per-cwd
  (single-instance lock); pi-rs auditing pi-rs is the only target
  here.
- **Skill-vs-AGENTS.md split.** Skills (`crates/pi-coding-agent/
  src/skills.rs`) are read-only context too. Whether AGENTS.md
  rules should migrate into per-skill files is a separate design
  question; leaving as-is.
- **Replacing the freeform `web_search` in earlier audit dogfoods
  with autoresearch retroactively.** Going forward only.

## Open questions

- **What's the right cost cap per day for the evolve daemon?** The
  default in `EvolveSettings::default()` is conservative ($1?
  $5?). For dogfooding pi-rs against itself, with Opus 4.7 at the
  newly-corrected $5/$25 rates, even one section-rewrite per day
  is plausibly under $0.50. Lean: keep the conservative default
  and let the user opt up.
- **Should the supervisor commit the evolve dry-run output as a
  patch artifact in `/tmp/pi-dogfood/`?** Yes, it's already a
  diff. Cheap to keep around per-PR.
- **Does `--continue` / `--resume` make sense for chained dogfoods
  (e.g. RFD 0012 picks up where 0011 left off)?** Subtle: the
  next-RFD task is a fresh assignment but should *inherit*
  AGENTS.md learnings. Lean no — fresh sessions, AGENTS.md as the
  shared memory channel.
