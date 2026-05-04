# RFD 0029 — `pi --orchestrate` v2 (durability, parallelism, sandboxed dispatch)

> **Renumber note:** Originally drafted on the `rfd-orch-v2-feat` worktree
> as RFD 0023, but `0023` is already taken by `0023-sandbox-microvm.md`.
> Renumbered to 0029 (next free slot after 0028) when the doc landed on
> main. References elsewhere in the repo to "RFD 0023 pi-orchestrate-v2"
> predate the rename.

- **Status:** Discussion (v0.20)
- **Author:** pi-rs maintainers (drafter: opus-4-7, thinking=high)
- **Created:** 2026-04-30
- **Implemented:** &lt;pending&gt;

## Revision history

| Version | Commit | Notes |
| ------- | ------ | ----- |
| v0.1–v0.6 | 7fe581e–9d73ab0 | Earlier rounds (citation rot fix, typed `CampaignEvent`, session-capture as prereq, M13 reframed as blast-radius, M12 hard-prereq, `git patch-id --stable`, `merge_retry_max` rename, one-way migration, persisted `usage.cost_usd` rollup, fresh-allocate `worktree::ensure`, FAILED-retains-worktree, omitted-`sandbox`-injects-no-flag, M11 `current_thread` tokio + `spawn_blocking`, per-SHA `git diff-tree -p \| git patch-id --stable`, typed `DispatchSession`, explicit-`target_branch` worktree startpoint, unified `--orchestrate-reset`, top-level-only cost scope, `Forward` carries body + `source_iter` + `reviewer_session_jsonl`, `MergeSnapshot` event before merge, explicit child-emitted `--session-pointer` file, branch-ref deletion on reset, `SessionEntryKind::ToolCall { call.name == "task" }` subagent-footer trigger, byte-based durability assertion, `--stable` reordered-file-diff wording). |
| v0.7 | 66e2747 | Reviewer NEEDS_FIX. (1) §3.1: restored RFD 0021's `spec.toml` snapshot + content-hash drift check that v0.4 silently dropped; resume on a mutated TOML now hard-errors with `E_SPEC_DRIFT`. New `CampaignEvent::SpecSnapshot` variant. (2) §3.4 single-commit invariant block: `merge.rs::assert_single_commit` gate fails loud on multi-commit milestone branches; v1's silent tip-only merge is no longer reachable. (3) §3.6 sibling-JSONL test: dropped the wrong "`parent_id: None` distinguishes top-level" claim (`SessionManager::create` writes `None` on every session's `Meta`); the pointer file is the sole disambiguation. (4) §3.4: removed stale "named fields on `Transition::detail`" wording; v2 replay reads `MergeSnapshot` directly, v1 replay still parses the legacy `detail` string. (5) §3.12: provider resolver wires the child runtime's already-built `ToolRegistry`, not `LocalProcessProvider::with_defaults()`; new acceptance test pins this. (6) `--orchestrate-reset` now targetable via `--milestone <id>` (the non-blocking suggestion); all-eligible default preserved. Earlier revision-history entries compressed to keep total length under guideline. |
| v0.8 | e0c26b3 | Reviewer NEEDS_FIX. (1) §3.1 identity/migration: resolved the path-hash-vs-"new campaign-id" contradiction; campaign-id is **always** `sha256[..16](canonical TOML path)`, drift- and v1-migration share an explicit `archive_campaign_id(campaign_id, ts)` routine that renames `<state-root>/<campaign-id>/`, prunes orphaned worktrees (or keeps them under `--keep-worktrees`), and renames any rendered `MERGE-REPORT-…md` so a fresh report after migration cannot overwrite the archived one. Pre-drift v2 events are explicitly **not** lifted into the post-migration log. (2) §3.5 watchdog: replaced the stdout-only timer with a two-layer policy — `max_attempt` (wall-clock per-attempt cap, default 30 min) plus `io_idle` (combined stdout+stderr activity watchdog, default 10 min, operator-disablable). Three new acceptance tests (`io_idle_kills_silent_dispatch`, `io_idle_resets_on_stderr`, `io_idle_disabled`) pin the stderr-coverage hole that the v0.7 spec would have killed healthy long-tool-call dispatches on. v3 deferral: explicit child heartbeat. |
| v0.9 | a2aea8a | Reviewer NEEDS_FIX. (1) §3.1 migration: `archive_campaign_id` now also **deletes every milestone branch ref** for the live spec (`refs/heads/<branch>` for each milestone in the new TOML), and the `--keep-worktrees` flag was removed entirely — kept worktrees would have remained under the stable `<campaign-id>--<mid>` namespace where the next `ensure()` call would clobber them, defeating the point of the archive. New acceptance test `migrate_clean_baseline` proves a post-migrate run starts each milestone from `target_branch`, not from archived branch/worktree state. (2) §3.5 / Out-of-scope: explicit caveat that `io_idle` is heuristic and can kill healthy *truly silent* tool calls (pure-CPU bash loops with no stdout/stderr activity); operators set `defaults.io_idle_secs = 0` if their workload triggers this. (3) §3.5: new `[defaults]` schema reference table consolidating `attempt_timeout`, `io_idle_secs`, `max_attempts`, `merge_retry_max` (with `push_retry_max` serde alias), types, units, defaults. (4) §3.1: `--orchestrate-migrate` always writes a fresh `spec.toml` copy alongside the new `state.jsonl`, made explicit as part of the migrate routine and added to the migrate test. (5) Open Question 7 reference removed (drafter only ever had six; the back-reference to "rejected it (Open Question 7)" in §3.1 was a stale citation). |
| v0.10 | 51c27e5 | Reviewer NEEDS_FIX. (1) §3.1 migration + §3.4 reset: `git branch -D <branch>` errors with `cannot delete branch 'X' used by worktree` when the branch is currently checked out in the parent repo. v1's runner cherry-picks to `target_branch` and then `git_checkout`s a milestone branch *in `repo_root` itself* (`runner.rs:192`), so milestone branches really are checked out in the parent. v0.10 specifies the explicit detach sequence: before any `git branch -D`, the migration/reset routine runs `git -C <repo_root> checkout --detach <target_branch>` (or, if `target_branch` is unresolvable, `git checkout --detach HEAD`); only then does it delete refs and prune worktrees. New acceptance tests `migrate_detaches_parent_repo` and `reset_detaches_parent_repo` pin this. (2) §3.2 + §3.4: added `CampaignEvent::FixLoopAppend`, the persisted source of truth that lets resume reconstruct v1's in-memory `accumulated_assignment`. (3) §3.2 `DispatchSession.iter` numbering convention pinned 1-based. (4) §3.2 schema validator now rejects duplicate milestone `branch =`. (5) M11.1 framed as the second commit of M11. |
| v0.11 | ffa954a | Reviewer NEEDS_FIX. Earlier-round notes elided; full text in commit ffa954a. Highlights: (1) restored top-level `target_branch` (no `[defaults]`-nested invention); dropped TOML `auto_approve`. (2) Resolved replay-authority contradiction: `Forward` is sole pre-dispatch authority; `FixLoopAppend` is sole same-milestone post-dispatch authority; the two never overlap. (3) §3.12 lists child-side `--sandbox-provider` consumers. (4) §3.4 `--orchestrate-status` warns (never hard-fails) on `assert_single_commit` violations. (5) §3.1 adds `E_BRANCH_HELD_ELSEWHERE` for non-campaign linked worktrees. (6) `git-branch(1)` citation softened. |
| v0.12 | a8e5319 | Reviewer NEEDS_FIX. (1) §3.4 `--orchestrate-reset` now mirrors §3.1's `E_BRANCH_HELD_ELSEWHERE` rule for non-campaign linked worktrees: a refusal hard-fails with the same error code, naming the offending worktree path; reset emits **no** transition in this case so the milestone state on disk is unchanged and the operator can retry once the foreign worktree is cleared. New acceptance test `reset_branch_held_elsewhere` (test #18) pins this. (2) §3.4 reset step ordering reworked to attempt branch deletion **before** the destructive worktree-removal step (steps in order: detach → branch -D → worktree remove → emit `PENDING` transition). A failure at branch deletion now leaves no partial side effects on disk. (3) §3.2 deletes the `MergeSnapshot.iter == 0` sentinel exception. Every reviewer verdict — including the M10 forward-only verdict path — follows at least one implementer dispatch, so the 1-based `iter` is always defined; a sentinel `0` would never match the `(milestone, iter)` lookup that §3.4 resume uses against `DispatchSession`. The doc comment on `MergeSnapshot.iter` is rewritten accordingly; new test `merge_snapshot_iter_on_forward_only_verdict` (test #11a) pins `iter == 1` on an all-forward verdict. (4) §3.2 acceptance test #9 reorders events so `MergeSnapshot` is emitted **before** `Transition REVIEWED→MERGE_PENDING`, matching the §3.4 write-before-transition rule. (5) §3.4 file/function paragraph removes the stale "plus the M10 `Forward`-applied path" wording: forwarded text emits a `Forward` event, never a sibling `FixLoopAppend`, per the §3.9 single-source rule. (6) §3.1 quoted git refusal text replaced with a generic description plus the locally observed `used by worktree at <path>` form (matching the diagnostic the reviewer reproduced); the runner matches on exit status + offending-path presence in stderr, not on message text verbatim. (7) Header version stamp bumped to v0.12. |
| v0.13 | 5f3f175 | Reviewer NEEDS_FIX. (1) **§3.11 crate-graph fix.** v0.12 called `pi_coding_agent::native::worktree::ensure` and `::git::run` from `pi-orchestrate`, but `pi-coding-agent`'s `Cargo.toml:47` already lists `pi-orchestrate.workspace = true` — that is a cycle. v0.13 owns a small mechanical prerequisite refactor inside the M12 PR: extract the four worktree files (`mod.rs`, `git.rs`, `reconcile.rs`, `baseline.rs`) verbatim into a new lower-level crate `crates/pi-worktree/`. `pi-coding-agent::native::worktree` collapses to a `pub use pi_worktree::*;` re-export, preserving every existing import path and behaviour; `pi-orchestrate` adds `pi-worktree.workspace = true` and imports `pi_worktree::{ensure, worktree_dir, git, WorktreeError}` directly. The dependency graph becomes `pi-coding-agent → pi-orchestrate → pi-worktree` and `pi-coding-agent → pi-worktree` — a fan-in, not a cycle. M12 LOC bumped from ~410 to ~440 to absorb the ~30 LOC of `Cargo.toml` plumbing + `mod` declarations + the one re-export. The §3.11 snippet now imports `pi_worktree::ensure` and `pi_worktree::git::run`. Two alternatives — moving the orchestrator runtime into `pi-coding-agent` (largest blast radius) and duplicating worktree wrappers in `pi-orchestrate` (drift hazard on `ConflictedBranch`) — are explicitly considered and rejected with rationale. §2.4 #1 grows a "Crate-graph caveat" paragraph; M12's "File / function" line lists the new crate; the implementation-plan row lists the prereq as the first sub-task; the References section flags the post-extraction location. (2) **§3.4 reviewer-rerun prompt source.** v0.12 said `REVIEWED + no MergeSnapshot` resumes by re-running the reviewer, but did not specify which prompt the rerun fed. v0.13 specifies a deterministic two-mode contract: (a) **diff-only mode** (default in M6, mirroring v1's `runner.rs::reviewer_assignment` shape) — `git diff <target_branch>...<milestone_branch>` plus the milestone's spec assignment text plus any forwarded-in headers reconstructed from `Forward` replay (§3.9); the implementer chat transcript is **not** included. (b) **session-aware mode** (post-M8a, optional) — if `DispatchSession` for `(milestone, iter)` is on disk and parseable, the rerun may additionally include the truncated final-assistant-message that v1 fed `reviewer_assignment`. M6 ships only mode (a); the M8a upgrade is a one-line change in the prompt builder once captured-session paths are available. Either way, every input is on-disk in `state.jsonl` + `git`, so the rerun does not depend on pre-crash in-memory state. (3) **§3.4 step-count typo.** "Performs five steps" → "performs four steps" (the v0.12 patch trimmed step 5 — the explicit milestone-branch ref deletion was folded into step 2 — but the prose intro still said "five"). (4) Header version stamp bumped to v0.13. |
| v0.14 | 549e170 | Reviewer NEEDS_FIX. (1) **§3.11 single-worktree-per-branch preflight + failure mode.** v0.13's `allocate_milestone_worktree` did `git checkout <branch>` on the existing-ref path with no contract for git's "branch already used by another worktree" refusal (`fatal: '<branch>' is already used by worktree at '<path>'`, reproduced locally). v0.14 closes the hole on the *normal allocation path*, not just on reset/migrate cleanup. (2) **§3.11 + §3.6 + dispatch.rs decouple `agents_root` from child `cwd`** by splitting the dispatcher's single `cwd` parameter into `agents_root` + `cwd`. (3) **§3.4 reviewer-rerun prompt is an intentional change, not "mirroring v1".** (4) Header version stamp bumped to v0.14. (Earlier verbose v0.14 notes were trimmed in v0.15 to stay under the doc length guideline; full text in commit 549e170.) |
| v0.15 | 99b3d55 | Reviewer NEEDS_FIX. (1) **§3.11 + §3.6 + new §3.6a — promote `agents_root`/`cwd` to a child-runtime-wide `project_root`/`cwd` split** (full text in commit 99b3d55). (2) **§3.11 worktree-state key consistency** — pinned to `milestone_id`. (3) Header version stamp bumped to v0.15. |
| v0.16 | 60ad065 | Reviewer NEEDS_FIX. (1) **§3.11 / M12-pre-2 `ToolContext` citation fixed.** v0.15 said the new `project_root` field on `ToolContext` lives in `crates/pi-agent-core/src/tool_context.rs` (`pi_agent_core::ToolContext`). That file/type does not exist — `ToolContext` actually lives in `crates/pi-tools/src/lib.rs:39` (`pi_tools::ToolContext`); `pi-agent-core` consumes it as a leaf-crate dependency (`pi-tools.workspace = true` in `crates/pi-agent-core/Cargo.toml`) and does not re-export it. The §3.11 prereq matrix entry, the LOC breakdown, and the file-list paragraph at the bottom of §3.11 now point at the real location and call out the workspace blast radius (test fixtures across `pi-tools`, `pi-agent-core`, `pi-coding-agent`, and `pi-orchestrate` directly construct `ToolContext { ... }` literals and need `..Default::default()` / explicit `project_root` updates). (2) **§3.11 acceptance tests #9/#10 rewritten against real surfaces.** v0.15's tests relied on a `--list-tools` / `--list-tools-json` CLI flag (which does not exist on `pi -p`) and a `marker.toml` / `manifest.toml` fixture (the real extension manifest is `pi-extension.json`, per `crates/pi-coding-agent/src/extensions.rs:243`). v0.16 rewrites both tests to drive `startup::assemble(cli)` in-process — asserting against `Startup.extensions` and `Startup.runtime_config.tools.specs()` directly — and uses the correct manifest filename. The nested-`task` test similarly drops the spawn-and-grep approach and exercises `discovery::load_all` against a live `pi_tools::ToolContext { project_root, cwd }`. (3) **§3.11 prose softened on agent-discovery layering.** v0.15 implied `<project_root>/.pi/agents/*.md` is the *only* valid agent source; that overstates the live tree. Nested `task`-tool subagents follow RFD 0005's `Project > User > Bundled` precedence (`crates/pi-coding-agent/src/native/task/discovery.rs:3` + `:47`). The orchestrator's top-level `load_agent_spec` (`dispatch.rs:66`) is intentionally project-only, but the broader child runtime's discovery is layered. The prose now distinguishes the two and makes clear that v2's contract is "the *project* leg of every lookup resolves from `project_root`," not "everything lives at `project_root` and only there." (4) Header version stamp bumped to v0.16. |
| v0.17 | 4362a87 | Reviewer NEEDS_FIX. (1) **§3.11 / M12-pre-2 step 4 — `SessionManager::on_disk` is now explicitly *not* changed.** v0.16 said the prereq passes `project_root` rather than `cwd` to `SessionManager::on_disk` "so the session-cwd slug is stable across a worktree-split run." That is not pure plumbing: `SessionManager::on_disk(base, cwd)` (`crates/pi-agent-core/src/session.rs:179`) feeds its `cwd` argument both into `cwd_slug()` (the on-disk subdir under `<base>/<cwd_slug>/`, `session.rs:193`) **and** into the persisted `SessionMeta.cwd` field plus the `SessionEntryKind::Meta { cwd }` JSONL entry (`session.rs:210`, `:222`). Downstream, `pi-stats` ingest treats that `cwd` as the session's folder key (`crates/pi-stats/src/ingest.rs:92–100`). Substituting `project_root` would make a milestone-worktree session falsely advertise the campaign repo root as its cwd, breaking pi-stats' per-folder roll-ups. v0.17 keeps `SessionManager::on_disk(session_dir, cwd.clone())` unchanged at this layer, accepts the per-worktree session slug as semantically correct, and notes that a future RFD wanting a stable storage key without lying about `SessionMeta.cwd` should add a separate `on_disk_with_storage_key(...)` constructor. (2) **§3.11 stale `RuntimeContext::project_root` reference removed.** No such type exists in this tree. The `nested_task_resolves_project_subagents_from_repo_root` test no longer claims to construct one; it now exercises `discovery::load_all` directly against `ctx.project_root` after the M12-pre-2 step-6 rename, which is the actual contract M12-pre-2 changes. (3) **Implementation-plan M12 row corrected.** v0.16 still said "and `pi-agent-core::{RuntimeConfig,ToolContext}`" in the M12 prereq summary; the row now spells `pi-coding-agent::{cli, startup, context, native::task::tool}`, `pi-agent-core::RuntimeConfig`, **and** `pi_tools::ToolContext` separately, matching §3.11 and reality. (4) **§3.11 adds an explicit one-line bound on the refactor's blast radius:** "this refactor changes `.pi/*` discovery roots only — ordinary tool path resolution continues to resolve relative to `cwd`." (5) Header version stamp bumped to v0.17. |
| v0.18 | 7a767af | Reviewer NEEDS_FIX. (1) **§3.11 / M12-pre-2 step 4 — `discover_context_files` is now explicitly *not* repointed at `project_root`.** v0.17 added a one-line "this refactor changes `.pi/*` discovery roots only" blast-radius bound, but step 4 still listed `discover_context_files` in the rewire list — directly contradicting that bound. `discover_context_files(cwd, agent_dir, names)` (`crates/pi-agent-core/src/context.rs:12-32`) walks `cwd` ancestors looking for the `AGENTS.md` / `CLAUDE.md` tracked convention. That is **not** a `.pi/*` lookup, and `AGENTS.md` is git-tracked — every milestone worktree gets it via `git checkout`, including any milestone-branch-local edits, which is the desired semantic. v0.18 removes `discover_context_files` from the step-4 rewire list and adds an explicit paragraph stating it is left at `cwd` on purpose: the `cwd`-based ancestor walk is what makes per-milestone `AGENTS.md` overrides possible (a milestone branch is allowed to add a paragraph that affects only that milestone's runs). The blast-radius bound and the step-4 instruction are now consistent. The other entries in step 4 (`prompts_dirs`, `skills_dirs`, `system_prompt_paths`, `themes_dirs`, `settings_paths`, `ext_roots`) are all rooted at the literal `.pi/...`; those are the project-private namespace this refactor moves. The step-4 LOC estimate is unchanged (no callers gained, one removed). (2) Header version stamp bumped to v0.18. |
| v0.19 | 648f319 | Reviewer NEEDS_FIX. (1) **Branch-junk cleanup.** v0.18 left `campaign.toml`, `run.log`, and `state/rfd-orch-v2/state.jsonl` tracked on the feat branch — runtime artifacts from the campaign that produced this RFD, not deliverables. v0.19 removes them; the branch is once again a single-artifact diff under `rfd/`. `.gitignore` gains a `.trash/` entry (the staging area used during the cleanup so the working tree stays clean). (2) **§3.11 resume-time worktree reuse — dirty-state hazard closed.** v0.18's "rebuild `MilestoneWorktreeState` and reuse the recorded path" rule made post-crash resume depend on filesystem state that is not in `state.jsonl` or git's commit graph: a child crashing in `DISPATCHED` after editing files but before committing left a dirty index plus untracked files which the next implementer dispatch would silently inherit. v0.19 makes resume-time reuse stateful only about the *path*, not about the working-tree contents — on the first redispatch after replay (i.e. the milestone is about to enter `DISPATCHED` again on a fresh process boot), the orchestrator runs `git -C <recorded_path> reset --hard refs/heads/<branch>` followed by `git clean -fdx -e '.pi-session-pointer*'`. In-run reuse (fix-loop iteration in the same orchestrator process) does *not* reset, because the same process's prior step is the one that committed. New acceptance test §3.11 #11 (`resume_does_not_inherit_dirty_worktree`) drives crash-mid-edit end-to-end. (3) **§3.11 parent-repo working-tree precondition — clean-repo requirement, no `capture_baseline`.** M12 moves dispatch into milestone worktrees, which means staged/unstaged/untracked WIP in the parent checkout is no longer reachable from a child. The reviewer correctly flagged that the RFD never picked between option A (require a clean parent repo) and option B (use `pi_worktree::baseline::{capture_baseline, apply_baseline}` to preserve operator WIP across worktree runs). v0.19 picks **option A**: the once-per-run preflight runs `git -C <repo_root> status --porcelain=v1`; non-empty output aborts allocation with `E_PARENT_REPO_DIRTY` before any worktree is created and before the parent-repo detach step runs. Rationale: orchestrate runs are long, automated, and land cherry-picks against `target_branch`; tying their correctness to whatever the operator happened to have in the parent index is an unforced foot-gun. `pi_worktree::baseline::*` stays in `pi-worktree` for `task`-tool callers that *do* want WIP propagation; `pi-orchestrate` deliberately does not call it. New acceptance test §3.11 #12 (`orchestrate_aborts_on_dirty_parent_repo`) pins this. (4) **§3.11 / M12-pre-2 step 3 — `skills_dirs()` legacy `.agents/skills` entry is rerooted identically to `.pi/skills`.** v0.18 was silent on whether the legacy compatibility entry got the project-root treatment; v0.19 says yes, both project-relative entries are rerooted, and the `<HOME>/.pi/skills` user-scope entry is left alone. (5) **M11/M12 LOC accounting clarified.** §3.10's "~570 LOC" inline figure refers to source + tests combined (270 + 300); the implementation-plan column tracks source LOC only. Both numbers describe the same body of work. M12's row bumps from ~570 to ~640 to absorb the new resume-time reset contract, the `git status --porcelain` parent-clean precheck, the `AllocateError::ParentRepoDirty` variant, and tests #11 + #12. (6) Header version stamp bumped to v0.19. |
| v0.20 | (this) | Reviewer NEEDS_FIX. (1) **§3.5 × §3.11 — same-process retry path now scrubs the worktree.** The reviewer flagged that v0.19's "in-run reuse does *not* reset" rule was overbroad: it correctly skipped the scrub for clean fix-loop iterations (where the prior implementer attempt exited 0 and committed), but it also skipped the scrub for **same-process retries after an abnormal kill** (`max_attempt` / `io_idle` / transient-stderr exit), where the child died mid-edit and the next attempt would silently inherit dirty state. v0.20 splits the redispatch space into three paths in §3.11 (table A/B/C): same-process clean fix-loop (no scrub, path A), same-process retry after abnormal exit (scrub, path B), fresh-process resume (scrub, path C). M7's retry wrapper in §3.5 now invokes `pi_orchestrate::worktree::reset_worktree_to_branch` before re-spawning whenever the prior attempt was classified transient and a worktree exists; the hook is gated on `Option<&MilestoneWorktreeState>` so M7 still ships pre-M12 as a no-op. New acceptance test §3.11 #11b (`retry_after_abnormal_exit_does_not_inherit_dirty_worktree`) pins the same-process variant; existing #11 still covers the cross-process variant. (2) **§3.11 — drop the wrong `clean -fdx -e '.pi-session-pointer*'` carve-out.** The reviewer flagged that §3.6 stores the session-pointer file under `<state-root>/<campaign-id>/milestones/<mid>/<role>.<iter>/.pi-session-pointer`, **outside** the worktree, so `git clean -fdx` running inside the worktree never sees it; the carve-out was protecting nothing. v0.20 simplifies the scrub to bare `git -C path clean -fdx`, with a one-line note explaining why no exclusion is needed. (3) **Implementation-plan totals corrected.** Reviewer arithmetic: the listed rows sum to ~3 990 LOC and ~$13.20, not the printed ~3 790 / $12.70. v0.20 fixes the totals line. (4) **`.gitignore` removed from the branch diff.** The single-artifact milestone scope is exactly `rfd/0023-pi-orchestrate-v2.md`; the v0.19 `.trash/` `.gitignore` entry was a working-tree convenience that should not have been committed. v0.20 reverts `.gitignore` to its `rfd-orch-v2-target` content, restoring the strict single-artifact diff. (5) Header version stamp bumped to v0.20. |

## Summary

RFD 0021 v1 shipped a sequential `pi --orchestrate <campaign.toml>`
runner with implementer → reviewer → fix-loop → cherry-pick and a
truncation-tolerant `state.jsonl`. Two real campaigns landed under
it (`d0fbc5e fix(cli)`, `682c62d fix(task)`) and a 10-milestone
pi-ai sweep is in flight at 3/10 complete. That validates the
spec — now we close every "deferred to v2" item from RFD 0021
§"Out of scope (v1)" plus three new findings from the dogfood:

1. `state.jsonl` writes are buffered; operators read stale state
   for minutes because Rust's `File::write_all` does not call
   `fdatasync(2)`.
2. Hand-spawned implementer pis reflexively `cargo fmt --all` on
   the workspace and produce 47-line diff sprawl that eats their
   token budget.
3. The pi runtime hung silently for 14 min on a Transport
   mid-stream error before commit `c0c8a61` made the failure
   loud — orchestrate must therefore assume implementer
   subprocesses will occasionally wedge and put a watchdog on
   them.

This RFD scopes nine milestones (M5–M13, continuing v1's M1–M4
numbering) covering durability, full resume, retry, the
MERGE-REPORT writer, structured concerns, override forwarding,
parallel execution, worktree-per-milestone, and sandboxed tool
dispatch. **Scope: harden v1 and lift the four v1 caps
(PARALLEL=1, no worktree, no forwarding, no retry). Not:
invent new schema features, not: replace the cherry-pick merge
primitive, not: add cross-repo orchestration.**

## Background

### What v1 already does (RFD 0021, Implemented)

- TOML schema with `#[serde(deny_unknown_fields)]`, validator,
  topological order, dry-run.
- Per-milestone state machine (PENDING → DISPATCHED → REVIEWED
  → MERGE_PENDING → MERGED, with FAILED / BLOCKED_ON_CONFLICT /
  BLOCKED_ON_REVIEW_STALE terminals).
- Sequential runner: dispatch implementer → review snapshot →
  dispatch reviewer → parse FINAL `Merge readiness:` line →
  cherry-pick or fix-loop or fail. Lives in
  `crates/pi-orchestrate/src/{runner,dispatch,verdict,merge,
  plan,validate,schema}.rs` (~1.7 kLOC).
- `state.jsonl` append-only with truncated-final-line
  tolerance on replay (`runner.rs::replay`, lines 561–620).
- Current event shape: flat `StateEvent { milestone, from,
  to, ts, detail }` (`runner.rs:56–63`) serialised one-per-
  line via `runner.rs::emit_event` (lines 520–541).

### What v1 deferred and what dogfood revealed

v1's deferred-to-v2 list: `PARALLEL > 1`, worktree-per-
milestone, override-rule forwarding, structured Concerns
extraction, retry policy, MERGE-REPORT writer, full resume,
`Ctrl-C` cancellation, BLOCKED_ON_REVIEW_STALE auto-recovery,
streaming subprocess output, cross-process state lock. This
RFD ships the first seven; the last four stay deferred (§Out
of scope v2).

| Finding | Symptom | Where it bites | Fix in this RFD |
| ------- | ------- | -------------- | --------------- |
| `state.jsonl` not flushed | `tail -f state.jsonl` shows nothing for minutes; `--orchestrate-status` lies. | `runner.rs::emit_event` (lines 520–541) writes to a long-lived `&mut File` and never calls `sync_data`/`fsync`. The kernel's writeback timer eventually flushes; on a busy host it can stall. | M5 — `sync_data` after every event. |
| Implementer formats the world | `cargo fmt --all` rewrites 47 unrelated files; tokens go on the formatter diff, not the assignment. | The bundled `router-implementer` system prompt does not forbid full-tree formatting; nothing structurally constrains the diff. | M12 — per-milestone worktree contains the blast radius (the diff still lands, but on a side branch we cherry-pick *one* commit out of). M13 adds telemetry on top, not a security boundary. |
| Pi runtime hangs on Transport errors | 14-min silent stall before c0c8a61. | `dispatch.rs` calls `Child::wait_with_output()` with no timeout. | M7 — wrap dispatch in a watchdog; transient timeouts feed the retry policy. |
| 47-line sprawl and unfixable nits | Reviewer marks NEEDS_FIX on style; fix-loop counter ticks toward exhaustion. | No structural concern → forward path; everything is in-scope. | M9 + M10 — structured Concerns parser plus override forwarding. |

### 2.4 Existing primitives this composes from

This RFD composes the following primitives that already exist
in the tree (cited verbatim, file:line):

1. **Worktree reconciler** (RFD 0006). `crates/pi-coding-agent/
   src/native/worktree/{mod.rs,git.rs,reconcile.rs,baseline.rs}`.
   `worktree_dir(repo_root, task_id)` returns a stable path
   under `~/.pi/wt/data/<encoded-repo>/<task-id>/`
   (`mod.rs:57`); `ensure(repo_root, task_id)` (`mod.rs:61`)
   **fresh-allocates** at that stable path — it
   `worktree_try_remove`s any prior registration, blows the
   dir away with `tokio::fs::remove_dir_all`, then calls
   `git::worktree_add_detached` (`git.rs:74`). It does **not**
   reuse an existing checkout; reuse semantics are an
   orchestrator-level policy v2 introduces in §3.11. The
   reconciler classifies a base-HEAD-moved subagent commit as
   a `ConflictedBranch` outcome (`reconcile.rs:124–163`).
   **Crate-graph caveat:** `pi-coding-agent` already depends on
   `pi-orchestrate` (`Cargo.toml:47`), so M12 cannot import
   these helpers from their current home without creating a
   cycle. §3.11 owns a small mechanical extraction of the
   four worktree files into a new lower-level crate
   `pi-worktree`; existing `pi-coding-agent` callers continue
   to work via a `pub use` re-export. The primitives
   themselves (`ensure`, `worktree_dir`, `git::run`,
   `git::worktree_add_detached`, `reconcile::finish`,
   `ConflictedBranch`) are unchanged in API and behaviour.
2. **Sandbox provider trait** (RFD 0022). `crates/pi-sandbox/
   src/{provider.rs,local.rs,lib.rs}`: `SandboxProvider` trait
   (`provider.rs:52`), `execute_tool` (`provider.rs:62`),
   `LocalProcessProvider` (`local.rs:25`) — executes a tool
   from a `ToolRegistry` *in-process* against `ctx.cwd`. There
   is **no registry module**, **no remote provider**, and
   **no runtime hook on `pi-agent-core`**. M13 §3.12 lists
   those as hard prerequisites; this RFD does not own those
   PRs.
3. **Subprocess dispatch** (`crates/pi-orchestrate/src/
   dispatch.rs::RealDispatch`, struct at line 179, `dispatch`
   impl 195–~280 of a 358-line file). Spawns `pi -p` via
   `std::env::current_exe()`. Does NOT pass `--session-dir`,
   capture the child session id, or preserve its JSONL — M8a
   adds those.
4. **Cherry-pick merge** (`crates/pi-orchestrate/src/
   merge.rs::cherry_pick_to_target`, line 73). Idempotent at
   the git level once the bookkeeping in M6 lands.
5. **Child-side `--session-dir`**. The flag exists on the
   spawned child (`crates/pi-coding-agent/src/cli.rs:77`,
   honoured by `crates/pi-coding-agent/src/startup.rs:
   197–205`); orchestrator-side wiring is M8a.
6. **Orchestrate dispatch site.** v1's main-binary dispatch
   for `--orchestrate-dry-run` / `--orchestrate` lives in
   `crates/pi-coding-agent/src/bin/pi.rs:113–167` — not in
   `startup.rs`. M6 adds the four new flags from scratch in
   the same block; the v0.3 draft incorrectly described those
   flags as "introduced in v1".
7. **Existing v1 CLI surface** (verbatim from
   `crates/pi-coding-agent/src/cli.rs:242–256`):
   `--orchestrate-dry-run`, `--orchestrate`,
   `--orchestrate-state-root`. That is the entire orchestrate
   surface today.

**Not yet existing** (each is a prerequisite, not a primitive):

- A v2 typed `CampaignEvent` enum and `state.jsonl` schema
  bump (today's flat `StateEvent` at `runner.rs:56–63`; §3.2
  replaces it).
- `--session-dir` plumb-through plus orchestrator-side capture
  of the resolved session JSONL path; §3.6 owns it.
- A runtime hook on `pi_agent_core` that routes
  `Tool::invoke()` through a `SandboxProvider`. The current
  `LocalProcessProvider` is not wired into any agent runtime.
  M13 §3.12 lists this as a hard prereq.

## Proposal

The nine milestones below ship in roughly the order listed,
respecting the dependency arrows. They land on branches
`claude/orchestrate-v2-*` per RFD 0021's prefix convention.

### 3.1 Campaign-id identity (orchestrator-wide invariant)

Two identifiers are in flight today: v1's `state_path_for`
(`runner.rs:112`) sanitises the campaign *name* and uses that
as the on-disk directory, while RFD 0021 §"Persisted state
layout" specifies `sha256[..16]` of
`std::fs::canonicalize(campaign.toml)` as the canonical
`campaign-id`. v2 picks **one and only one**: the path-hash.

Concretely: a new `runner.rs::campaign_id_for(toml_path:
&Path) -> String` returns `sha256[..16]` of the canonicalised
TOML path. The state directory becomes
`<state-root>/<campaign-id>/`, the milestone worktrees
become `~/.pi/wt/data/<encoded-repo>/<campaign-id>--<mid>/`
(M12), and the report filename becomes
`MERGE-REPORT-<slug(name)>-<campaign-id>.md` (M8).

**Identity is single-valued.** `campaign-id = sha256[..16](
canonicalised TOML path)`. Always. Operators editing the TOML
*in place* hit the same `campaign-id` on the next invocation;
operators copying it to a new path get a different one. There
is no "incarnation id" or `path_hash + spec_sha`; we considered
that and rejected it — chasing identity through every nested
location (worktree paths, report filenames, `--session-pointer`
paths, `state.jsonl` lines) for a corner case (drift-migrate)
costs more than archive-in-place, and the migration routine
below makes archive-in-place safe.

**Migration is one-way, explicit, and *archive-in-place*.**
There are exactly two migration triggers and they share one
archival mechanism:

- **v1→v2 directory migration**: legacy v1 sanitised-name dir
  exists at `<state-root>/<sanitised-name>/`, no v2 dir at
  `<state-root>/<campaign-id>/`.
- **v2 spec-drift migration**: live `sha256(campaign.toml)`
  differs from the `SpecSnapshot.spec_sha256` recorded in the
  v2 log.

In both cases `pi --orchestrate-migrate <toml>` (M6 owns the
CLI) executes the **same** archival routine, then writes a
fresh v2 directory under the campaign-id:

1. `archive_campaign_id(campaign_id, ts, milestones) ->
   ArchiveReport`:
   - Rename `<state-root>/<campaign-id>/` (or, for v1→v2, the
     legacy sanitised-name dir at its old path) to
     `<…>.migrated-<ts>/`. The directory's `spec.toml` is
     archived along with the rest of the contents.
   - For every `CampaignEvent::Worktree { milestone, path,
     action: "allocated"|"reused" }` whose milestone has no
     subsequent `"pruned"` event in the log, run
     `git::worktree_try_remove(path)` followed by
     `git worktree prune` in `repo_root`. Worktrees live in the
     stable `<campaign-id>--<mid>` namespace; if we left them
     in place, the next `ensure()` call (§3.11) would clobber
     them, defeating the archive. v0.8's `--keep-worktrees`
     escape hatch was therefore removed in v0.9 — operators who
     want to inspect a pre-migrate worktree must `cp -a` it
     out-of-band before running `--orchestrate-migrate`. The
     archive directory's log preserves the paths so the
     operator knows which dirs existed.
   - **Detach the parent repo, then delete every milestone
     branch ref** for the live TOML. v1's runner does
     `git_checkout(repo_root, &m.branch)` directly inside
     `repo_root` before each implementer dispatch
     (`crates/pi-orchestrate/src/runner.rs:192`), so the parent
     repo's `HEAD` may be on any milestone branch when migration
     runs — and `git branch -D <branch>` refuses to delete a branch
     that is currently checked out in any linked worktree (verified
     locally against this version of git, which emits a message of
     the general form `error: cannot delete branch '<branch>' used
     by worktree at '<path>'`; `git-branch(1)`'s `--delete` section
     documents the linked-worktree protections in general terms,
     and the exact diagnostic string is not quoted from the manpage —
     downstream code matches on the refusal exit status, not the
     message text). The migration routine therefore runs, in order:
     1. Read the **top-level** `target_branch` field from the
        live TOML (same field as v1; `crates/pi-orchestrate/src/
        schema.rs:19` defines it as a required top-level
        `String`, not a `[defaults]` key — TOML validation
        already rejects a missing value).
     2. `git -C <repo_root> checkout --detach <target_branch>`.
        If `target_branch` is unresolvable (e.g. the operator
        renamed it out from under the campaign), fall back to
        `git -C <repo_root> checkout --detach HEAD`. We
        deliberately move to a detached head — not a branch —
        so the cleanup step below can delete *every* milestone
        branch without one being "the current branch".
     3. For each milestone in the *new* spec, run
        `git -C <repo_root> branch -D <branch>` (where
        `<branch>` is the milestone's branch name from §3.11),
        ignoring "branch not found". **If `git branch -D`
        still refuses because the branch is held by another
        linked worktree** (the locally observed message takes
        the form `error: cannot delete branch '<branch>' used
        by worktree at '<other-path>'`) — i.e. some
        *non-campaign* linked worktree elsewhere on disk has
        the milestone branch checked out (an operator's
        parallel investigation, an archived worktree from an
        earlier `--orchestrate-reset` that did not run with
        the live spec, etc.) — the migration routine **fails
        loud** with `E_BRANCH_HELD_ELSEWHERE`, naming the
        offending worktree path and instructing the operator
        to either remove it (`git worktree remove --force
        <path>`) or detach it. The match key is the non-zero
        exit status of `git branch -D` combined with the
        presence of the offending-worktree path in stderr; we
        do not match the diagnostic message text verbatim. The
        runner deliberately does not `worktree remove --force`
        unknown paths on the operator's behalf; that would
        risk clobbering a parallel investigation. Detaching
        the parent repo (step 2) is the only worktree we
        touch unilaterally because we know it belongs to the
        campaign's `repo_root`.
     4. `git -C <repo_root> worktree prune` to clear stale
        administrative entries left by the worktree removal
        in the previous step.

     This is essential because §3.11 says an existing
     `refs/heads/<branch>` is checked out *without* reset; if
     migration left old branch refs in place, the supposedly
     fresh post-migrate campaign would silently inherit
     archived milestone history (the same v1→v2 bug §3.1's
     archive was meant to close). Two acceptance tests pin
     this: `migrate_clean_baseline` (§3.4) for the end-to-end
     guarantee, and `migrate_detaches_parent_repo` (§3.4) for
     the explicit detach precondition — the test arranges
     `repo_root` to have a milestone branch checked out and
     asserts the migration succeeds without the
     `cannot delete branch` error v0.9 would have hit.
   - For every previously rendered
     `MERGE-REPORT-<slug>-<campaign-id>.md` at the campaign's
     repo root, rename to
     `MERGE-REPORT-<slug>-<campaign-id>.migrated-<ts>.md`. The
     campaign-id (= path-hash) is unchanged on the next run, so
     without this rename a freshly rendered report would
     overwrite the archived one.
2. Replay the v1 log (or pre-drift v2 log) into memory.
3. Write a fresh `<state-root>/<campaign-id>/state.jsonl`
   containing, in order: `SchemaVersion { v: 2 }`,
   `SpecSnapshot { spec_sha256: <hash of the live TOML> }`,
   then one `Transition` event per replayed v1 line via the §3.2
   lift, then nothing else (no Forward / Retry / Worktree /
   DispatchSession events; those did not exist in v1 and there
   is no safe synthesis from a flat detail string). For drift-
   migration the *pre-drift* v2 events are **not** lifted into
   the new file — drift means the spec changed, so prior
   transitions belong to a logically different DAG and replaying
   them against the new spec is unsafe. Operators wanting the
   pre-drift trace read the `.migrated-<ts>/` archive.
4. **Write a fresh `<state-root>/<campaign-id>/spec.toml`** by
   copying the live TOML byte-for-byte; this is the snapshot
   that subsequent invocations compare against for spec-drift
   detection (the same `spec_sha256` recorded in step 3's
   `SpecSnapshot` event).
5. Refuse to run if any of: live TOML is unreadable, the rename
   would clobber an existing `…migrated-<ts>` directory (force a
   different `<ts>`), or both v1 and v2 directories exist for
   the same `campaign-id` simultaneously.

**Post-migration the log is v2-only.** All future appends are
v2 `CampaignEvent` lines.

**v1→v2 hard-error before migration.** Until the operator runs
`--orchestrate-migrate`:

1. Every orchestrate command computes both the v2 path-hash
   directory and the v1 sanitised-name directory.
2. If the v1 dir exists *and* the v2 dir does not, the
   command **hard-errors** (fail-closed):
   ```
   error: legacy v1 orchestrate state at <v1-path>; no v2
   state at <v2-path>. Run `pi --orchestrate-migrate <toml>`
   to convert before continuing.
   ```
3. If both v1 and v2 dirs exist for the same campaign-id, the
   command also hard-errors and asks the operator to remove
   one — we never silently merge histories.

**Spec-drift protection (`SpecSnapshot` event).** RFD 0021's
v1 persisted-state contract included a `spec.toml` snapshot
inside the campaign directory and aborted resume when the
on-disk TOML's content hash no longer matched. v0.4's
path-hash transition silently dropped that check — and because
campaign-id is the path-hash, an operator editing
`campaign.toml` *in place* would reuse the same campaign-id
and the runner would happily replay an old DAG against a new
assignment set (or an old set of milestones against a new
DAG). v0.7 restores it as a typed event:

1. On first orchestrate dispatch (`PENDING` for every
   milestone, no events yet), the runner copies
   `campaign.toml` to `<state-root>/<campaign-id>/spec.toml`
   and emits, in order:
   `CampaignEvent::SchemaVersion { v: 2, ts }`, then
   `CampaignEvent::SpecSnapshot { spec_sha256, ts }`.
2. On every subsequent invocation (resume, status, reset,
   migrate), the runner re-reads the live TOML, hashes it, and
   compares to `spec_sha256` from the log. Mismatch is the
   `E_SPEC_DRIFT` hard error:
   ```
   error: campaign.toml content has changed since campaign-id
   <id> began (recorded sha256=<old>, live sha256=<new>).
   Resuming a campaign against a mutated spec is unsafe.
   Either revert the TOML, or run
   `pi --orchestrate-migrate <toml>` to archive the old run
   under `<state-root>/<campaign-id>.migrated-<ts>/` and
   restart.
   ```
3. `--orchestrate-migrate <toml>` invokes the archive-in-place
   routine above on the same campaign-id, with the live
   TOML's hash recorded in the new `SpecSnapshot`.
4. `CampaignEvent::SpecSnapshot { spec_sha256, ts }` is one
   variant of the §3.2 enum.

This keeps campaign-id stable (worktrees, report filenames,
TOML-path slugs all keep working) while still preventing the
"replay an old DAG against a new spec" foot-gun.

### 3.2 Versioned event schema (`CampaignEvent` v2)

**Why now.** M6 (resume), M7 (retry events), M8 (cost
rollup), M10 (forward dedup), M11 (parallel writers), M12
(worktree path records) all need richer event shapes than
v1's flat `{milestone, from, to, ts, detail}` (`runner.rs:
56–63`). Stuffing everything into `detail` works for M5 but
breaks at M10 because forward-dedup needs structured fields.

**Schema bump.** v2 introduces a typed enum
(`#[serde(tag = "kind")]`, each line self-describing):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CampaignEvent {
    SchemaVersion { v: u32, ts: u64 }, // first line of any v2 log
    /// Records the sha256 of the TOML at campaign-start time.
    /// Written exactly once, immediately after `SchemaVersion`,
    /// during the first orchestrate dispatch. Replay computes
    /// the live TOML's hash and emits `E_SPEC_DRIFT` on mismatch
    /// (§3.1 spec-drift protection).
    SpecSnapshot { spec_sha256: String, ts: u64 },
    Transition {
        milestone: String,
        from: String,
        to: String,
        ts: u64,
        detail: String,
    },
    Forward {
        source_milestone: String,
        target_milestone: String,
        source_iter: u32,              // reviewer fix-loop iter that emitted the concern
        reviewer_session_jsonl: String, // exact source: path of the reviewer JSONL
        concern_line_start: u32,
        concern_line_end: u32,
        body: String,                  // FULL forwarded concern body (post-fold)
        body_sha256_prefix: String,    // first 8 hex chars; used for dedup header
        ts: u64,
    },
    Retry {
        milestone: String,
        role: String,                  // "implementer" | "reviewer"
        attempt: u32,
        reason: String,                // e.g. "transport_error"
        ts: u64,
    },
    Worktree {
        milestone: String,
        path: String,
        action: String,                // "allocated" | "reused" | "pruned"
        ts: u64,
    },
    DispatchSession {
        milestone: String,
        role: String,                  // "implementer" | "reviewer"
        iter: u32,                     // 1-based fix-loop iteration; matches v1 `iter += 1` accounting (`runner.rs:184`)
        session_jsonl: String,         // path to the child's session JSONL
        exit_status: i32,              // 0 = success, non-zero = failure
        ts: u64,
    },
    /// Persisted record of one piece of text appended to the
    /// implementer's *next* prompt during a **same-milestone**
    /// fix-loop iteration. v1 builds this in memory only
    /// (`runner.rs::accumulated_assignment`, lines 181/410/442);
    /// a process restart in the middle of a fix-loop iteration
    /// would otherwise lose every prior reviewer block. M6
    /// resume reconstructs the next-turn assignment for milestone
    /// `m` by:
    ///   1. starting from the **forward-replayed assignment** for
    ///      `m` — i.e. `m.assignment` from the spec, plus every
    ///      `Forward { target_milestone: m, .. }` body in log
    ///      order with the §3.9 dedup header (this is the
    ///      *base assignment* visible to `m`'s very first
    ///      implementer dispatch);
    ///   2. then concatenating every `FixLoopAppend { milestone:
    ///      m, .. }` event in log order up to (but not including)
    ///      the iter being re-dispatched.
    ///
    /// **Authoritative replay split.** `Forward` is the **sole**
    /// persisted source for descendant-assignment mutation
    /// *before* milestone `m`'s first dispatch. `FixLoopAppend`
    /// is the **sole** persisted source for *same-milestone*
    /// fix-loop prompt growth *after* dispatch. The two never
    /// overlap, so replay has exactly one authority per phase.
    ///
    /// `role` distinguishes the source of the appended block
    /// for the report renderer; `body` is the *exact* bytes
    /// that v1 calls `.push_str(...)` with (header line plus
    /// reviewer text), so the reconstruction is byte-equivalent
    /// to v1's in-memory string.
    FixLoopAppend {
        milestone: String,
        iter: u32,                     // 1-based; iter that PRODUCED this append (next dispatch is iter+1)
        role: String,                  // "reviewer_needs_fix" | "reviewer_unparseable"
        body: String,                  // exact appended bytes including the leading header
        ts: u64,
    },
    /// Reviewer returned READY_TO_MERGE; this event PERSISTS the
    /// approved-state tuple before any cherry-pick is attempted, so
    /// crash-resume cannot synthesise a fresh approved SHA from a
    /// possibly-mutated branch HEAD. Written exactly once per
    /// (milestone, fix-loop terminating iter); see §3.4 resume matrix.
    /// `iter` is **always 1-based** (matching `DispatchSession.iter`)
    /// and equals the iteration that produced the approval — including
    /// the M10 forward-only verdict path. "Forward-only" means "no
    /// extra same-milestone fix-loop redispatch was needed", **not**
    /// "no implementer turn ran"; an implementer turn always runs
    /// before any reviewer verdict. v0.11's `iter == 0` exception was
    /// wrong: §3.4 resume looks up `MergeSnapshot` by `(milestone, iter)`
    /// against the same-keyed `DispatchSession` events, so a snapshot
    /// recorded at a sentinel `iter == 0` would never match the real
    /// dispatch iteration that produced the approval and replay would
    /// incorrectly re-run the reviewer.
    MergeSnapshot {
        milestone: String,
        iter: u32,
        reviewed_branch_sha: String,    // git rev-parse refs/heads/<milestone>
        reviewed_target_head_sha: String, // git rev-parse refs/heads/<target>
        ts: u64,
    },
}
```

**Iteration-numbering convention.** All `iter:` fields above are
**1-based** to match v1's `let mut iter: u32 = 0;
loop { iter += 1; … }` shape (`runner.rs:182–184`); `iter=1` is
the first dispatch. There are **no sentinel values**; v0.11's
`MergeSnapshot.iter == 0` exception (intended to model the M10
forward-only path) was incorrect — every reviewer verdict, including
all-forward, follows at least one implementer dispatch, so the
1-based `iter` is always defined. `FixLoopAppend.iter` is the iter
that *produced* the append, so the next dispatch consumes appends
with `iter <= N` and writes new ones tagged with `iter = N+1`
on its own reviewer return.

**Schema validator (M6 row).** v1's validator in
`crates/pi-orchestrate/src/schema.rs` already rejects unknown
fields and asserts a topological order over `depends_on`. v2
adds one more rule: **two milestones may not declare the same
`branch =`**. Duplicate branch names break worktree path
collisions (M12 would fresh-allocate two worktrees pointing at
the same `refs/heads/<branch>`) and §3.1/§3.4's per-branch-ref
deletion (the second milestone's reset would no-op once the
first deleted the shared ref). The check runs at TOML parse
time and emits `E_DUPLICATE_BRANCH` with both offending
milestone ids; ~10 LOC.

**Replay rules.** `runner.rs::replay` (currently lines
561–620) is rewritten to recognise exactly two file shapes:

1. **v2 log.** First non-blank line parses as
   `CampaignEvent::SchemaVersion { v: 2, .. }`. Every
   subsequent line parses as `CampaignEvent`. Truncated
   last-line tolerance is preserved (concern C2 from the v1
   review).
2. **v1 log.** First non-blank line parses as the legacy flat
   shape (`milestone`/`from`/`to`/`ts`/`detail`, *no* `kind`
   field). Every subsequent line must parse the same way. This
   shape is returned to the caller as a `LegacyV1Log` value.
   The orchestrate entry point treats `LegacyV1Log` as the
   trigger for the §3.1 hard-error (or, when invoked via
   `--orchestrate-migrate`, as the input to the lift below).
3. **Mixed shape** (v1 lines followed by a v2
   `schema_version`, or vice versa): replay returns
   `InvalidData` with a diagnostic referencing
   `--orchestrate-migrate`. We never silently mix shapes,
   either on read or on write.

**Lift (used by `--orchestrate-migrate`).** Each v1 `{m, from,
to, ts, detail}` becomes
`CampaignEvent::Transition { milestone: m, from, to, ts,
detail }` — byte-equivalent fields, just re-tagged. v1 logs
have no Forward/Retry/Worktree events to lift.

**Writers.** Once `replay` returns a v2-shape log,
`emit_event` only writes v2 `CampaignEvent` lines. There is
no "append v1 to v1" code path post-v0.4: a fresh v2 log
starts with `SchemaVersion`; a migrated log starts with
`SchemaVersion` followed by lifted Transitions; either way
all subsequent appends are v2.

**File / function.** `crates/pi-orchestrate/src/runner.rs`
(replace `StateEvent` with `CampaignEvent`, ~140 LOC delta;
update `emit_event` to take `&CampaignEvent`; rewrite `replay`
to dispatch on schema). Migration helper in `runner.rs::
migrate_v1_log` (~60 LOC; renames legacy dir, writes new
file, calls into the lift).

**Acceptance test.** `crates/pi-orchestrate/tests/
event_schema.rs`:

1. v2 round-trip: write one of each variant
   (`SchemaVersion`, `SpecSnapshot`, `Transition`, `Forward`
   with a multi-line `body`, `Retry`, `Worktree`,
   `DispatchSession`, `FixLoopAppend` with a multi-line
   `body`, `MergeSnapshot`), read back via `replay`,
   byte-identical re-serialisation.
2. v1 detection: a hand-rolled v1 `state.jsonl` returns
   `LegacyV1Log` with N entries; the orchestrate runner
   asserts the §3.1 hard-error with the
   `--orchestrate-migrate` hint string.
3. Migration: same fixture run through `migrate_v1_log`
   produces a v2 file whose first line is `SchemaVersion`,
   second line is `SpecSnapshot`, followed by N lifted
   `Transition` events; byte-for-byte stable across two runs
   of the lift.
4. Mixed-shape rejection: prefix v1 lines, then a v2
   `schema_version` line; `replay` returns `InvalidData`.
5. Migrate-refuses-overwrite: pre-create a v2 dir; running
   `migrate_v1_log` with both present errors out without
   touching either directory.
6. **Spec-drift detection.** Write a v2 log with
   `SpecSnapshot { spec_sha256: "abc..." }`, then mutate the
   on-disk TOML so its hash differs; running `--orchestrate
   <toml>` returns `E_SPEC_DRIFT` and touches no events. The
   `--orchestrate-migrate <toml>` path on the same fixture
   creates a new campaign-id directory with a fresh
   `SpecSnapshot` matching the mutated TOML.
7. **Duplicate-branch validation.** A TOML where two
   milestones declare the same `branch = "claude/shared"`
   is rejected at parse time with `E_DUPLICATE_BRANCH` naming
   both offending milestone ids; the validator does not write
   any state.
8. **`FixLoopAppend` replay (same-milestone).** Emit (in
   order): a `Transition` from `PENDING` to `DISPATCHED` for
   milestone `m1` with `iter=1`, a `DispatchSession` with
   `iter=1`, a `FixLoopAppend { iter=1,
   role:"reviewer_needs_fix", body:"...A..." }`, a `Transition`
   to `DISPATCHED` for `iter=2`, a `DispatchSession` with
   `iter=2`, a `FixLoopAppend { iter=2,
   role:"reviewer_unparseable", body:"...B..." }`. Replay;
   reconstruct the next-turn assignment for `m1` at `iter=3`;
   assert it equals
   `m1.assignment + "...A..." + "...B..."` byte-for-byte. No
   `forwarded_concern` role appears here — forwarded bodies are
   handled exclusively by phase (1) of §3.4's reconstruction
   (i.e. by replaying `Forward` events), never by emitting a
   sibling `FixLoopAppend`.
9. **Forward-then-crash-then-resume (cross-milestone replay).**
   The reviewer asked for one concrete end-to-end example of
   the case the previous draft was muddled on: a forward
   targets a still-`PENDING` descendant, the orchestrator
   crashes before the descendant's first dispatch, and resume
   must produce the descendant's first-dispatch prompt
   correctly. Test fixture: campaign with `m1` and `m2`
   (`depends_on = ["m1"]`); `m1` reviewer returns one
   forwarded concern targeting `m2`. Emit the events in the
   order §3.4 mandates (`MergeSnapshot` is written *before* the
   `REVIEWED→MERGE_PENDING` transition so a crash between the two
   leaves a recoverable approved tuple): `Transition` `m1:
   PENDING→DISPATCHED`, `DispatchSession` for `m1.iter=1`,
   `Forward { source_milestone:"m1", target_milestone:"m2",
   source_iter:1, body:"...C...", body_sha256_prefix:"deadbeef",
   .. }`, `MergeSnapshot` for `m1` with `iter=1`, `Transition`
   `m1: REVIEWED→MERGE_PENDING`, `Transition` `m1:
   MERGE_PENDING→MERGED`. Then *kill* the runner before any
   `m2` event. Resume; assert the next dispatch for `m2` uses
   the prompt `m2.assignment + "<!-- forwarded from
   m1:<line> body=deadbeef -->\n...C..."` byte-for-byte. No
   `FixLoopAppend` event for `m2` exists at this point (M2 has
   not dispatched yet), so phase (2) is empty; phase (1) does
   all the work.

**LOC estimate.** ~370 LOC (215 source + 155 tests; +30 over
v0.9 for `FixLoopAppend` round-trip + replay test, and the
duplicate-branch validator + test).
**Dependencies.** None. Lands first. M6 wires the CLI flag
that calls `migrate_v1_log`.

### 3.3 M5 — Durable state writes

**Motivating example.** During the 10-milestone pi-ai sweep,
`pi --orchestrate-status` showed two milestones in `DISPATCHED`
that had reached `MERGED` six minutes earlier. `tail -f
state.jsonl` was equally stale.

**Proposed primitive.** `runner.rs::emit_event` (lines
520–541) gains one line:

```rust
log.write_all(line.as_bytes())?;
log.write_all(b"\n")?;
log.sync_data()?;          // ← new: durable before we return
```

`sync_data` (POSIX `fdatasync`) does flush the data and any
metadata required to make the new bytes visible to a reader
(crucially, file size — the reviewer is right that size is
not a "skipped" metadata field; what `fdatasync` skips
relative to `fsync` is *non-essential* metadata like `atime`
and `mtime`). For an append-only log that is exactly the
trade we want.

**Open-once vs open-per-event.** v1 and v2 both keep the
file open for the full run (`emit_event` takes `&mut File`);
M5 doesn't change that, and M11 wraps it in a
`tokio::sync::Mutex<File>`. Open-per-event would force a
syscall pair per write and is rejected.

**Concurrent-reader semantics.** `pi --orchestrate-status`
opens read-only and replays. Each event is one
`write_all + sync_data` pair, so partial-line tearing is the
only failure mode (already handled by truncated-last-line
drop). No `flock`; cross-process *write* serialisation on the
same campaign is out of scope (§Out of scope v2).

**File / function.** `crates/pi-orchestrate/src/runner.rs`
(`emit_event`, ~3 LOC delta). Bench
`crates/pi-orchestrate/benches/state_durability.rs` (~80
LOC). The bench reports µs/event; we **do not** wire it as a
CI pass/fail gate (the reviewer is right that SSD/NVMe/NFS
variance makes a fixed threshold meaningless). The number is
recorded in the bench output for human inspection.

**Acceptance test.**
`crates/pi-orchestrate/tests/state_durability.rs`:

1. `kill -9` a child after 100 writes; parent re-opens and
   reads all 100 lines.
2. Concurrent reader: a *fresh* reader opens the file after
   each writer return from `emit_event` and asserts the new
   line's bytes are visible (i.e. read against the live byte
   stream, not against `mtime` — `fdatasync` does not promise
   `mtime` update, only that data + size required for
   subsequent reads have hit disk).

**LOC estimate.** ~100 LOC (3 source + 80 bench + ~20 test).
**Dependencies.** §3.2.

### 3.4 M6 — Full resume from `state.jsonl` (and CLI surface)

**Motivating example.** v1 replays state to rebuild the plan,
but the runner's main loop iterates the topological order
from the top and only the *current* state machine respects
already-terminal states. A 10-milestone campaign that crashes
at milestone 8 currently re-dispatches milestones 1–7 on
resume.

**Proposed primitive.** Move the gate from "is this
milestone's `from` PENDING" to "is this milestone's *current*
state non-terminal". The replayed `current_state` map is
already produced by `runner.rs::replay`. Add the explicit
terminal set:

```rust
fn is_terminal(state: &str) -> bool {
    matches!(state,
        "MERGED" | "FAILED" | "BLOCKED_ON_CONFLICT" | "BLOCKED_ON_REVIEW_STALE")
}
```

The runner skips terminal milestones. The review-snapshot
tuple (`reviewed_branch_sha`, `reviewed_target_head_sha`)
that v1 packed into the `detail` string of the
`REVIEWED → MERGE_PENDING` transition lives in v2 as a
**separate typed event**, `CampaignEvent::MergeSnapshot`
(§3.2). v2 replay therefore has two distinct rules:

- **v1 logs** (parsed only via `--orchestrate-migrate`): the
  legacy `detail` string is parsed by a small back-compat
  parser to recover the snapshot tuple; that recovery is used
  during the lift only.
- **v2 logs** (every post-migration log): replay reads
  `MergeSnapshot` directly. There is no named-field shape on
  `Transition::detail` — `detail` remains a free-form string
  and never carries load-bearing data again. The v0.5 / v0.6
  drafts described the snapshot as named fields on
  `Transition::detail`; that wording was self-inconsistent
  with §3.2 and is now removed.

**Resume matrix.**

| Replayed state | Resume action |
| -------------- | ------------- |
| `PENDING` | Dispatch implementer (fresh). |
| `DISPATCHED` | Re-dispatch implementer; the previous session JSONL (§3.6) is preserved as `implementer.<iter>.aborted.jsonl`. The implementer's prompt is reconstructed in two phases per §3.2 and §3.9: (1) **base assignment** — start from `m.assignment` (spec text) and apply every `CampaignEvent::Forward { target_milestone: m, .. }` event in log order, appending the dedup header + body for each; (2) **fix-loop growth** — concatenate every `FixLoopAppend { milestone: m, iter: i }` event in log order with `i < <iter being re-dispatched>` onto the result of (1). This matches v1's in-memory `accumulated_assignment` byte-for-byte (since v1 also appends forwarded bodies before the first dispatch), so a resumed fix-loop iteration sees the same prompt v1 would have built had it not crashed. `Forward` is the sole authority for phase (1); `FixLoopAppend` is the sole authority for phase (2); the two never overlap. |
| `REVIEWED` (no `MergeSnapshot` event for this `(milestone, iter)`) | **Re-run the reviewer.** v0.5's "re-take a snapshot from current HEAD" was unsafe: between crash and resume, an operator (or another worker) could have moved the milestone branch, and synthesising an "approved" SHA from current branch state would let unreviewed commits slip through. v2 only proceeds to merge when an explicit `MergeSnapshot` event is in the log. The reviewer-rerun prompt is reconstructed deterministically from persisted state, and is an **intentional departure** from v1's `runner.rs::reviewer_assignment` (which lines 487–509 embed a `truncate(implementer_output, 4000)` block alongside the diff instruction): in-memory implementer chat output is not available at resume, so v2 uses (a) **diff-only mode** (the default in M6) — `git diff <target_branch>...<milestone_branch>` against the milestone worktree (or `repo_root` pre-M12), preceded by the milestone's spec assignment text and any forwarded-in headers reconstructed from `Forward` replay (§3.9). The implementer's chat transcript is **omitted on purpose**, not by oversight. (b) **M8a-aware mode** (post-M8a, optional) — if the captured `DispatchSession` event for `(milestone, iter)` exists and the implementer session JSONL is present on disk, the reviewer prompt restores the truncated final-assistant-message block that v1 fed into `reviewer_assignment`, recovering parity with the v1 prompt shape. The runner picks (a) when M8a is not yet in the campaign log, (b) when it is. M6 ships only mode (a); the M8a upgrade is a one-line change in the prompt builder once captured-session paths are available. Either way the reviewer rerun never reads in-memory state from before the crash — every input is on disk in `state.jsonl` (spec text, `Forward` events, branch SHAs) plus `git`. |
| `REVIEWED` *and* a matching `MergeSnapshot` event exists | Enter `MERGE_PENDING` with the snapshot tuple from the event; identical to the `MERGE_PENDING` row below. |
| `MERGE_PENDING` | Run the merge queue with the replayed snapshot tuple. Cherry-pick is idempotent: if `target_branch` already contains a commit with the same `git patch-id --stable` as the snapshotted commit, record `MERGED` without invoking `git cherry-pick`. |
| `MERGED` / `FAILED` / `BLOCKED_ON_*` | Skip; the campaign report renders the recorded state. |

**Single-commit-per-milestone invariant.** v1's design
implicitly assumed each milestone branch contained exactly one
commit atop `target_branch`: `cherry_pick_to_target` cherry-
picks `<branch>` (a single commit reference), `MergeSnapshot`
records one `reviewed_branch_sha`, and `is_already_applied`
checks one candidate. But neither v1 nor any earlier draft of
this RFD *enforced* the invariant. A milestone branch with
two implementer commits (e.g. an implementer that ran `git
commit` twice during a fix-loop iteration) would silently
merge only the tip commit; the second-from-top would be lost.

v2 makes this an explicit invariant. New helper
`merge.rs::assert_single_commit(repo, branch, target) ->
Result<String, MergeError>` runs:

```text
git -C <repo> rev-list --count <target>..<branch>
```

and returns:

- `Ok(<sha>)` when the count is exactly `1` — the single
  commit's SHA becomes `reviewed_branch_sha` for the
  `MergeSnapshot`.
- `Err(MergeError::ZeroCommits)` when the count is `0` — no
  implementer work happened; reviewer should not have
  returned `READY_TO_MERGE`. The runner converts this into
  `BLOCKED_ON_REVIEW_STALE` so an operator can investigate.
- `Err(MergeError::MultipleCommits { count })` when the count
  is `>= 2`. The runner converts this into `FAILED` with
  `detail: "reason=multiple_commits_on_milestone_branch
  count=<n>"`. A future v3 may add an auto-squash policy; v2
  fails loud rather than silently dropping commits.

The check runs at exactly one place: immediately before
`MergeSnapshot` is emitted. By construction every
post-snapshot path (cherry-pick, `is_already_applied`, resume)
is guaranteed to operate on a single-commit branch. The
campaign's bundled implementer system prompt is updated to
include "produce exactly one commit per milestone; squash if
you commit twice"; the invariant gives the runner a hard
backstop independent of prompt discipline.

`assert_single_commit` is also called by
`--orchestrate-status` so the operator sees the count
violation as soon as the implementer returns, not only after
the reviewer says READY. **Status-side behaviour:** the call
runs against any milestone whose state is `DISPATCHED` or
`REVIEWED` (i.e. has a milestone branch with potentially
implementer commits on it); 0 commits or ≥2 commits surface as
an annotated **warning** in the status output (one line per
violation, e.g. `m1: WARN multi-commit (count=2); merge will
fail with reason=multiple_commits_on_milestone_branch`), not a
hard error. Status is read-only and must never refuse to print
a campaign overview just because a milestone is in a bad shape;
the operator is precisely the audience that needs to *see* the
violation. The hard-fail path (the runner's
`MergeError::MultipleCommits` → `FAILED` transition) only
fires inside the merge gate, where it is load-bearing.

**`MergeSnapshot` write timing.** The runner emits
`CampaignEvent::MergeSnapshot` *before* the
`Transition { from: "REVIEWED", to: "MERGE_PENDING" }` event,
and `emit_event`'s `sync_data` (M5) guarantees the snapshot
hits disk before the transition. Crashes between the two
events are recoverable — replay sees `REVIEWED` plus a
`MergeSnapshot` and proceeds to merge. Crashes *before* the
snapshot leave only `REVIEWED`, and the resume matrix above
forces a re-review. The single-commit invariant runs *before*
the snapshot is written, so a violating branch never produces
a snapshot at all.

**Idempotent cherry-pick.** New helper
`merge.rs::is_already_applied(repo, target_branch,
candidate_sha)` answers "does `target_branch` already contain
a commit that produces the same patch as `candidate_sha`".
Two earlier shapes were wrong: v0.3's `git rev-list $target |
git patch-id` (patch-id reads patches, not hashes); v0.4's
concatenated `git show --pretty=format:'' "$sha"` stream
(stripping commit headers removes patch-id's per-commit
boundary, yielding one aggregate id, not N pairs). The
correct shape runs `patch-id` once per SHA so each commit has
its own input boundary:

```text
# Build the set of stable patch-ids on the target branch.
declare -A PATCH_IDS=()
for sha in $(git -C $repo rev-list --first-parent \
                 --max-count=5000 "$target_branch"); do
    pid=$(git -C $repo diff-tree -p "$sha" \
            | git -C $repo patch-id --stable \
            | awk '{print $1}')
    if [ -n "$pid" ]; then PATCH_IDS[$pid]=$sha; fi
done

# Compute the candidate's stable patch-id the same way.
candidate_pid=$(git -C $repo diff-tree -p "$candidate_sha" \
                  | git -C $repo patch-id --stable \
                  | awk '{print $1}')

# Membership test → already_applied.
[ -n "${PATCH_IDS[$candidate_pid]:-}" ]
```

`git diff-tree -p <sha>` emits the patch `patch-id --stable`
wants (no commit header) and we run it once per SHA so each
invocation is its own stream. The Rust implementation invokes
the same primitives via `std::process::Command` and collects
results into a `HashSet<String>` of patch-ids. `--stable` is
mandatory: it makes the patch-id invariant under file-diff
reordering inside one patch (e.g. when git rewrites the
diff order via `-O<orderfile>` or version-dependent
heuristics) and produces ids that are comparable across git
versions configured the same way. The unstable default can
drift across git releases (manpage says so explicitly: the
unstable hash is "compatible with the patch ID value
produced by Git 1.9 and older"). Scan caps at the most
recent 5 000 first-parent commits; v3 may add a persistent
patch-id index. Empty-diff merge commits yield an empty
`patch-id` line and are skipped.

**CLI ownership (added by M6, not v1).** v1 ships only
`--orchestrate-dry-run`, `--orchestrate`, and
`--orchestrate-state-root` (`crates/pi-coding-agent/src/
cli.rs:242–256`). M6 introduces four new flags **from
scratch** — none of these exist in the live tree today:

- `--orchestrate-status <toml>`: replays state.jsonl, prints
  the per-milestone state and triggers the M8 report
  re-render.
- `--orchestrate-reset <toml> [--milestone <id>]`: for every
  milestone whose current state is `FAILED` or
  `BLOCKED_ON_*`, performs four steps in **this exact order**
  (the order matters: branch deletion must happen *before* the
  destructive worktree-removal step so a refusal short-circuits
  with no partial side effects on disk):
  1. **Detach the parent repo if necessary** — if `repo_root`'s
     `HEAD` is on any milestone branch about to be deleted, run
     `git -C <repo_root> checkout --detach <target_branch>`
     (with the same fallback to `checkout --detach HEAD` as the
     migration routine in §3.1). Reset reads `target_branch`
     from the live TOML's top-level field (same source as
     §3.1).
  2. **Delete the milestone branch ref** — `git -C <repo_root>
     branch -D <branch>`, ignoring "branch not found". If
     `git branch -D` *still* refuses because the branch is held
     by some non-campaign linked worktree elsewhere on disk
     (the same refusal shape §3.1 hits during migration), reset
     **fails loud** with `E_BRANCH_HELD_ELSEWHERE`, naming the
     offending worktree path and instructing the operator to
     either remove it (`git worktree remove --force <path>`) or
     detach it. Reset emits **no transition event** in this
     case, so the milestone state is unchanged on disk and the
     operator can retry once they've cleared the foreign
     worktree. We deliberately do not `worktree remove --force`
     on paths the campaign does not own — same rationale as
     §3.1. This mirrors §3.1's migration contract; the v0.11
     draft only had this rule for migration, leaving reset open
     to the same silent-stale-branch hazard the migration fix
     was meant to close.
  3. **Prune the campaign's worktree** — `git worktree remove
     --force` on the campaign-owned worktree path, plus
     `git worktree prune` to clear administrative entries. Only
     reachable if step 2 succeeded.
  4. **Emit** `CampaignEvent::Transition { to: "PENDING",
     detail: "reset-by-operator" }` so the next run starts the
     milestone fresh from `target_branch`. Only reachable if
     steps 2–3 succeeded; partial-cleanup states never emit a
     reset transition.

  With `--milestone <id>` the reset is targeted to a single
  milestone (which still must be `FAILED` or `BLOCKED_ON_*`);
  without `--milestone` the reset applies to *every* eligible
  milestone in the campaign. Each milestone runs the four-step
  sequence independently, so an `E_BRANCH_HELD_ELSEWHERE` on
  one milestone does not block reset on another. The targeted
  form is the safe default for operators who want to retry
  just one failure without disturbing the rest of the
  in-flight campaign; the all-eligible form is the big-hammer
  recovery for a campaign that has accumulated multiple
  terminals. Refuses to touch `MERGED` milestones (their
  cherry-picked commits in `target_branch` are permanent;
  resetting their branch ref does not un-merge anything but is
  misleading) and refuses to run while any milestone is
  in-flight (`DISPATCHED`, `REVIEWED`, `MERGE_PENDING`) — the
  operator must wait for the campaign to settle first. The
  branch-deletion step closes the v0.5 ambiguity about whether
  reset preserves prior branch contents: it does not. Combined
  with §3.11's "absent ref ⇒ create from `target_branch`",
  this gives reset a single contract — *milestone returns to a
  clean PENDING off `target_branch`*. The detach step closes
  the v0.9 hole that `git branch -D` errors when the branch is
  currently checked out in the parent repo (see §3.1 for the
  same primitive).
- `--orchestrate-re-review <toml> --milestone <id>`: clears a
  `BLOCKED_ON_REVIEW_STALE` terminal and re-takes the snapshot.
- `--orchestrate-migrate <toml>`: §3.1 / §3.2 v1→v2 directory
  + log lift. Refuses to overwrite an existing v2 dir.

All four ship in the M6 PR alongside the dispatch shim added
to `crates/pi-coding-agent/src/bin/pi.rs` (today the
orchestrate dispatch is at `bin/pi.rs:113–167`; M6 grows that
block from ~55 LOC to ~140 LOC). `startup.rs` is **not**
touched — orchestrate has never lived there.

**File / function.** `crates/pi-orchestrate/src/runner.rs`
(eligibility gate + `SpecSnapshot` write/check +
`FixLoopAppend` emit at every `accumulated_assignment.push_str`
call site for **same-milestone reviewer text** (`runner.rs:410`,
`runner.rs:442`) — note that the M10 forward-applied path emits
a `Forward` event instead, never a sibling `FixLoopAppend`, per
the §3.9 single-source rule + replay reconstruction of
`accumulated_assignment` from `FixLoopAppend` events, ~80 LOC),
`merge.rs::is_already_applied` (~70 LOC),
`merge.rs::assert_single_commit` (~30 LOC),
`crates/pi-coding-agent/src/bin/pi.rs` (the four new flags
wire into the same dispatch block as the existing
`--orchestrate*` family, ~130 LOC), `crates/pi-coding-agent/
src/cli.rs` (flag declarations including
`--milestone <id>`, ~30 LOC),
`runner.rs::migrate_v1_log` (~60 LOC, shared with §3.2;
includes the §3.1 detach-parent-repo helper).

**Acceptance test.**
`crates/pi-orchestrate/tests/resume_matrix.rs`:

1. Each row of the matrix gets a fixture state.jsonl plus a
   stub git repo, a re-run of the runner, and an assertion
   on the resulting state.jsonl tail.
2. `resume_after_partial_merge`: write events through
   `MERGE_PENDING`, perform the cherry-pick out-of-band,
   resume; assert no second cherry-pick attempted, terminal
   `MERGED`.
3. `resume_reviewed_without_snapshot_re_runs_reviewer`: log
   contains `REVIEWED` but no `MergeSnapshot` for the same
   `(milestone, iter)`. Assert resume re-dispatches the
   reviewer and does NOT call `cherry_pick_to_target`. The
   test mutates the milestone branch HEAD between the
   recorded `REVIEWED` event and the resume call; assert the
   reviewer (not a synthesised snapshot) is what re-validates.
4. `resume_reviewed_with_snapshot_proceeds_to_merge`: log
   contains `REVIEWED` plus a matching `MergeSnapshot`.
   Assert resume goes straight to `MERGE_PENDING` carrying
   the snapshot tuple (no extra reviewer dispatch).
5. `reset_deletes_branch_ref`: log a milestone in `FAILED`
   with `refs/heads/claude/m1` pointing at some commit `X`;
   run `--orchestrate-reset`; assert (a) the worktree dir is
   gone, (b) `git show-ref --verify refs/heads/claude/m1`
   exits non-zero (branch deleted), (c) the next
   `--orchestrate` invocation creates `claude/m1` fresh
   from `target_branch` (per §3.11) — i.e. `git
   merge-base --is-ancestor target_branch claude/m1` holds,
   and `claude/m1` does not contain `X` unless `X` was
   already on `target_branch`.
6. `reset_targeted_milestone`: log two milestones in `FAILED`
   (`m1`, `m2`); run `--orchestrate-reset --milestone m1`;
   assert `m1` is now `PENDING` and its branch ref is gone,
   while `m2` remains `FAILED` with its branch ref intact and
   its worktree dir still on disk.
7. `reset_refuses_in_flight`: log a milestone in
   `MERGE_PENDING`; `--orchestrate-reset` exits non-zero and
   touches no state.
8. `reset_refuses_merged`: log a milestone in `MERGED`;
   `--orchestrate-reset` exits non-zero and does not delete
   `claude/m1`.
9. `single_commit_invariant_zero`: build a milestone branch
   identical to `target_branch` (zero commits ahead); run the
   `assert_single_commit` gate; assert
   `BLOCKED_ON_REVIEW_STALE` and no `MergeSnapshot` event.
10. `single_commit_invariant_multiple`: build a milestone
    branch with two implementer commits ahead of
    `target_branch`; run the gate; assert `FAILED` with
    `detail` containing `multiple_commits_on_milestone_branch
    count=2` and no `MergeSnapshot` event.
11. `single_commit_invariant_one`: build a milestone branch
    with exactly one commit ahead; assert `MergeSnapshot` is
    emitted with `reviewed_branch_sha` equal to that single
    commit's SHA.
11a. `merge_snapshot_iter_on_forward_only_verdict`: simulate a
    reviewer verdict where every concern forwards to a
    descendant (so the source milestone's
    `accumulated_assignment` is unchanged after the verdict and
    no extra fix-loop redispatch happens). Run the merge gate
    on the source milestone at `iter=1`. Assert the emitted
    `MergeSnapshot.iter == 1` (not `0`); the forward-only
    branch must use the same 1-based `iter` value as every
    other verdict path. Re-running resume against this log
    must read the snapshot and proceed to merge without
    re-running the reviewer (i.e. the `(milestone, iter)`
    lookup in §3.4 succeeds). This pins the v0.12 removal of
    the `iter == 0` sentinel.
12. `patch_id_stable_show_pipeline`: two commits with
   reordered file diffs (the actual `--stable` guarantee per
   `git-patch-id(1)` — file-diff reordering, not hunk-line
   reordering inside a single file) yield identical
   `--stable` patch-ids when their diffs are fed through
   `git diff-tree -p … | git patch-id --stable`;
   non-`--stable` ids differ. Validates the §3.4 helper
   matches `git-patch-id(1)`'s actual stdin contract.
13. `patch_id_branch_scan_collects_per_commit_pairs`: build a
   target branch with two commits whose patches differ; run
   the `is_already_applied` helper's scan logic; assert the
   resulting `HashSet<String>` contains exactly two distinct
   patch-ids (not one aggregate id, the v0.4 bug). Then build
   a candidate commit whose patch is byte-equivalent to the
   second commit's patch (cherry-picked onto a side branch
   with a different parent SHA); assert `is_already_applied`
   returns `true`. Build a third candidate with a fresh patch;
   assert `false`.
14. `migrate_clean_baseline`: build a v1 (or pre-drift v2)
   state directory plus matching milestone branch refs and
   worktree dirs as if a previous campaign had failed
   mid-flight. `refs/heads/claude/m1` points at commit `Y`
   (downstream of `target_branch`); the worktree dir at
   `~/.pi/wt/data/<encoded-repo>/<campaign-id>--m1/` exists
   on disk; a `MERGE-REPORT-<slug>-<campaign-id>.md` exists
   at the repo root. Run `pi --orchestrate-migrate
   <toml>`. Assert: (a) the state dir is renamed to
   `…migrated-<ts>/`; (b) the worktree dir no longer exists
   (`git worktree list` does not include it); (c) `git
   show-ref --verify refs/heads/claude/m1` exits non-zero
   (branch ref deleted); (d) the report is renamed to
   `MERGE-REPORT-<slug>-<campaign-id>.migrated-<ts>.md`;
   (e) a fresh `spec.toml` exists at
   `<state-root>/<campaign-id>/spec.toml` with bytes equal
   to the live TOML; (f) the new `state.jsonl` first three
   events are `SchemaVersion { v: 2 }`, `SpecSnapshot {
   spec_sha256: <hash> }`, then the lifted Transitions; (g)
   a subsequent `pi --orchestrate <toml>` invocation
   creates `claude/m1` fresh from `target_branch` (i.e. `git
   merge-base --is-ancestor target_branch claude/m1` holds,
   and `claude/m1` does not contain `Y` unless `Y` was
   already on `target_branch`); (h) the new worktree dir
   created during that subsequent run is fresh (its `HEAD`
   matches `target_branch` initially, not the archived
   branch contents). This is the post-migrate clean-baseline
   guarantee that v0.8 lacked.
15. `migrate_detaches_parent_repo`: arrange `repo_root`'s
    `HEAD` to be on `claude/m1` (matching v1 runner
    behaviour, which checks out the milestone branch in the
    parent repo at `runner.rs:192`). Run
    `--orchestrate-migrate <toml>`. Assert (a) migration
    succeeds — *no* `cannot delete branch 'claude/m1' used by
    worktree` error — (b) `repo_root`'s `HEAD` is detached
    on the `target_branch` SHA after migration (verified via
    `git -C <repo> symbolic-ref -q HEAD` returning non-zero),
    (c) `refs/heads/claude/m1` is gone. This pins the v0.10
    detach precondition.
16. `reset_detaches_parent_repo`: same setup as #15, but with
    a `FAILED` milestone in the log; run
    `--orchestrate-reset --milestone m1` while `repo_root`'s
    `HEAD` is on `claude/m1`. Assert reset succeeds and the
    branch ref is gone, with the same detach behaviour. This
    pins the parallel detach contract for `--orchestrate-reset`.
17. `resume_reconstructs_fixloop_assignment`: build a fixture
    state.jsonl carrying, in order, transitions through
    `iter=1` and `iter=2` plus two
    `CampaignEvent::FixLoopAppend` events with bodies `"A"`
    and `"B"` for milestone `m1`. Truncate the log at a
    `Transition { from: "REVIEWED", to: "DISPATCHED" }` for
    `iter=3`. Resume the runner (with a stub dispatcher that
    records the assignment string passed to it). Assert the
    captured implementer assignment equals
    `m1.assignment + "A" + "B"` byte-for-byte — proving full
    resume reconstructs v1's in-memory `accumulated_assignment`
    from the persisted log alone.
18. `reset_branch_held_elsewhere`: same setup as #16 but
    additionally arrange a *non-campaign* linked worktree
    elsewhere on disk (e.g. `git -C <repo_root> worktree add
    /tmp/foreign claude/m1`) holding `claude/m1`. Run
    `--orchestrate-reset --milestone m1`. Assert (a) reset
    exits non-zero with `E_BRANCH_HELD_ELSEWHERE` mentioning
    `/tmp/foreign`; (b) `state.jsonl` has **no** new
    `Transition { to: "PENDING", detail: "reset-by-operator" }`
    event for `m1` — i.e. the milestone state on disk is
    unchanged; (c) `refs/heads/claude/m1` still exists; (d)
    the campaign's own worktree dir is **not** removed (step
    3 must not run after step 2 fails). This pins the §3.4
    reset-side mirror of the §3.1 `E_BRANCH_HELD_ELSEWHERE`
    contract and the documented step ordering.

**LOC estimate.** ~570 LOC (350 source + 220 tests).
**Dependencies.** §3.2 (typed events, including
`MergeSnapshot`, `SpecSnapshot`, and `FixLoopAppend`),
M5 (durable state — `MergeSnapshot`, `SpecSnapshot`, and
`FixLoopAppend` must hit disk before the related
transition / before the first dispatch).

### 3.5 M7 — Retry policy for transient failures

**Motivating example.** Two failure classes showed up in
dogfood: (a) provider returns HTTP 502 mid-stream and the
pi subprocess exits non-zero with a Transport error;
(b) the subprocess wedges silently. v1's dispatcher
(`dispatch.rs::RealDispatch::dispatch`) treats both as
terminal `FAILED`.

**Proposed primitive.** A `RetryPolicy` struct (defined under
"Liveness watchdog" below, since it carries the watchdog
configuration the next subsection introduces). Default
backoff `[30 s, 90 s]`; tests pass a no-op `sleep` closure so
retries run in <10 ms. Default `sleep` is
`std::thread::sleep` (we deliberately avoid
`tokio::time::sleep`; M11 introduces tokio at the scheduler
layer but `dispatch.rs` stays sync-subprocess).

**Classification.** Retried iff one of:

- exit code != 0 *and* stderr matches one of `Transport`,
  `Connection reset`, `EOF`, `502 Bad Gateway`, `503 Service
  Unavailable`, `504 Gateway Timeout`, `broken pipe`.
  Patterns live in a single `dispatch.rs::TRANSIENT_PATTERNS:
  &[&str]` for audit.
- watchdog tripped; the subprocess is killed and stderr/stdout
  captured up to that point; counts as transient.

Anything else (malformed verdict, missing agent definition,
git error, schema error) is **never** retried.

**Liveness watchdog.** v0.7 specified an *stdout-only* timer.
The reviewer correctly flagged this as wrong for the actual
`pi -p` dispatch path: assistant text deltas go to stdout
(`crates/pi-coding-agent/src/modes/print.rs:42`), tool-call
notices go to stderr (`eprintln!` lines 46, 49, 64, 68 of the
same file), and *successful* tool execution is **not** streamed
to the parent at all — the runtime reports tool start/stop on
stderr but the body of the tool call (e.g. a long
`cargo build`) produces no parent-visible bytes. In this repo,
a healthy implementer can therefore go silent on stdout for
many minutes during normal work; an stdout-only watchdog would
kill it.

v2 splits liveness into two layers:

1. **Hard wall-clock cap (`max_attempt`).** A per-attempt
   absolute deadline. Default 30 min, configurable via
   `defaults.attempt_timeout` in TOML and clamped to ≤2 h. If
   the subprocess has not exited by `max_attempt`, parent
   `Child::kill`s it; the kill counts as transient (retried
   under the policy above). This is the primary safety net and
   the only liveness criterion claimed for v2.
2. **Combined I/O-activity watchdog (`io_idle`).** Ticks on
   *either* stdout or stderr activity. Implemented with one
   `std::sync::mpsc` carrying `(Stream::Stdout|Stderr,
   bytes_read)` from two reader threads (one per pipe); if both
   pipes are silent for `io_idle`, parent calls `Child::kill`.
   Default `io_idle = 10 min` — long enough to ride out a
   `cargo build --workspace` cold compile in this repo (worst
   observed: ~6 min), short enough to surface a wedged
   subprocess before `max_attempt`. **Operators can disable the
   I/O watchdog entirely** by setting
   `defaults.io_idle_secs = 0` in TOML; in that case only the
   wall-clock cap applies. We keep the I/O watchdog in v2
   because it catches real wedges (the `c0c8a61` Transport-hang
   class) faster than 30 min, and stderr-coverage closes the
   "silent during a long tool call" hole.

   **Heuristic, not positive liveness.** `io_idle` is still
   activity-derived: a healthy implementer running a *truly
   silent* tool (e.g. a `bash` that loops in pure CPU work and
   writes nothing to either stdout or stderr) trips the
   watchdog at `io_idle_secs` even though the child is making
   progress. The dogfood we have observed (`cargo build`,
   `cargo test`, `cargo fmt`, `cargo clippy`) all stream
   progress to stderr so this is acceptable for v2; operators
   whose campaigns include silent tools should set
   `defaults.io_idle_secs = 0` until v3 introduces a proper
   child heartbeat (Open question 2).

A future v3 watchdog will key off an explicit child heartbeat
(e.g. a periodic `SessionEntryKind::Heartbeat` line, or a
streamed JSON progress event) once `pi -p` grows that channel;
the I/O-activity watchdog is the v2 stop-gap.

```rust
pub struct RetryPolicy {
    pub max_attempts: u32,            // default 3 (1 try + 2 retries)
    pub backoff: BackoffSchedule,     // default [30s, 90s]
    pub max_attempt: Duration,        // default 30 min, hard cap per attempt
    pub io_idle: Option<Duration>,    // default Some(10 min); None = disabled
    pub sleep: Arc<dyn Fn(Duration) + Send + Sync>,  // injectable
}
```

**Cherry-pick retry knob (renamed).** v1's schema field
`defaults.push_retry_max` is misnamed — the cherry-pick
operation it would govern is a *local merge*, not a push. M7
renames it to `defaults.merge_retry_max` (with a serde alias
for the old name so v1 TOMLs keep parsing) and wires it into
`merge.rs::cherry_pick_to_target` with backoff [5s, 15s, 60s].
A future actual-push step (e.g. auto-push to origin once a
campaign succeeds) would get its own `push_retry_max`.

**Consolidated `[defaults]` schema (M7-extended).** v2 inherits
v1's TOML schema unchanged at the top level — `target_branch`
stays a required top-level `String` (`crates/pi-orchestrate/src/
schema.rs:19`), and `--auto-approve` is a CLI/runtime flag, not
a TOML field (RFD 0021 explicitly says the campaign TOML does
not override `--auto-approve`). The only v1 `[defaults]` key
was `push_retry_max`. M7 grows the `[defaults]` table; reviewer
asked for one place that lists every key, type, unit, default,
and alias:

| Key                       | Type         | Unit  | Default            | Origin     | Alias            |
| ------------------------- | ------------ | ----- | ------------------ | ---------- | ---------------- |
| `merge_retry_max`         | `u32`        | tries | `3`                | M7 (v2)    | `push_retry_max` |
| `attempt_timeout`         | `String`     | dur   | `"30m"`            | M7 (v2)    | —                |
| `io_idle_secs`            | `u64`        | s     | `600` (10 min)     | M7 (v2)    | —                |
| `max_attempts`            | `u32`        | tries | `3` (1 + 2 retries)| M7 (v2)    | —                |

(Top-level fields — `target_branch`, `name`, `milestones`,
`override_rules` — are unchanged from v1 and not repeated here.)

`attempt_timeout` is parsed by `humantime::parse_duration` and
clamped to `[1s, 2h]`. `io_idle_secs = 0` disables the I/O
watchdog. `max_attempts = 1` disables retries (kill on first
failure). The `push_retry_max` alias is `#[serde(alias =
"push_retry_max")]` on `merge_retry_max` so v1 TOMLs keep
parsing; orchestrate emits a deprecation warning to stderr
when the alias is used.

**File / function.** `crates/pi-orchestrate/src/dispatch.rs`
(retry wrapper, ~120 LOC), `merge.rs` (cherry-pick retry,
~40 LOC), `runner.rs` (thread `RetryPolicy` through, ~10
LOC), `schema.rs` (rename + alias, ~5 LOC).

**Worktree scrub on retry (cross-cut with M12).** When the
retry wrapper decides an attempt was transient and an M12
worktree path exists for the milestone (post-M12 only — M7's
own tests run pre-M12 with no worktree state), the wrapper
invokes `pi_orchestrate::worktree::reset_worktree_to_branch
(path, branch)` *before* re-spawning the implementer. This is
the same primitive M12 calls on fresh-process resume; see the
redispatch table in §3.11 (paths B and C). Without this hook,
an implementer killed mid-edit by `max_attempt` / `io_idle`
would leak uncommitted changes into the retry attempt. The
hook lives behind a `Option<&MilestoneWorktreeState>`
parameter so M7 can ship before M12 (no-op when `None`); M12's
wiring fills it in. Acceptance test #2b below covers this
specifically; the cross-process variant is M12 test #11b.

**Acceptance test.**
`crates/pi-orchestrate/tests/retry_transient.rs`:

1. `tests/fixtures/flaky-pi.sh` exits 1 with `Transport:
   connection reset` on attempts 1+2, succeeds on 3. Assert
   terminal `MERGED`, two `CampaignEvent::Retry` events in
   `state.jsonl`, total wall <100 ms with no-op sleep.
2. `retry_exhaustion`: same fixture, exits 1 every time;
   `FAILED` with `reason=dispatch_error_after_retries`.
3. `wall_clock_kills_wedged_dispatch`: fixture `sleep 600`,
   `max_attempt = 50 ms`, no-op sleep; parent reaps within
   200 ms; counts as transient (one `Retry` event with
   `reason="attempt_timeout"`).
4. `io_idle_kills_silent_dispatch`: fixture writes one byte
   then `sleep 600`, `io_idle = 50 ms`,
   `max_attempt = 30 min`; parent reaps within 200 ms with
   `reason="io_idle"`.
5. `io_idle_resets_on_stderr`: fixture writes a byte to stderr
   every 30 ms while doing no stdout work for 1 s,
   `io_idle = 100 ms`; parent does **not** kill the child (the
   stderr-coverage hole). Assert no `Retry` event, normal exit.
6. `io_idle_disabled`: fixture as in #4 but
   `defaults.io_idle_secs = 0` and `max_attempt = 1 s`; parent
   reaps via wall-clock cap at ~1 s, never via `io_idle`.

**LOC estimate.** ~350 LOC (175 source + 175 tests).
**Dependencies.** §3.2 (Retry events).

### 3.6 Session capture (M8 prerequisite)

The reviewer is correct that v1 dispatch does not pass
`--session-dir` and does not capture the child's session id,
so M8's per-milestone cost rollup is not implementable
against the current dispatcher. M8 therefore ships in two
sub-steps:

**M8a — Session capture (prerequisite).**
`dispatch.rs::RealDispatch::dispatch` learns to:

1. Compute a per-call session dir
   `<state-root>/<campaign-id>/milestones/<mid>/<role>.<iter>/`
   and pass `--session-dir <that>` to the spawned `pi -p`.
   The flag already exists on the child
   (`crates/pi-coding-agent/src/cli.rs:77`) and is honoured by
   `startup.rs:197–205`.
2. **Pass an explicit top-level session pointer path** to the
   child via a new flag `--session-pointer <path>` (added to
   `crates/pi-coding-agent/src/cli.rs`, plumbed through
   `startup.rs` to `SessionManager`). The child's
   `SessionManager`, on creating its top-level session JSONL
   in `<base>/<cwd_slug>/<uuid>.jsonl`, also writes the
   absolute path of that JSONL file to `--session-pointer`
   atomically (`tempfile + rename` inside the same
   directory). This is the **explicit child-to-parent
   handoff** v0.5 lacked: directory-scan was ambiguous because
   `task`-tool subagents (RFD 0005) clone the same
   `SessionManager` base and create sibling JSONLs under
   `<cwd_slug>/`.
3. After the child exits, the orchestrator reads
   `--session-pointer`; the resolved path becomes
   `DispatchOutcome.session_jsonl: Option<PathBuf>` (a single
   value, not a directory listing). If the pointer file does
   not exist (e.g. the child crashed before
   `SessionManager::open`), `session_jsonl` is `None` and
   M8b's report renders `(no session captured)` for that
   dispatch.
4. The runner emits a `CampaignEvent::DispatchSession`
   (§3.2) **after every child exit, regardless of outcome**,
   with the resolved path (or empty string when `None`). The
   v0.4 draft proposed shoving the path into
   `Transition::detail` on `DISPATCHED → REVIEWED` and
   `→ FAILED`, which silently dropped the path on a
   *successful* implementer run.

**Why pointer file, not env-injected session id.** v3 may
switch to a child-emitted JSON line on stdout once
streaming-stdout (§"Out of scope v2") lands; for v2 the
filesystem is the simplest reliable handoff that survives a
child crash mid-write (the `rename` is atomic, so the parent
sees either the pre-update content or the post-update
content, never a torn write).

This is real work, not bookkeeping. Acceptance test
`crates/pi-orchestrate/tests/session_capture.rs`:

1. Stub dispatcher writes a pointer file pointing at a fake
   JSONL in the supplied `--session-dir`; assert exactly one
   `CampaignEvent::DispatchSession` per child exit, with the
   pointer-resolved path and `exit_status`.
2. Successful campaign with implementer + reviewer for one
   milestone: assert two `DispatchSession` events
   (`role="implementer"` and `role="reviewer"`), with the
   implementer event present even though the milestone never
   left `DISPATCHED` between dispatch start and reviewer
   return (the v0.4 hole).
3. **Sibling-JSONL disambiguation.** Stub dispatcher writes
   *three* JSONL files into `--session-dir` (one top-level
   plus two created by nested `task` subagents). Only the
   pointer file names which one is the top-level — there is
   no distinguishing field on the JSONLs themselves;
   `SessionManager::create` writes `parent_id: None` on
   *every* session's `Meta` entry (`crates/pi-agent-core/src/
   session.rs:217–220`), so v0.6's "top-level has `parent_id:
   None`, descendants don't" claim was incorrect. The pointer
   file is the *only* disambiguation. Assert
   `DispatchOutcome.session_jsonl` equals the pointed file,
   **not** a directory of all three, even though all three are
   indistinguishable from their `Meta` entries alone.
4. **Crash before pointer write.** Stub dispatcher exits 137
   without writing the pointer. Assert
   `DispatchOutcome.session_jsonl == None` and
   `CampaignEvent::DispatchSession.session_jsonl` is empty;
   M8b's render prints `(no session captured)` for that row.
5. Real `pi -p` integration test (skipped under `which::which
   ("pi").is_err()` per the lsp-tool skip pattern): assert
   the pointer file exists at the expected path after a
   one-shot prompt and resolves to a real session JSONL.

**LOC estimate (M8a).** ~140 LOC (90 source + 50 tests).
**Dependencies.** §3.1 (campaign-id), §3.2 (typed events).

### 3.7 M8 — `MERGE-REPORT` writer

**M8b** (the actual writer).

**Proposed primitive.** New module
`crates/pi-orchestrate/src/report.rs`:

```rust
pub fn render_merge_report(
    spec: &Campaign,
    state: &CampaignState,
    log: &[CampaignEvent],
) -> String { … }

pub fn write_merge_report(
    repo_root: &Path,
    spec: &Campaign,
    state: &CampaignState,
    log: &[CampaignEvent],
) -> std::io::Result<PathBuf> { … }
```

Sections (per RFD 0021 §"Output"):

1. Header (campaign id, name, started/ended).
2. Cost rollup. Walk every `CampaignEvent::DispatchSession`
   event (§3.2), open each `session_jsonl` file, and **sum
   the persisted `usage.cost_usd` field**
   (`crates/pi-ai/src/message.rs:117–127`) across every
   `SessionEntryKind::Usage` entry. That is what the runtime
   already computed and stored; re-deriving cost in the report
   risks a drift between rendered and recorded numbers. We do
   *not* feed `Usage` into `pi_ai::cost::compute_cost` — that
   takes `(ModelInfo, UsageAcc)` and the session file does not
   expose either shape directly. (A v3 follow-up may parse the
   session `Meta` entry to resolve `provider`/`model` →
   `ModelInfo`, convert tokens into `UsageAcc`, and recompute;
   v2 trusts the persisted value.)

   **Rollup scope** (open question 6): v2 sums only the
   top-level implementer and reviewer sessions captured by
   M8a. Descendant `task`-tool subagent sessions
   (RFD 0005) write their own JSONLs but `TaskBatchResult.usage`
   is currently zeroed (`crates/pi-coding-agent/src/native/
   task/executor.rs`). v3 will recurse into descendants once
   that propagation lands; v2 explicitly under-reports cost
   when a milestone delegates to subagents and the report
   carries a `(top-level only — subagent costs not included)`
   footer. The footer is emitted iff any walked session JSONL
   contains a `SessionEntryKind::ToolCall` entry whose inner
   `pi_ai::ToolCall::name == "task"` (i.e. the actual session
   schema at `crates/pi-agent-core/src/session.rs:21–45` —
   there is no `SessionEntryKind::Task` variant; the v0.5
   draft's reference to one was wrong).
3. Per-milestone outcome table.
4. Override decisions: every `CampaignEvent::Forward` event
   maps to a row rendered directly from the event's fields:
   `m1 → m2 (iter N), reviewer=<reviewer_session_jsonl>,
   line L1–L2: "<body>" (sha <body_sha256_prefix>)`. Because
   `body` lives on the event itself, the report renderer
   never has to re-open the reviewer JSONL or re-derive text
   from another source — replay alone is sufficient, even if
   the reviewer session file has since been moved or
   garbage-collected.
5. Final test sweep: a TODO marker until the dogfood
   automates `cargo test --workspace` at end of campaign
   (deferred to v3).
6. Deviations: any `BLOCKED_ON_*` terminal.

**When to write.** v1 reviewer is right: rewriting after
every transition is overengineered and creates watcher noise.
v2 writes the report:

- Once at run-end (success or fatal).
- Once per `--orchestrate-status` invocation, regenerated
  from the live `state.jsonl`.

That's it. The function is pure (same `state.jsonl` →
same bytes), so re-running `--orchestrate-status` is safe.

**File / function.** `crates/pi-orchestrate/src/report.rs`
(~250 LOC), `runner.rs` (call at run-end, ~10 LOC),
orchestrate shim (call from `--orchestrate-status`, ~10
LOC).

**Acceptance test.**
`crates/pi-orchestrate/tests/merge_report_render.rs`:

1. Fixture campaign with three milestones (two MERGED, one
   FAILED): render and snapshot-test the markdown body.
2. `report_idempotent`: render twice, byte equality.
3. `report_cost_rollup`: fixture `state.jsonl` containing
   three `CampaignEvent::DispatchSession` events whose
   `session_jsonl` files each carry one or more
   `SessionEntryKind::Usage` entries with known `cost_usd`
   values; assert the rendered total equals the sum of those
   `cost_usd` values and that no `compute_cost` re-derivation
   runs (i.e. removing `pi-ai::cost` from the import list of
   `report.rs` does not break the test). Crucially, one of the
   three events represents a *successful* implementer session
   that produced no `Transition` change between dispatch and
   the reviewer's return — this is the v0.4 hole the typed
   event closes.
4. `missing_session_jsonl_is_soft`: delete one of the
   `DispatchSession` files; report renders with
   `(session lost)` and the total reflects only the remaining
   files.
5. `subagent_footer`: a session JSONL containing a
   `SessionEntryKind::ToolCall` entry whose
   `call.name == "task"` triggers the
   `(top-level only — subagent costs not included)` footer.
   A control session with only `Assistant` / `Usage` / a
   non-`task` `ToolCall` (e.g. `bash`) does not.

**LOC estimate.** ~400 LOC (260 source + 140 tests).
**Dependencies.** M5 (durable state), M8a (session capture).

### 3.8 M9 — Structured `Concerns` parser

**Motivating example.** RFD 0021 §"Reviewer parser contract"
specs structured-mode parsing of `## Concerns` bullets; v1
parses only the FINAL `Merge readiness:` line and feeds the
entire reviewer text back to the implementer. That blocks
override forwarding (M10).

**Proposed primitive.** `verdict.rs::parse_concerns(text:
&str) -> ConcernsBlock`:

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct Concern {
    pub body: String,
    pub line_start: usize,    // 1-based
    pub line_end: usize,
    pub kind: ConcernKind,    // Bullet | ImplicitProse
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConcernsBlock {
    Structured { concerns: Vec<Concern>, verdict: MergeReadiness },
    Fallback   { reason: FallbackReason, verdict: MergeReadiness },
}
```

Bullet boundaries: a bullet body runs from `- ` / `* ` to the
next bullet, the next blank line, the next `## ` heading, or
EOF. Continuation lines (≥2 leading spaces, or the next
non-empty unindented line that is itself not a bullet, for a
single-paragraph continuation) are folded.

Implicit-prose chunks become `ConcernKind::ImplicitProse`
concerns. They are **always in-scope** and never matched
against override rules (M10 enforces).

The existing `verdict.rs::parse_verdict` is kept as a
back-compat shim that calls `parse_concerns` and returns the
inner verdict.

**File / function.** `crates/pi-orchestrate/src/verdict.rs`
(~200 LOC additional).

**Acceptance test.**
`crates/pi-orchestrate/tests/parse_concerns.rs`:

1. Bullet-only verdict → 3 concerns, `Structured`.
2. Bullets interleaved with prose → bullets + N
   `ImplicitProse` concerns, `Structured`.
3. Missing `## Concerns` heading → `Fallback{NoHeading}`.
4. Heading present, zero bullets → `Fallback{NoBullets}`.
5. Missing `Merge readiness:` → `Fallback{NoVerdict}`.
6. Continuation-line folding.
7. Position tracking: `line_start`/`line_end` round-trip.

**LOC estimate.** ~430 LOC (200 source + 230 tests).
**Dependencies.** None. Pre-req for M10.

### 3.9 M10 — Override-rule forwarding

**Motivating example.** RFD 0021 §"Override rules" specs the
regex-match → `forward_to` flow; v1 has the schema and
validator but the runtime never evaluates the rules.

**Proposed primitive.** New module
`crates/pi-orchestrate/src/forward.rs`:

```rust
pub enum ForwardOutcome {
    InScope,
    Forwarded { target: String, header: String },
    ForwardFailed { target: String, reason: ForwardFail },
}

pub fn evaluate_concern(
    concern: &Concern,
    rules: &[OverrideRule],
    target_states: &HashMap<String, String>,
) -> ForwardOutcome { … }
```

First match wins. Implicit-prose concerns bypass rules → always
`InScope`. A successful forward appends the concern body to
the target milestone's assignment with a stable dedup header:

```text
<!-- forwarded from <source-id>:<concern.line_start>
     body=<sha256(body)[:8]> -->
```

We pick a **content-hash header** (not a timestamp). The
reviewer raised this as Open Question 4 in v0.1; the resolution
is: timestamps double-append on operator `state.jsonl`
edits-and-replay, content-hashes do not. The temporal trace
lives in the `CampaignEvent::Forward` event itself
(`ts` field), not in the assignment append.

Resume rebuilds the appended-assignment map by replaying
`CampaignEvent::Forward` events in order. Each event carries
the full forwarded `body`, the source/target milestone ids,
and `body_sha256_prefix`; the resume routine re-appends the
header + body to the in-memory copy of `target_milestone`'s
assignment iff the `body=<prefix>` header is not already
present. Because the body is on the event itself, replay is
self-sufficient and does not need to re-read the (possibly
deleted) reviewer session JSONL. That gives **exactly-once
forwarding under arbitrary replay**, including after
`--orchestrate-reset` clears the source milestone state.

**Replay authority — single-source rule.** `Forward` is the
**sole** persisted source for descendant-assignment mutation
*before* the descendant's first dispatch. Crucially, M10 does
**not** also emit a sibling `FixLoopAppend` for the forwarded
body: that would create two replay authorities for the same
bytes, and (worse) `FixLoopAppend.iter` is 1-based for the
producing iter on the *target* milestone, which has no legal
value when the target is still `PENDING` (the previous draft
hand-waved this as `iter=0`, which the §3.2 schema rejects).
The split is therefore strict, by phase:

- **Phase 1 — base assignment** (before `m`'s first dispatch):
  start from `m.assignment` (spec text), apply every
  `Forward { target_milestone: m, .. }` event in log order.
- **Phase 2 — fix-loop growth** (after `m` has dispatched):
  apply every `FixLoopAppend { milestone: m, .. }` event in
  log order with `iter < <iter being re-dispatched>`.

Phase 1 events never produce a `FixLoopAppend`; Phase 2 events
never produce a `Forward`. The §3.4 DISPATCHED resume row and
the §3.2 acceptance tests #8 (same-milestone) and #9
(forward-then-crash) pin both halves of this contract. A rare
edge case — a forward arriving at `m` *while `m` is already
in-flight* — is forbidden by the §3.9 forward-target-state
gate (`Forwarded` only fires if the target state is `PENDING`;
otherwise the concern is `ForwardFailed` and falls back to
in-scope, which is a same-milestone reviewer block and
therefore correctly emits a `FixLoopAppend` on the *source*
milestone, not on the target). So by construction the two
replay authorities never address the same milestone-iter pair.

**Counter discipline.** Per RFD 0021 v1.3 decision #3:
`fix_loop_max` ticks only on in-scope concerns. If every
concern in a verdict forwards successfully, the milestone
transitions to `MERGE_PENDING` (forward-only verdict path).
If some concerns forward and some are in-scope, the in-scope
ones re-dispatch the implementer and the counter ticks once
for the entire turn.

**File / function.** `crates/pi-orchestrate/src/forward.rs`
(~150 LOC), `runner.rs` (rule eval in the verdict-handling
branch, ~80 LOC).

**Acceptance test.**
`crates/pi-orchestrate/tests/forward_eval.rs`:

1. Rule `"(?i)e2e"` forwards an `e2e` bullet to `m2`; assert
   `CampaignEvent::Forward` event carries the full bullet
   `body`, `source_iter`, and `reviewer_session_jsonl`, and
   `m2`'s assignment ends with the dedup header + body.
2. Forward target not PENDING → `ForwardFailed`, in-scope
   fallback, fix-loop counter ticks.
3. First-match-wins ordering with two overlapping rules.
4. Resume after a successful forward: re-running the runner
   does not duplicate the appended block (header dedup).
5. **Replay-from-log-only.** Capture a state.jsonl after a
   forward, then *delete* the reviewer session JSONL the
   forward referenced; resume the runner; assert the
   appended assignment is reconstructed identically (the
   body comes from the event, not from re-reading the
   session). This is the v0.5 hole the body-on-event change
   closes.
6. All-out-of-scope verdict → `MERGE_PENDING`.

**LOC estimate.** ~430 LOC (230 source + 200 tests).
**Dependencies.** M9.

### 3.10 M11 — Parallel execution

**Hard prerequisite.** **`PI_ORCHESTRATE_PARALLEL > 1` ships
only when M12 is also in.** The reviewer is right: without
worktrees, two implementers fighting over the shared HEAD is
unsafe. The implementation order is therefore:

1. M11 lands the *scheduler infrastructure* (semaphore, merge
   queue, state-mutex, eligibility loop) **with the env var
   silently capped to 1 regardless of value, behind no
   user-facing flag**. The internal default
   stays sequential. No parallel acceptance tests are claimed.
2. M12 lands worktree-per-milestone.
3. **M11's second commit** (~20 LOC, same PR or follow-up)
   lifts the internal cap to `min(env_var, 4)` and turns on
   the parallel acceptance tests. The reviewer noted this is
   not really a separate milestone — it's the "enable" half
   of M11 — and the implementation plan table treats it as
   a single trailing line under M11 rather than a
   numbered milestone of its own.

If M11 and M12 land in the same release window we may merge
them; the staged version above is the safe default.

**Runtime boundary.** v1's runner is sync and so is
`dispatch.rs::RealDispatch::dispatch` (it uses
`std::process::Child::wait_with_output()`). M11 does **not**
rewrite dispatch as async. Instead:

- The orchestrate scheduler runs on a `tokio::runtime::Builder
  ::new_current_thread().enable_all().build()` runtime created
  inside the orchestrate entry point.
- One milestone's blocking dispatch body — plus the M7
  watchdog and retry wrapper — runs under
  `tokio::task::spawn_blocking`, holding one semaphore permit
  for its lifetime.
- `emit_event` runs `write_all + sync_data` inside
  `spawn_blocking` while holding the
  `tokio::sync::Mutex<File>`.

Why not full async dispatch? The blocking subprocess body is
~80 LOC and the LLM round-trip dominates wall time;
`tokio::process::Command` would force an auditable rewrite
out of v2 scope.

**Concurrency model (post-M11.1).**

- **Worker pool.** `tokio::sync::Semaphore` with `min(env_var,
  4)` permits, default 2.
- **Eligibility.** A milestone is eligible when every
  `depends_on` is `MERGED` and its current state is `PENDING`.
  `tokio::sync::Notify` wakes the scheduler on each transition.
- **Scheduler.** Single async task picks eligible milestones
  in topological order (deterministic tie-break by id).
- **Merge queue.** Single-threaded. `tokio::sync::Mutex`
  guards the cherry-pick subprocess. Workers transitioning to
  `MERGE_PENDING` enqueue onto a `tokio::sync::mpsc`.
- **state.jsonl serialisation.** A `tokio::sync::Mutex<File>`
  guards `emit_event`; M5's `sync_data` runs **inside** the
  lock so writes from N workers serialise into a totally-
  ordered durable log. Outside-the-lock fdatasync admits a
  window where on-disk order does not match in-memory order,
  which bites the M6 resume invariant; the ~50–200 µs cost is
  negligible next to LLM I/O.
- **Error containment.** Worker panics convert to
  `CampaignEvent::Transition { to: "FAILED",
  detail: "reason=worker_panic" }` via outer `catch_unwind`.

**Cap rationale.** Beyond 4, two implementers `cargo build`-ing
the same workspace invalidate each other's `target/`. v3 may
revisit with sccache or per-worktree target redirection.

**Determinism.** Tests that assert event ordering pin
`PI_ORCHESTRATE_PARALLEL=1` (the *user-facing* meaning of 1
is unchanged from M11.1 onwards).

**File / function.** `crates/pi-orchestrate/src/runner.rs`
(scheduler rewrite, ~250 LOC), `Cargo.toml` (add
`tokio = { version = "1", features = ["sync", "macros",
"rt-multi-thread"] }` if not already present).

**Acceptance test (claimed at M11.1, not M11).**
`crates/pi-orchestrate/tests/parallel_runner.rs`:

1. Three independent milestones with stub dispatchers each
   `sleep 200ms`. PARALLEL=3 → wall <500 ms; PARALLEL=1 →
   >600 ms.
2. Merge queue serialises: stub two milestones with
   conflicting cherry-picks; one `MERGED`, one
   `BLOCKED_ON_CONFLICT`, no race.
3. state.jsonl event ordering: PARALLEL=4 with 8 stub
   milestones; every line parses; per-milestone `from`/`to`
   chains are linearisable.
4. Worker panic: stub dispatcher panics on a specific id; that
   id ends `FAILED`, the others complete.

**LOC estimate.** ~570 LOC total (270 source + 300 tests),
M11+M11.1 combined. The implementation-plan table on §M11
tracks the ~270 *source* LOC for budget accounting; the
~300 LOC of test code is real but does not roll up into
"source LOC shipped". Both numbers refer to the same body
of work, just sliced differently.
**Dependencies.** M5 (durable writes), §3.2 (typed events),
**M12 (hard prereq for any user-visible parallelism)**.

### 3.11 M12 — Worktree-per-milestone

**Motivating example.** Without M12, parallel execution (M11)
fights over the shared repo's HEAD. Implementer m4 does
`git checkout claude/m4`, m5 does `git checkout claude/m5`,
their `cargo build` trees stomp each other.

**Crate dependency direction (prerequisite refactor).** v0.12
of this RFD called `pi_coding_agent::native::worktree::ensure`
and `pi_coding_agent::native::worktree::git::run` directly from
`pi-orchestrate`. The reviewer correctly flagged this as
unimplementable: `pi-coding-agent`'s `Cargo.toml` already lists
`pi-orchestrate.workspace = true`
(`crates/pi-coding-agent/Cargo.toml:47`), so calling
`pi-coding-agent` internals from `pi-orchestrate` produces a
cycle. M12 therefore lands behind a small **prerequisite
refactor** that this RFD owns:

> **Prereq M12-pre — extract worktree helpers into a new crate
> `pi-worktree`.** Move
> `crates/pi-coding-agent/src/native/worktree/{mod.rs,git.rs,
> reconcile.rs,baseline.rs}` verbatim into a new crate
> `crates/pi-worktree/src/{lib.rs,git.rs,reconcile.rs,
> baseline.rs}`. The new crate has no dependency on
> `pi-orchestrate` or `pi-coding-agent`; its dependencies are
> exactly the worktree module's current ones (`tokio`,
> `thiserror`, `dirs`, `tracing`, `serde`). `pi-coding-agent`'s
> `native::worktree` module becomes a one-line `pub use
> pi_worktree::*;` re-export, preserving every existing import
> path inside `pi-coding-agent` (RFD 0006 callers, the `task`
> tool executor, etc.). Tests under
> `crates/pi-coding-agent/tests/worktree_*.rs` either move with
> the helpers or stay where they are and exercise the re-
> exports — both work because the public API is unchanged.
>
> `pi-orchestrate` then adds `pi-worktree.workspace = true` and
> imports `pi_worktree::{ensure, git, worktree_dir,
> WorktreeError}` directly. **No circular dependency**: the
> graph becomes `pi-coding-agent → pi-orchestrate →
> pi-worktree` and `pi-coding-agent → pi-worktree` (a fan-in,
> not a cycle).
>
> Estimated cost: ~30 LOC of `Cargo.toml` plumbing, file moves,
> and one re-export. The refactor is mechanical (`git mv` plus
> `mod` declarations) and goes in as commit 1 of the M12 PR
> with no behaviour change. Acceptance: `cargo build
> --workspace` and `cargo test --workspace` both pass after the
> refactor with no other source changes.

We considered three alternatives and rejected them:

1. **Move the orchestrator runtime into `pi-coding-agent`.**
   Inverts the current dependency direction, requires touching
   every call site in `bin/pi.rs`, and conflates the
   orchestrate scheduler with the agent runtime — exactly what
   RFD 0021 split apart. Largest blast radius; rejected.
2. **Duplicate the worktree wrapper inside `pi-orchestrate`.**
   Cheapest in commits, but two copies of `git worktree add`
   wrapping logic drift; the reconciler's `ConflictedBranch`
   classifier (`reconcile.rs:124–163`) is exactly the kind of
   subtle invariant that should not exist twice. Rejected.
3. **Extract only `mod.rs` + `git.rs` (skip
   `reconcile.rs`/`baseline.rs`).** Tempting (M12 only needs
   `ensure` and `git::run`), but the reconciler is a single
   coherent unit and `pi-coding-agent`'s callers already
   import from `native::worktree::reconcile`. Splitting two of
   the four files would force a second extraction in v3 when
   M11/M12 want the conflict classifier. Reject for the
   smaller current-cost.

**Proposed primitive.** Each implementer/reviewer dispatch
runs in its own worktree. The reconciler's `ensure` is a
fresh-allocate primitive (§2.4 #1: it tears down any prior
checkout at the stable path, then `worktree_add_detached`s a
new one). v2 builds **reuse-across-fix-loop-iterations** as
an orchestrator-owned policy on top: the orchestrator
remembers the worktree path on the campaign state and only
calls `ensure` once per `(campaign-id, milestone-id)`.

**Single-worktree-per-branch preflight.** Git refuses to
check out a branch if any other linked worktree already has
it checked out. The locally observed message takes the form
`fatal: '<branch>' is already used by worktree at '<path>'`.
v1's runner left `repo_root` itself sitting on the most-
recently-dispatched milestone's branch
(`runner.rs:192` does `git_checkout(repo_root, &m.branch)`
directly), and an operator can also leave a non-campaign
worktree pointing at any milestone branch. M12 must therefore
preflight both cases on the **normal allocation path**, not
only on `--orchestrate-reset` / `--orchestrate-migrate`:

1. **Parent-repo detach (once per run).** Before the very
   first `allocate_milestone_worktree(...)` call in a campaign
   run, the orchestrator inspects
   `git -C <repo_root> symbolic-ref --short HEAD`. If that
   resolves to any milestone branch in the live spec, the
   orchestrator runs
   `git -C <repo_root> checkout --detach <target_branch>`
   exactly once and emits a `tracing::info!` log line. The
   detach is *not* recorded as a `CampaignEvent` because it
   touches the parent checkout, not orchestrator state; an
   already-detached parent triggers no action. This guarantees
   that no milestone branch is held by `repo_root` itself when
   the milestone worktrees go to check it out.
2. **Foreign-worktree refusal mapping.** If the
   `git checkout <branch>` step inside the freshly-allocated
   milestone worktree refuses because some non-campaign linked
   worktree elsewhere on disk holds `refs/heads/<branch>`, the
   allocator does **not** retry — it surfaces an
   `E_BRANCH_HELD_ELSEWHERE` error matching the §3.1/§3.4
   contract. The error names the offending worktree path
   parsed from `git`'s stderr and is mapped to a `FAILED`
   transition with `detail: "reason=branch_held_elsewhere
   path=<other-path>"` so the operator sees it in
   `--orchestrate-status`. Match key: non-zero exit status of
   the `git checkout` invocation combined with the presence of
   an offending-worktree path token in stderr; we do not text-
   match the diagnostic message itself.

```rust
async fn allocate_milestone_worktree(
    repo_root: &Path,
    campaign_id: &str,
    milestone_id: &str,
    branch: &str,            // milestone branch, e.g. "claude/m4"
    target_branch: &str,     // campaign target, e.g. "main"
    state: &MilestoneWorktreeState, // orchestrator-owned reuse memo, keyed by milestone_id
) -> Result<MilestoneWorktree, AllocateError> {
    // `task_id` is only the file-system path key fed to
    // `pi_worktree::ensure(repo_root, task_id)` (the existing API
    // takes a stable string and turns it into a worktree path under
    // `~/.pi/wt/data/<encoded-repo>/<task_id>/`). The orchestrator's
    // `MilestoneWorktreeState`, by contrast, is keyed strictly by
    // `milestone_id` because that is what every other v2 event
    // (Transition, DispatchSession, MergeSnapshot, Forward,
    // FixLoopAppend, Worktree) is keyed on. Don't conflate the two.
    let task_id = format!("{campaign_id}--{milestone_id}");

    // First call this run: fresh-allocate a detached worktree
    // at the stable path. Subsequent calls in the same run
    // (fix-loop iter, reviewer dispatch): reuse the
    // remembered path, validate it still exists.
    let path = match state.recorded_path(milestone_id) {
        Some(p) if p.exists() => p,
        _ => {
            let p = pi_worktree::ensure(
                repo_root, &task_id,
            ).await?;
            state.record_allocation(milestone_id, &p);
            p
        }
    };

    // Branch checkout. Two cases:
    //   (a) refs/heads/<branch> already exists (rerun, resume,
    //       prior milestone iteration that left a branch
    //       behind): check it out *without* `-B`, so we never
    //       reset the existing branch ref.
    //   (b) the branch does not exist: create it from an
    //       explicit `target_branch` startpoint. NOT from
    //       implicit HEAD — `repo_root` may have any branch
    //       checked out at the moment we're called, and a
    //       parallel run (M11) makes that even less
    //       deterministic.
    //
    // The allocator's caller is responsible for having run the
    // once-per-run parent-repo detach preflight before the
    // first call (see "Single-worktree-per-branch preflight"
    // above); this function does not re-run it.
    let branch_exists = pi_worktree::git::run(
        &path,
        &["show-ref", "--verify", "--quiet",
          &format!("refs/heads/{branch}")],
        "show-ref",
    ).await.is_ok();

    let checkout_result = if branch_exists {
        pi_worktree::git::run(
            &path, &["checkout", branch], "checkout-existing-branch",
        ).await
    } else {
        pi_worktree::git::run(
            &path,
            &["checkout", "-b", branch, target_branch],
            "checkout-new-branch",
        ).await
    };

    // Map "branch held by another linked worktree" git refusals
    // to E_BRANCH_HELD_ELSEWHERE so the runner can record a
    // FAILED transition with reason=branch_held_elsewhere
    // instead of partially allocating + retrying. The match key
    // is the non-zero exit status of `git checkout` plus an
    // offending-worktree path token in stderr (matching the
    // shape `used by worktree at '<path>'`); we do not text-
    // match the diagnostic verbatim.
    if let Err(e) = checkout_result {
        if let Some(other_path) = parse_held_elsewhere(&e) {
            return Err(AllocateError::BranchHeldElsewhere {
                branch: branch.to_string(),
                other_worktree: other_path,
            });
        }
        return Err(AllocateError::Git(e));
    }
    Ok(MilestoneWorktree { path, task_id })
}
```

Note: this RFD does **not** assume a `git::create_or_reuse`
(the v0.1 draft cited that name in error). The primitive is
`pi_worktree::ensure` (fresh allocation, post-extraction) plus
an orchestrator-side "did we already allocate this run?" memo,
plus an explicit-startpoint branch checkout via the existing
`git::run` helper. The v0.4 draft used `git checkout -B
<branch>` from implicit HEAD, which silently reset the
milestone branch (or started it from whatever the parent
checkout happened to be on); v0.5 fixes this by branching
case (a) "ref exists, plain checkout" from case (b) "create
from `target_branch`".

**Project-local state must not move with the worktree.**
M12's headline change is "dispatch CWD becomes the milestone
worktree path", but the spawned `pi -p` child resolves a
*much wider* set of project-local namespaces relative to its
process cwd than just agent definitions. Auditing the live
tree:

- **Top-level orchestrate agent definitions** —
  `crates/pi-orchestrate/src/dispatch.rs:66`'s
  `load_agent_spec` reads `<repo_root>/.pi/agents/<name>.md`
  by joining `.pi/agents/<name>.md` onto the dispatcher's
  `cwd` argument.
- **Nested `task`-tool subagents** (RFD 0005) —
  `crates/pi-coding-agent/src/native/task/tool.rs:131-133`
  takes `repo_root = ctx.cwd.clone()` and feeds it to
  `discovery::load_all(&repo_root)`, which walks
  `<repo_root>/.pi/agents/`
  (`crates/pi-coding-agent/src/native/task/discovery.rs:9-12`).
- **Project settings, prompts, skills, system prompts,
  extensions** —
  `crates/pi-coding-agent/src/context.rs::project_dir()`
  returns the bare relative `PathBuf::from(".pi")`. Every
  consumer (`settings_paths`, `prompts_dirs`, `skills_dirs`,
  `system_prompt_paths`, `themes_dirs`, plus the explicit
  `PathBuf::from(".pi").join("extensions")` in
  `crates/pi-coding-agent/src/startup.rs:267`) joins onto
  that relative path — i.e. resolves it against the child
  process's actual cwd at startup time.
- **Session manager cwd-slug** —
  `crates/pi-coding-agent/src/startup.rs:205` constructs
  `SessionManager::on_disk(session_dir, cwd.clone())`. The
  session manager hashes that cwd into its on-disk layout
  (`<base>/<cwd_slug>/<uuid>.jsonl`), so under a worktree
  cwd the session JSONL lands under a different slug than
  any other invocation in the same repo.

`AGENTS.md` is fine in a worktree because it is git-tracked
and therefore checked out into the worktree by `git
checkout`. The breakage is the **untracked / project-private
`.pi/*` namespace** plus nested-subagent discovery. `git
ls-files '.pi/**'` returns nothing in this repo, so a fresh
worktree under `~/.pi/wt/data/...` will contain *no* `.pi/`
directory at all.

The narrow v0.14 fix — splitting only the dispatcher's `cwd`
into `agents_root` + `cwd` — solves only the first bullet.
v0.15 promotes this to a child-runtime-wide concept:
**`project_root` (where `.pi/*` is resolved from) and `cwd`
(where tool execution actually runs) are two distinct
inputs to the spawned child**. The orchestrator passes both;
the child-side runtime threads `project_root` through every
project-local namespace lookup. This refactor changes
`.pi/*` discovery roots only — ordinary tool path
resolution (e.g. `read`, `write`, `bash`, the file paths
the model passes to the worktree-local `cargo`) continues
to resolve relative to `cwd`. `project_root` exists solely
so that operator-private, untracked `.pi/*` definitions
that live in the campaign repo follow the campaign across
its worktrees.

This is a real prerequisite refactor that lives below the
orchestrator. It owes its own row in the M12 prereq matrix:

> **Prereq M12-pre-2 — child-side `--project-root` plumbing.**
>
> 1. `crates/pi-coding-agent/src/cli.rs`: add
>    `--project-root <PATH>`, default `None`. When `None`,
>    fall back to `current_dir()` (so existing v1 invocations
>    behave identically).
> 2. `crates/pi-agent-core/src/runtime.rs::RuntimeConfig`:
>    add `project_root: PathBuf`. Built in
>    `crates/pi-coding-agent/src/startup.rs` from
>    `cli.project_root.clone().unwrap_or_else(|| cwd.clone())`.
> 3. Rewire project-local lookups in
>    `crates/pi-coding-agent/src/context.rs`:
>    `project_dir()` becomes `project_dir(project_root: &Path)
>    -> PathBuf` returning `project_root.join(".pi")`. All
>    callers in `context.rs` (`settings_paths`,
>    `prompts_dirs`, `skills_dirs`, `themes_dirs`,
>    `system_prompt_paths`) take a `project_root: &Path`.
>    Note: `skills_dirs()` today returns three roots —
>    `<HOME>/.pi/skills`, the literal `.agents/skills`
>    (legacy compatibility entry), and `.pi/skills`
>    (`crates/pi-coding-agent/src/context.rs:48-53`). M12-pre-2
>    reroots both project-relative entries:
>    `.agents/skills` becomes
>    `project_root.join(".agents").join("skills")` and
>    `.pi/skills` becomes `project_root.join(".pi").join(
>    "skills")`. The legacy `.agents/skills` path is rerooted
>    identically; we do not silently drop it (some long-lived
>    operator setups depend on it, and the live code keeps it
>    on purpose). The `<HOME>` user-scope entry is unaffected.
> 4. `crates/pi-coding-agent/src/startup.rs`: thread
>    `project_root` into `prompts.load_all(&prompts_dirs(
>    project_root))`, into `skills.load_all(&skills_dirs(
>    project_root))`, into `system_prompt_paths(project_root)`,
>    into `themes_dirs(project_root)`, into `settings_paths(
>    project_root)`, and into the `ext_roots` builder (replace
>    the literal `PathBuf::from(".pi").join("extensions")` with
>    `project_root.join(".pi").join("extensions")`).
>    `discover_context_files(&cwd, &agent_dir(), &[…])` is
>    **deliberately left unchanged**: it walks `cwd` ancestors
>    looking for the `AGENTS.md` / `CLAUDE.md` tracked
>    convention (`crates/pi-agent-core/src/context.rs:12-32`).
>    `AGENTS.md` is already inside any milestone worktree
>    because git checks tracked files into the worktree, and
>    its semantics are intentionally per-checkout (a
>    milestone branch is allowed to add or amend an `AGENTS.md`
>    paragraph that affects that milestone's runs only). The
>    "discovery roots only change for `.pi/*`" bound stated
>    below applies here verbatim — `AGENTS.md` is not a `.pi/*`
>    lookup and is not rerooted. `SessionManager::on_disk(
>    session_dir, cwd.clone())` is similarly **left unchanged**
>    — the second argument is both the on-disk slug input
>    (`cwd_slug()` at `crates/pi-agent-core/src/session.rs:193`)
>    *and* the value persisted as `SessionMeta.cwd` /
>    `SessionEntryKind::Meta { cwd }` (`session.rs:210`,
>    `:222`), and pi-stats ingest treats that field as the
>    session's folder key (`crates/pi-stats/src/ingest.rs:92–
>    100`). Rewriting it to `project_root` would make a
>    session executing in a milestone worktree falsely
>    advertise the repo root as its cwd, breaking pi-stats'
>    per-folder roll-ups. The cost is a per-worktree session
>    slug under the M12 layout (one extra subdirectory per
>    milestone under `<base>/<cwd_slug>/`); accepted as the
>    semantically correct trade-off. If a future RFD wants
>    stable storage keys without lying about the recorded
>    cwd, the right shape is a separate
>    `SessionManager::on_disk_with_storage_key(base, cwd,
>    storage_key)` constructor; not in v2 scope.
> 5. `crates/pi-tools/src/lib.rs` (`pi_tools::ToolContext`,
>    the actual home of `ToolContext` — it is **not** in
>    `pi-agent-core`; `pi-agent-core` consumes the type via
>    `pi-tools.workspace = true` in `crates/pi-agent-core/
>    Cargo.toml` but does not re-export it): add
>    `project_root: PathBuf` next to the existing
>    `cwd: PathBuf` on the public struct. Update `Default`
>    so direct constructions in tests still compile (set
>    `project_root` to the same `current_dir()` fallback
>    `cwd` uses today). `crates/pi-agent-core/src/runtime.rs`
>    populates the new field from `RuntimeConfig::project_root`
>    at the construction site that already builds the
>    runtime's `ToolContext`. Blast radius: `pi-tools` is a
>    leaf crate, but `ToolContext` is referenced directly
>    from `pi-agent-core`, `pi-coding-agent`,
>    `pi-orchestrate` (test fixtures), and several test-only
>    constructors across the workspace; the prereq adds
>    `..Default::default()` to those literals where
>    appropriate.
> 6. `crates/pi-coding-agent/src/native/task/tool.rs:131-133`:
>    replace `let repo_root = ctx.cwd.clone();` with
>    `let repo_root = ctx.project_root.clone();`. Nested
>    subagent discovery now reads `<project_root>/.pi/agents/`
>    even when the child cwd is a worktree.
>
> Estimated cost: ~190 LOC (60 in `cli.rs` + signature
> changes, 50 in `context.rs` + callers, 40 in `startup.rs`,
> 10 in `runtime.rs` + 15 in `pi-tools/src/lib.rs`
> (`ToolContext` field + `Default` update + workspace test
> fixture touch-ups), 30 in `task/tool.rs`
> plus its tests). Pure plumbing; no behaviour change for
> the default `--project-root == cwd` case. Acceptance:
> `cargo build --workspace` and `cargo test --workspace`
> both pass after the refactor with no orchestrate code yet
> consuming the new flag.

The orchestrator-side leg — passing `--project-root <repo_root>`
on dispatch — stays in `pi-orchestrate` and is the smallest
slice of M12 proper. We considered three alternatives and
rejected them:

1. **Copy `.pi/*` into each worktree on allocation.**
   Duplicates a source-of-truth that needs to stay live
   across edits (an operator editing `.pi/agents/code-
   reviewer.md` mid-campaign would not see the change reach
   later milestones); also racy under M11 parallel
   allocation. Rejected.
2. **Require users to commit `.pi/*` so worktrees inherit it
   via `git checkout`.** Contradicts the existing project-
   private convention. Many `.pi/agents/*.md` files contain
   model + thinking knobs that are operator-specific and
   intentionally untracked. Rejected.
3. **Symlink `<wt>/.pi -> <repo_root>/.pi` on allocation.**
   Tempting; trips on Windows-equivalent worktree backends
   in v3 and on tools that follow symlinks for write-side
   effects (`cargo` does, in practice). Rejected as a
   portability tax for cosmetic gain.

The dispatcher's signature gains one parameter
(`project_root`) and passes it through twice — once to
`load_agent_spec` (for top-level agent lookup), once to the
spawned `pi -p` as `--project-root <project_root>`:

```rust
pub fn load_agent_spec(project_root: &Path, name: &str)
    -> std::io::Result<AgentSpec> {
    let path = project_root.join(".pi").join("agents").join(format!("{name}.md"));
    // ...rest unchanged
}

impl Dispatch for RealDispatch {
    fn dispatch(
        &self,
        role: DispatchRole,
        agent_name: &str,
        assignment: &str,
        project_root: &Path,  // always the campaign repo_root
        cwd: &Path,           // M12: milestone worktree; pre-M12: == project_root
    ) -> std::io::Result<DispatchOutcome> {
        let agent = load_agent_spec(project_root, agent_name)?;
        // ...
        cmd.arg("--project-root").arg(project_root);
        cmd.current_dir(cwd);
        // ...
    }
}
```

`project_root` is always the campaign's `repo_root` — every
*project-private* `.pi/*` lookup the child does (project
agents under `.pi/agents/`, project extensions under
`.pi/extensions/`, project skills under `.pi/skills/`,
project prompts under `.pi/prompts/`, project settings,
etc.) is rooted there. This does not redefine the broader
agent-discovery layering: nested `task`-tool subagents
follow RFD 0005 precedence (`Project > User > Bundled`),
where `discovery::load_all` still reads the user-level
`~/.pi/agent/agents/` and bundled definitions in addition
to the **project** root. The v2 contract is just that the
*project* leg of that lookup must resolve from
`project_root`, never from a milestone worktree's process
cwd. The orchestrator's top-level `load_agent_spec`
(`dispatch.rs:66`) is project-only by design — it is the
campaign-driver lookup, not the nested-subagent lookup —
and remains project-only in v2; we deliberately do not
fall through to user/bundled there. `cwd` is what the
spawned `pi -p` child sees as its current directory (the
milestone worktree under M12, the campaign `repo_root`
pre-M12).

Three new acceptance tests pin the wider surface; they live
in `crates/pi-orchestrate/tests/worktree_lifecycle.rs` for
the orchestrator legs and in `crates/pi-coding-agent/tests/
project_root_split.rs` for the child-side leg:

- `dispatch_resolves_agents_from_repo_root_not_worktree`
  (orchestrator): create `.pi/agents/x.md` only in
  `repo_root`, leave the worktree free of any `.pi/`
  directory, assert the dispatcher loads the spec.
- `dispatch_resolves_project_extensions_from_repo_root`
  (child-side, in-process): create
  `repo_root/.pi/extensions/marker/pi-extension.json`
  declaring a single stub tool (`pi-extension.json` is the
  manifest filename `extensions::discover` consumes —
  `crates/pi-coding-agent/src/extensions.rs:243`). Drive
  `startup::assemble(cli)` with `cli.project_root =
  Some(repo_root)` and the process cwd set to a tempdir
  lacking `.pi/`. Assert `Startup.extensions` contains the
  marker manifest **and** `Startup.runtime_config.tools`
  exposes the marker tool's `ToolSpec` (no separate
  `--list-tools` CLI surface required; this is a direct
  in-process check against the live `ToolRegistry`).
- `nested_task_resolves_project_subagents_from_repo_root`
  (child-side, in-process): create `repo_root/.pi/agents/
  sub.md`. Build a `pi_tools::ToolContext` with
  `cwd = <worktree-style tempdir lacking .pi/>` and
  `project_root = <repo_root>` (the new field added in
  M12-pre-2 above). Drive
  `crates/pi-coding-agent/src/native/task/tool.rs::TaskTool::
  resolve_repo_root(&ctx)` (or, equivalently, the inlined
  `let repo_root = ctx.project_root.clone();` after the
  rename in M12-pre-2 step 6) and assert it equals
  `repo_root`, then call
  `discovery::load_all(&repo_root)` and assert it returns a
  spec for `sub`. The test does not need a live model and
  does not need a `RuntimeContext` — it exercises only the
  discovery call against the new `ToolContext.project_root`
  field, since that is the contract M12-pre-2 changes.

**Resume-time worktree reuse — and the dirty-state hazard.**
After a process restart (`pi --orchestrate <toml>` re-invoked
on an existing v2 campaign), the orchestrator must rebuild
`MilestoneWorktreeState` before the runner picks any
milestone. The replay rule walks `CampaignEvent::Worktree`:

```text
for each CampaignEvent::Worktree { milestone, path, action } in order:
    match action:
        "allocated" | "reused" → state.record_allocation(milestone, path)
        "pruned"               → state.forget(milestone)
```

But filesystem state on a recorded path is **not** part of
`state.jsonl` or git's commit graph. A child crashing in
`DISPATCHED` after editing files but **before committing**
leaves a dirty index plus untracked files on the milestone
branch checkout. Naïvely "reusing" that path on resume would
silently inherit those edits into the next implementer
dispatch — they would land in whatever the next iteration
commits, with no record of where they came from. That
contradicts the rest of the RFD, which insists campaign
state is reconstructible from `state.jsonl` + git.

v2 therefore makes worktree reuse **stateful only about the
path, not about the working-tree contents**. The contract is:
any time the orchestrator is about to redispatch into a
worktree whose previous occupant did **not** end with a clean
commit, it hard-resets the worktree to the milestone branch
ref before redispatch. Concretely, the scrub runs on every
redispatch after an **abnormal implementer exit** —
regardless of whether the orchestrator process restarted in
between:

```text
fn reset_worktree_to_branch(path, branch):
    git -C path reset --hard refs/heads/<branch>
    git -C path clean -fdx
```

The session-pointer file (M8a, §3.6) lives **outside** the
worktree at `<state-root>/<campaign-id>/milestones/<mid>/
<role>.<iter>/.pi-session-pointer`, so `clean -fdx` does not
need to carve it out — it is not a child of the worktree path
in the first place. (A v0.19 draft had a `-e
'.pi-session-pointer*'` exclusion; the reviewer flagged that
the carve-out path was wrong, and v0.20 simply drops it.)

After this step, the worktree's tree state is exactly the tip
commit of the milestone branch, identical to a fresh `ensure`
followed by `git checkout`.

**Three redispatch paths, two of which need the scrub.**

| Path | Triggered by | Scrub required? | Why |
| ---- | ------------ | --------------- | --- |
| **A. Same-process clean fix-loop** | Reviewer returned `NEEDS_FIX`; the child's prior implementer attempt exited 0, `git -C <wt> commit -m "…"` ran, the new commit is on `refs/heads/<branch>`. | **No.** | The previous attempt's tree state is the committed tip; reusing the worktree as-is is correct. |
| **B. Same-process retry after abnormal exit** | M7 retry path: the implementer was killed by `max_attempt` / `io_idle`, exited non-zero with a transient stderr pattern, or the dispatcher otherwise classified the attempt as transient. The child may have edited files but never committed. | **Yes.** | The child died mid-edit; uncommitted changes plus untracked files would silently leak into the retry. M7's retry wrapper invokes `reset_worktree_to_branch(path, branch)` before re-spawning the implementer for the next attempt. |
| **C. Fresh-process resume** | `pi --orchestrate <toml>` re-invoked after the orchestrator process itself exited (crash, Ctrl-C, host reboot). Replay rebuilds `MilestoneWorktreeState` from `CampaignEvent::Worktree`. | **Yes.** | The orchestrator has no in-process memory of whether the recorded worktree's last occupant exited cleanly; the conservative rule is to scrub on any post-replay reuse. |

Path **A** is the only path that skips the scrub, and it does
so only because the same orchestrator process is the one that
ran the commit and therefore knows the tree is the committed
tip. **Paths B and C share the same primitive**
(`reset_worktree_to_branch`); the only difference is who calls
it: M7's retry wrapper for path B, the M12 replay rebuild step
for path C. The acceptance tests below cover both:
`retry_after_abnormal_exit_does_not_inherit_dirty_worktree`
(test #11b, in-process retry path) and
`resume_does_not_inherit_dirty_worktree` (test #11, fresh-
process resume path).

If a `recorded_path` no longer exists on disk (operator did
`git worktree remove` out-of-band, or filesystem rolled
back), `allocate_milestone_worktree` falls back to the
`ensure` path — the failure-injection test "worktree dir
already exists from a previous run" exercises this branch
specifically.

`MilestoneWorktree` carries an RAII drop guard that does NOT
prune by default; pruning is explicit on terminal `MERGED`.
On `FAILED` and `BLOCKED_ON_*` the worktree is retained for
operator autopsy and is removed only by an explicit
`--orchestrate-reset` (M6); the same `reset --hard` + `clean`
contract applies the next time the milestone re-enters
`DISPATCHED` after `--orchestrate-reset` *if* reset somehow
fails to delete the worktree, but the normal path is "reset
removes the worktree, the next allocation calls `ensure`
fresh". An acceptance test (§3.11 #11 below) drives the
crash-mid-edit case end-to-end: dirty implementer state in a
recorded worktree must not leak into the next dispatch's
diff.

**Parent-repo working-tree precondition.** A second, related
hazard concerns the **parent checkout's own** WIP, not the
milestone worktrees. v1 dispatched implementers in
`repo_root` directly, so any staged/unstaged/untracked edits
the operator had in the parent checkout were visible to
implementer tooling (and could be inadvertently committed by
a `git add -A`). M12 moves dispatch into milestone worktrees,
which means the parent checkout's WIP is no longer reachable
from a child. That is a real semantic shift, not just
plumbing. RFD 0006 (worktree reconciler) already defines
`pi_worktree::baseline::{capture_baseline, apply_baseline}`
for the symmetric "preserve operator WIP across worktree
runs" use case in `task`-tool subagents, but extending that
to `pi --orchestrate` would make a campaign run silently
inherit the operator's parent-repo WIP into every milestone
worktree — almost always wrong for orchestrate (the operator
typically wants milestones rooted at `target_branch`, not at
their personal `git stash`).

v2 picks **option A: require a clean parent repo**, not
option B (`capture_baseline` / `apply_baseline`). The
allocator's once-per-run preflight runs
`git -C <repo_root> status --porcelain=v1`; if the output is
non-empty, allocation aborts with `E_PARENT_REPO_DIRTY`
before any worktree is created and before the parent-repo
detach step runs. The error message names every dirty path
and tells the operator to commit, stash, or `git clean` and
retry. Rationale: orchestrate runs are long, automated, and
land cherry-picks against `target_branch`; tying their
correctness to whatever the operator happened to have in the
parent index is an unforced foot-gun. Operators who
genuinely want WIP propagation can stash, run the campaign,
and `git stash pop`. The clean-repo requirement is recorded
in §3.4's CLI surface (`--orchestrate` on a dirty parent
prints the dirty paths and exits non-zero) and exercised by
acceptance test §3.11 #12. `pi_worktree::baseline::*` stays
in `pi-worktree` for `task`-tool callers that *do* want WIP
propagation; `pi-orchestrate` deliberately does not call it.

**Lifecycle.**

| Event | Worktree action |
| ----- | --------------- |
| Milestone enters `DISPATCHED` for the first time. | Fresh-allocate via `ensure`; orchestrator memoises the path. Emit `CampaignEvent::Worktree { action: "allocated" }`. |
| Fix-loop iteration. | Reuse via the orchestrator memo (no second `ensure` call). Emit `CampaignEvent::Worktree { action: "reused" }`. |
| Reviewer dispatch. | Reuse the implementer's worktree. No event (the path is unchanged). |
| Cherry-pick from this milestone. | Read `reviewed_branch_sha` from the worktree's branch ref; the cherry-pick itself runs in the *target-branch* checkout (the bare repo root or a dedicated target worktree), not in a milestone worktree. |
| Terminal `MERGED`. | `git worktree remove --force` via `git::worktree_try_remove` (`git.rs:102`, which only invokes `git worktree remove --force` — there is no `prune` step inside the helper), followed by an explicit `git worktree prune` invocation through `git::run` to clear the administrative entry. Emit `CampaignEvent::Worktree { action: "pruned" }`. |
| Terminal `FAILED` / `BLOCKED_ON_*`. | **Retain** the worktree for operator autopsy. No `pruned` event. `--orchestrate-reset` is the explicit cleanup path, and (per §3.4) it also deletes `refs/heads/<milestone-branch>` so the milestone next runs from a clean baseline. |

**Conflict surface.** The reconciler classifies base-HEAD-moved
subagent commits as `ConflictedBranch` (`reconcile.rs:124–163`).
v2 maps that to `BLOCKED_ON_REVIEW_STALE` exactly as v1 does
for review-snapshot mismatch — the merge queue's two
"can't merge cleanly" pathways converge.

**File / function.**
`crates/pi-worktree/{Cargo.toml,src/{lib.rs,git.rs,
reconcile.rs,baseline.rs}}` (new crate via verbatim move from
`pi-coding-agent`; ~0 net LOC plus ~30 LOC of plumbing — see
the prereq-refactor block above), `crates/pi-coding-agent/
src/native/worktree/mod.rs` (collapses to a `pub use
pi_worktree::*;` shim, ~10 LOC),
`crates/pi-orchestrate/Cargo.toml` (add
`pi-worktree.workspace = true`, ~1 LOC),
`crates/pi-orchestrate/src/worktree.rs` (new, ~210 LOC —
allocation, reuse, prune, drop guard, parent-detach
preflight, `parse_held_elsewhere` stderr classifier,
`AllocateError` variants, `MilestoneWorktreeState` keyed by
`milestone_id`, importing
`pi_worktree::{ensure, worktree_dir, git, WorktreeError}`),
`crates/pi-orchestrate/src/dispatch.rs` (rename `cwd` →
`project_root` + add a separate `cwd`, rewire
`load_agent_spec`, pass `--project-root <project_root>` to
the spawned `pi -p`, `Command::current_dir(cwd)`, ~50 LOC
delta — bumped from v0.14's ~40 to absorb the
`--project-root` argv push and the new symmetry rename).
The child-side leg (`crates/pi-coding-agent/src/cli.rs`,
`crates/pi-coding-agent/src/startup.rs`,
`crates/pi-coding-agent/src/context.rs`,
`crates/pi-coding-agent/src/native/task/tool.rs`,
`crates/pi-agent-core/src/runtime.rs`,
`crates/pi-tools/src/lib.rs` — `pi_tools::ToolContext`) lives
in **prereq
M12-pre-2** above and totals ~190 LOC; it is *not* counted in
M12's LOC line below since the prereqs are tracked
separately. `crates/pi-orchestrate/src/runner.rs` (thread
`repo_root` into the dispatch call as `project_root`; the
milestone worktree path becomes the new `cwd`, ~20 LOC
delta), `crates/pi-orchestrate/src/merge.rs` (explicit
assert that cherry-pick runs outside any milestone
worktree, ~10 LOC).

**Acceptance test.**
`crates/pi-orchestrate/tests/worktree_lifecycle.rs`:

1. Three milestones; three worktrees exist after `DISPATCHED`,
   disappear on `MERGED`.
2. Reused across fix-loop iterations: count `git worktree
   add` invocations; exactly one per milestone (the
   orchestrator memo prevents a second `ensure` call).
3. `BLOCKED_ON_CONFLICT` retains the worktree; subsequent
   `--orchestrate-reset` removes it.
4. `FAILED` retains the worktree; `--orchestrate-reset`
   removes it **and** deletes `refs/heads/<branch>`. (The
   branch-ref half is also exercised in `tests/
   resume_matrix.rs::reset_deletes_branch_ref` from M6;
   this test pins the M12 worktree-side half.)
5. Two parallel milestones operate on disjoint worktrees;
   asserting an implementer's `touch <wt>/foo` is visible only
   inside its own path.
6. **`allocate_detaches_parent_repo_first_call`.** Set
   `repo_root` to a checkout that is sitting on
   `refs/heads/<m1-branch>` before any M12 allocation runs.
   Assert that after the first
   `allocate_milestone_worktree(...)` call, the parent repo's
   `git symbolic-ref --short HEAD` returns nothing (detached)
   and that `git rev-parse HEAD` matches the resolved
   `target_branch` tip. Re-running allocation for a second
   milestone in the same campaign run does **not** detach
   again (the preflight is once-per-run).
7. **`allocate_branch_held_elsewhere_fails_clean`.** Set up a
   second linked worktree outside the campaign that has
   `claude/m1` checked out. Run M12 allocation for the
   campaign's `m1`. Assert: (a)
   `allocate_milestone_worktree(...)` returns
   `AllocateError::BranchHeldElsewhere` carrying the foreign
   worktree path; (b) the runner records a `FAILED` transition
   with `detail: "reason=branch_held_elsewhere
   path=<other-path>"`; (c) no partial state is written
   (no `Worktree { action: "allocated" }` event landed for
   `m1`); (d) once the foreign worktree is removed and the
   operator runs `--orchestrate-reset --milestone m1`, the
   next `pi --orchestrate <toml>` allocates cleanly.
8. **`dispatch_resolves_agents_from_repo_root_not_worktree`.**
   Create `repo_root/.pi/agents/x.md` with valid frontmatter.
   Allocate a milestone worktree under `~/.pi/wt/data/...`.
   Confirm `<wt>/.pi/agents/x.md` does not exist. Run a
   `RealDispatch::dispatch(role, "x", _, repo_root,
   wt_path)` and assert the agent spec loads (the test stubs
   the actual `pi -p` child with a noop binary so the test
   asserts on `load_agent_spec` reachability + `current_dir`,
   not on a real model call).
9. **`dispatch_resolves_project_extensions_from_repo_root`**
   (lives in `crates/pi-coding-agent/tests/
   project_root_split.rs`; cited here for the M12 contract).
   Create `repo_root/.pi/extensions/marker/pi-extension.json`
   declaring a stub tool (`pi-extension.json` is the manifest
   filename `extensions::discover` consumes, per
   `crates/pi-coding-agent/src/extensions.rs:243` — note this
   is JSON, not TOML). Drive `startup::assemble(cli)`
   in-process with `cli.project_root = Some(repo_root)` and
   the process cwd set to a tempdir lacking `.pi/`. Assert:
   (a) `Startup.extensions` contains the marker manifest;
   (b) `Startup.runtime_config.tools.specs()` contains the
   marker tool's name. (No `--list-tools` CLI surface is
   introduced or relied on.)
10. **`nested_task_resolves_project_subagents_from_repo_root`**
    (also in `crates/pi-coding-agent/tests/
    project_root_split.rs`). Create `repo_root/.pi/agents/
    sub.md`. Build a `pi_tools::ToolContext` with
    `cwd = <worktree-style tempdir lacking .pi/>` and
    `project_root = <repo_root>` (the new field added in
    M12-pre-2). Invoke the `task` tool's `discovery::load_all`
    callsite under `crates/pi-coding-agent/src/native/task/
    tool.rs:131-133` — rewired in M12-pre-2 to use
    `ctx.project_root` rather than `ctx.cwd` — and assert the
    `sub` agent definition is found. The test does not spawn
    a real subagent; it asserts the discovery resolution leg
    only.
11. **`resume_does_not_inherit_dirty_worktree`.** Drive a
    one-milestone campaign past `DISPATCHED -> REVIEWED`,
    then crash the orchestrator process (test stub returns a
    panic from the dispatcher). Before re-invoking
    `pi --orchestrate <toml>`, write an *uncommitted* file
    `<recorded_path>/leaked.rs` and dirty an existing tracked
    file in the worktree. Re-run. Assert that immediately
    after replay rebuilds `MilestoneWorktreeState` and the
    runner picks the milestone for the next dispatch:
    (a) `git -C <recorded_path> status --porcelain=v1` is
    empty (the resume-time `reset --hard` + `clean -fdx`
    contract ran); (b) `<recorded_path>/leaked.rs` is gone;
    (c) `git rev-parse HEAD` in the worktree equals the
    `refs/heads/<branch>` tip recorded in the `MergeSnapshot`
    or, if no snapshot yet, the same SHA as the recorded
    branch ref before resume. (d) The next implementer dispatch's
    diff against `target_branch` does not contain `leaked.rs`.
    The test exercises the contract that filesystem state on
    a recorded path is not considered durable; only the
    `state.jsonl` log + git refs are.
11b. **`retry_after_abnormal_exit_does_not_inherit_dirty_worktree`.**
    Same orchestrator process throughout; no crash. Drive a
    one-milestone campaign to first `DISPATCHED`. Stub the
    implementer dispatcher to (a) write
    `<recorded_path>/leaked.rs` and dirty a tracked file inside
    the worktree, then (b) sleep past `defaults.io_idle_secs`
    (e.g. configure `io_idle_secs = 1` in the test fixture and
    sleep 5 s) so the M7 watchdog kills the child with
    `reason="io_idle"`. The watchdog classifies this as
    transient and the retry wrapper schedules a second
    attempt. Before the retry's `Child::spawn`, assert:
    (a) `git -C <recorded_path> status --porcelain=v1` is
    empty (M7's retry wrapper invoked
    `reset_worktree_to_branch` *before* re-spawning);
    (b) `<recorded_path>/leaked.rs` is gone;
    (c) the `Retry` event in `state.jsonl` carries
    `reason="io_idle"` (the original kill cause), not a fresh
    failure. The retry's stub now exits 0 with a clean commit;
    assert the campaign reaches `MERGED` and the cherry-picked
    diff against `target_branch` does not contain `leaked.rs`.
    This test pins path **B** of the redispatch table — same
    process, abnormal prior exit — independent of the path-C
    fresh-process-resume coverage in test #11.
12. **`orchestrate_aborts_on_dirty_parent_repo`.** Stage and
    leave unstaged edits in `repo_root` itself (e.g.
    `echo X > foo.txt && git add foo.txt`, plus
    `echo Y > bar.txt` untracked). Run `pi --orchestrate
    <toml>`. Assert: (a) the process exits non-zero with
    `E_PARENT_REPO_DIRTY`; (b) the printed message includes
    the offending paths (`foo.txt` staged, `bar.txt`
    untracked); (c) **no** worktree is allocated (the
    `~/.pi/wt/data/<encoded-repo>/<campaign-id>--m1/` path
    does not exist); (d) the parent repo's HEAD is unchanged
    (no spurious detach happened); (e) `state.jsonl` was not
    created or modified for this run. After cleaning the
    parent repo (`git stash` + `rm bar.txt`), re-running
    `pi --orchestrate <toml>` succeeds.

**LOC estimate.** ~640 LOC owned by M12 itself (380 source
+ 260 tests; v0.18's ~570 grew by ~70 to absorb the
resume-time `reset --hard` + `clean -fdx` contract in
`crates/pi-orchestrate/src/worktree.rs`, the `git status
--porcelain` precheck in `allocate_milestone_worktree` /
the once-per-run preflight, the new `AllocateError::
ParentRepoDirty` variant, and tests #11 + #12). The
child-side prereq M12-pre-2 adds ~190 LOC outside M12 in
`pi-coding-agent` + `pi-agent-core` and is tracked in the
prereq matrix above, **not** in this LOC line.
**Dependencies.** §3.2; prereq M12-pre (extract
`pi-worktree`); prereq M12-pre-2 (child-side
`--project-root` plumbing).

### 3.12 M13 — Sandboxed dispatch (blast-radius containment)

**Honest framing.** `pi_sandbox::LocalProcessProvider` is
**not** a security boundary today. It executes a tool from
the registry against `ctx.cwd`, in-process, with no syscall
mediation. Tools (notably `bash`) can use absolute paths,
`..`, network calls. Calling that "sandboxed" overclaims.

What M13 delivers is **blast-radius containment**: the
implementer's mutations land inside the milestone's worktree
(M12), so a `cargo fmt --all` or even a `rm -rf .` damages a
side branch we cherry-pick *one commit* out of or throw away.
Combined with M11/M12/M7/M9/M10, an off-script implementer
costs one milestone's fix-loop budget, not the operator's
working tree.

This RFD therefore drops the security-flavoured acceptance
tests (`touch ../escape.txt does not exist`, etc.). The
remaining test surface is M12-style containment of mutations
to within the worktree, which is provably true by virtue of
the dispatch CWD being the worktree path. Real OS isolation
(namespace/container/VM) is v3.

**Prerequisite matrix (lower-layer work).** Three things must
land before M13's wiring is meaningful:

| Layer | Change | Owner |
| ----- | ------ | ----- |
| `pi-agent-core` | `RuntimeConfig::sandbox_provider: Option<Arc<dyn SandboxProvider>>` plus the runtime hook that routes `Tool::invoke()` through it. None exists today. | RFD 0022 follow-up. |
| `pi-sandbox` | A `ProviderResolver` that maps a provider name (`"local-process"`) to a `SandboxProvider` constructed against the child runtime's **already-built `ToolRegistry`**, not `LocalProcessProvider::with_defaults()`. The latter would silently substitute the built-in registry (`crates/pi-sandbox/src/local.rs:35–39` — `ToolRegistry::with_extras()`) and drop runtime-specific tools, custom wrappers, and runtime extensions. Concretely: the resolver signature is `fn resolve(name: &str, registry: &ToolRegistry) -> Result<Arc<dyn SandboxProvider>, ResolveError>`, and `"local-process"` becomes `Arc::new(LocalProcessProvider::new(registry.clone()))`. ~30 LOC. | RFD 0022 follow-up. |
| Session telemetry | A `SessionEntryKind::ToolInvocation` variant (or an extension of the existing tool-call event) recording `provider`, `tool_name`, `exit_status`. v0.1 draft's `SessionEntryKind::SandboxAction` does not exist. | `pi-stats` follow-up. |

M13's orchestrator-side wiring (~170 LOC) is small; the bulk
is in the three rows above. This RFD lists them as hard
prereqs so v3 milestone ordering is unambiguous.

**Orchestrator-side wiring (the part this RFD owns).**

```rust
pub struct DispatchConfig {
    pub agent: AgentSpec,
    pub assignment: String,
    pub cwd: PathBuf,
    pub sandbox_provider_name: Option<String>,  // resolved at runtime
    pub retry: RetryPolicy,                     // carries max_attempt + io_idle (§3.5)
}
```

When `sandbox_provider_name` is `Some(name)`, dispatch passes
`--sandbox-provider <name>` to the spawned `pi -p`; the child
resolves the name through the `pi-sandbox` resolver
(prereq above) and threads it into its `RuntimeConfig`. When
it is `None`, dispatch passes **no `--sandbox-provider`
flag** and the child runs exactly as it does in v1 / M12 (no
sandbox routing at all).

The campaign TOML gains:

```toml
[defaults]
# Optional. If omitted, the orchestrator passes no
# --sandbox-provider flag and the child agent runs without
# sandbox routing (identical to v1 dispatch).
# Currently the only accepted value is "local-process".
# sandbox = "local-process"
```

**Resolved contradictions.** The v0.3 draft simultaneously
claimed (a) "default if omitted: `local-process`" in the TOML
snippet and (b) "default = no `--sandbox-provider` flag" in
the acceptance test. v0.4 picks (b): omitted means *no
implicit injection*. Operators who want sandbox routing must
say so in TOML. This avoids surprising behaviour-flip when
the lower-layer prereqs land.

Only `"local-process"` is accepted as an explicit value in
v2. The reviewer flagged accepting `"remote:my-pool"` early as
future-proofing too soon; we agree. The schema parser
**rejects** any value other than `"local-process"` until the
resolver knows about the alternative — no silent fail-open.
v3 widens the accepted set when remote providers exist.

**File / function.** `crates/pi-orchestrate/src/dispatch.rs`
(plumb-through, ~80 LOC), `crates/pi-orchestrate/src/
schema.rs` (`Defaults::sandbox: Option<String>`, ~10 LOC),
`crates/pi-orchestrate/src/sandbox_resolve.rs` (~50 LOC, a
thin wrapper around the `pi-sandbox` resolver above; refuses
unknown names).

**Child-side consumption (orchestrator-owned files).** The
spawned `pi -p` must accept and act on `--sandbox-provider
<name>`. Two child-side files need a one-line change each, and
this RFD owns both:

- `crates/pi-coding-agent/src/cli.rs` — add the
  `--sandbox-provider <name>` clap flag alongside `--session-dir`
  (added by §3.6), ~5 LOC.
- `crates/pi-coding-agent/src/startup.rs` (the runtime
  assembly site that already consumes `--session-dir` at
  lines 197–205) — call the `pi-sandbox` resolver with the
  child's already-built `ToolRegistry` and stash the resulting
  `Arc<dyn SandboxProvider>` on `RuntimeConfig::sandbox_provider`
  (the field added by the prereq matrix above), ~15 LOC.

Total child-side delta: ~20 LOC. The actual `Tool::invoke`
routing through the provider is the prereq-matrix row labelled
"`pi-agent-core` runtime hook"; it is *not* owned by this RFD,
and M13 is gated on it landing first.

**Acceptance test.**
`crates/pi-orchestrate/tests/sandbox_dispatch.rs`:

1. Default campaign (no `sandbox =` set): dispatch passes
   no `--sandbox-provider` flag (verified by intercepting the
   spawned argv); the orchestrator's behaviour is identical
   to M12. Resolves the TOML-default contradiction in §3.12.
2. `sandbox = "local-process"` and the lower-layer prereqs
   are stubbed in: dispatch passes
   `--sandbox-provider local-process`; the stub records one
   invocation per tool call.
3. Schema validation: `sandbox = "remote:foo"` is rejected at
   campaign load time (before any dispatch).
4. Mutation containment: implementer-stub runs `touch
   <cwd>/inside.txt` and `touch <repo_root>/../outside.txt`;
   assert `inside.txt` is in the worktree, `outside.txt` is
   present outside (we are honest that this is *not* blocked),
   and the diff cherry-picked into the target branch contains
   only `inside.txt`. This is the actual blast-radius
   guarantee.
5. **Custom-tool survives sandbox.** Construct a child runtime
   with a `ToolRegistry` that has the seven defaults plus one
   custom tool `my_extra` (e.g. an in-test mock). Invoke
   dispatch with `--sandbox-provider local-process`; assert
   the child can call `my_extra` from inside the sandbox. This
   pins the §3.12 contract that resolver wires the child's
   *actual* registry into the provider, not
   `with_defaults()`.

**LOC estimate.** ~310 LOC (140 source + 170 tests).
**Dependencies.** M12 (worktrees as the containment unit), M7
(watchdog wraps the dispatch), the three lower-layer prereqs
above.

## Implementation plan

| M    | Branch                                  | Scope                                                                         | LOC est. | Dogfood spend |
| ---- | --------------------------------------- | ----------------------------------------------------------------------------- | -------- | ------------- |
| §3.2 | `claude/orchestrate-event-schema`       | Typed `CampaignEvent`; v2 schema bump; v1 lift helper.                        | ~340     | $0.50         |
| M5   | `claude/orchestrate-durable-state`      | `sync_data` after every event; bench (no CI gate).                            | ~100     | $0.50         |
| M6   | `claude/orchestrate-full-resume`        | Resume matrix; idempotent cherry-pick (per-SHA `git diff-tree -p \| git patch-id --stable`); single-commit-per-milestone invariant; four new CLI flags including `--orchestrate-migrate` and `--orchestrate-reset [--milestone <id>]`; `MergeSnapshot` write-before-transition; reset/migrate detach parent repo before deleting branch refs; `SpecSnapshot` drift gate; `FixLoopAppend` emit + replay reconstruction of `accumulated_assignment`. | ~560     | $1.20         |
| M7   | `claude/orchestrate-retry`              | Retry policy + wall-clock cap + I/O-activity watchdog + `merge_retry_max` rename. | ~350     | $1.50         |
| M8a  | `claude/orchestrate-session-capture`    | Pass `--session-dir`; record session JSONL paths in events.                   | ~140     | $0.50         |
| M8b  | `claude/orchestrate-merge-report`       | `MERGE-REPORT-…md` writer; cost rollup; run-end + status only.                | ~400     | $1.50         |
| M9   | `claude/orchestrate-concerns-parser`    | Structured `## Concerns` parser + implicit-prose handling.                    | ~430     | $1.50         |
| M10  | `claude/orchestrate-forward`            | Override-rule eval + content-hash dedup header + counter discipline.          | ~430     | $1.50         |
| M11  | `claude/orchestrate-parallel`           | Scheduler infra (commit 1: internal cap = 1, no user-visible parallelism) + enable commit (commit 2: `min(env_var, 4)`, parallel acceptance tests). The §3.10 inline figure of ~570 LOC was an editing slip — the actual estimate is ~270 source + ~300 tests = ~570 *combined*, of which ~270 LOC is source proper. The implementation-plan column tracks **source LOC only** for budget purposes; this row matches §3.10's source-LOC figure. | ~270    | $1.00         |
| M12  | `claude/orchestrate-worktree`           | Prereq M12-pre: extract `crates/pi-coding-agent/src/native/worktree/{mod,git,reconcile,baseline}` into new `crates/pi-worktree`; collapse `pi-coding-agent::native::worktree` to a re-export. Prereq M12-pre-2: child-side `--project-root` plumbing across `pi-coding-agent::{cli, startup, context, native::task::tool}`, `pi-agent-core::RuntimeConfig`, and `pi_tools::ToolContext` (the `ToolContext` struct lives in `crates/pi-tools/src/lib.rs:39`, *not* in `pi-agent-core`; `pi-agent-core` consumes it via `pi-tools.workspace = true`) so `.pi/*` namespaces and nested-`task` subagent discovery resolve from `project_root` rather than the child's cwd (~190 LOC, prereq, *not* counted in M12's LOC). Then: worktree-per-milestone; `MilestoneWorktreeState` keyed by `milestone_id`; lifecycle; prune on terminal; once-per-run parent-repo detach preflight; `git status --porcelain=v1` parent-repo clean precondition emitting `AllocateError::ParentRepoDirty`; resume-time `git reset --hard <branch>` + `git clean -fdx` contract before redispatch; `AllocateError::BranchHeldElsewhere` on foreign-worktree refusal; rename `dispatch.rs::cwd` → `project_root` and add a separate `cwd`, pass `--project-root <repo_root>` on dispatch so agent definitions / extensions / skills / prompts / nested subagents all resolve from `repo_root` while the child's `current_dir` is the milestone worktree. | ~640     | $2.00         |
| M13  | `claude/orchestrate-sandbox`            | Orchestrator-side resolver + child-side `--sandbox-provider` flag consumption + TOML `sandbox = "local-process"`. (Session telemetry is a `pi-stats` follow-up per §3.12 prereq matrix, **not** owned by this milestone.) | ~330     | $1.50         |

**Total LOC: ~3 990.** **Total dogfood spend: ~$13.20** (M11 and
its enable commit are now counted as one PR; M11.1 is no longer
a separate dogfood line). The
entire campaign is dispatchable by `pi --orchestrate` itself
once §3.2, M5, M6, M7 are in (the runner is durable,
resumable, retry-safe, and crash-tolerant).

## Test plan

Each milestone above lists its acceptance tests inline; this
section captures only the campaign-level smoke and
failure-injection tests that span multiple milestones.

### End-to-end smoke (post-M13)

`crates/pi-orchestrate/tests/orchestrate_v2_smoke.rs`: a
3-milestone campaign with a forward rule, run with
`PI_ORCHESTRATE_PARALLEL=2`, `sandbox = "local-process"`,
and an injected transient failure on milestone 2's first
attempt. Assert:

- M1 and M3 run in parallel (timestamps on `Transition`
  events with `to=DISPATCHED` overlap).
- M2 retries once and succeeds (one `Retry` event).
- M1's forwarded concern lands on M3's appended assignment
  with the dedup header (one `Forward` event, content-hash
  matches).
- All three worktrees are pruned on `MERGED` (three
  `Worktree { action: "pruned" }` events).
- `MERGE-REPORT-<slug>-<id>.md` exists, parses, contains the
  forward decision row.

### Failure injection (cross-milestone)

- **state.jsonl partial-write under parallel writers.** Inject
  a panic between `write_all` and `sync_data` via a test-only
  feature flag. Assert: replay drops the truncated line,
  surviving events resume correctly.
- **Worktree dir already exists from a previous run.**
  Assert: reuse, no duplicate `git worktree add`.
- **Sandbox provider returns `Timeout` from the lower layer.**
  Assert: dispatch retries (M7); after exhaustion the
  milestone is `FAILED` with `reason=sandbox_timeout`.
- **v1→v2 migration on a real v1 log.** Take the v1
  `state.jsonl` from the in-flight pi-ai sweep; run
  `--orchestrate-migrate`; assert resume picks up at the
  correct milestone.

### Real-world dogfood

The first production v2 campaign is **RFD 0023 itself**: §3.2,
M5, M6, M7 land hand-driven; M8 onwards ships via `pi
--orchestrate <rfd-0023.toml>`. The campaign report from that
run is the validation artifact.

## Out of scope (v2)

### Deferred to v3

- **Streaming subprocess stdout to the operator.** v2's I/O
  watchdog reads byte-counts off the stdout *and* stderr pipes
  but does not surface the bytes. RFD 0017's `monitor` tool is
  the obvious composition point; deferred to keep M7's surface
  area minimal.
- **Explicit child heartbeat / structured progress channel.**
  The v2 `io_idle` watchdog is a stop-gap; long silent tool
  calls may still trip `io_idle` even when the child is healthy
  (a healthy implementer running a 12-min `cargo build` is fine
  because build output reaches stderr, but a *truly* silent
  tool — e.g. a `bash` that loops in pure CPU work writing
  nothing anywhere — will be killed at `io_idle_secs` despite
  making progress). Operators can disable `io_idle` entirely
  (`defaults.io_idle_secs = 0`) until v3, which adds a periodic
  heartbeat line on stdout so liveness becomes positive rather
  than activity-derived; only then will the wall-clock
  `max_attempt` no longer be the safety net of last resort.
- **Cross-process state lock on the same campaign-id.** Two
  `pi --orchestrate <same.toml>` invocations on different
  shells are racy in v2. The right primitive is `flock(2)` on
  `state.jsonl` opened with `O_RDWR`; the failure mode (one
  invocation exits with a clear error) is mild enough to
  defer.
- **`Ctrl-C` two-press semantics.** v2 inherits v1: `SIGINT`
  once aborts the in-flight subagent; the runner exits at the
  next eligibility check. Two-press hard-kill with `.aborted`
  session preservation deferred.
- **`BLOCKED_ON_REVIEW_STALE` auto-recovery.** v2 ships
  manual `--orchestrate-re-review`; auto-rebase or
  auto-re-review needs a reviewer-cost budget gate we don't
  have telemetry for yet.
- **Cross-repo orchestration.** Single-repo only. A campaign
  spanning `pi-rs` + `oh-my-pi` is a v3 problem.
- **Webhook notifications.** Bolt-on once `state.jsonl` is the
  durable bus (M5 makes that real).
- **Reviewer ensembling** (two reviewers, majority verdict).
- **Per-milestone `auto_approve` overrides.**
- **`PI_ORCHESTRATE_PARALLEL > 4`.** Need cargo-cache
  measurement first.
- **Real OS-level sandbox isolation** (namespace/container/
  VM/chroot-style). M13's blast-radius containment is the
  honest v2 deliverable; real isolation depends on a
  follow-up RFD with concrete mechanism (Firecracker,
  bubblewrap, gVisor, …).
- **Remote sandbox provider** (`sandbox = "remote:…"`).
  Schema currently rejects it; v3 introduces both wire shape
  and at least one provider.
- **Recursive cost rollup into `task`-tool subagent
  sessions.** RFD 0005 subagents write their own session
  JSONLs; v2 trusts only the persisted top-level
  `usage.cost_usd` and emits a footer noting the
  under-report (§3.7). v3 wires `TaskBatchResult.usage`
  propagation in `crates/pi-coding-agent/src/native/task/
  executor.rs` and recurses into descendant sessions during
  M8b's render.

### Architecturally rejected (still)

- **Workflow language with conditionals/loops.** Override-rule
  regex remains the only escape hatch.
- **Cron / schedule.** Not CI; humans start campaigns.
- **Web UI / TUI dashboard.** `--orchestrate-status` + `tail
  -f` covers v2.
- **Auto-creation of subagent definitions from prompts.**
  Users author `.pi/agents/*.md` themselves.

## Open questions

These are unresolved trade-offs the drafter is genuinely
uncertain about; each will be re-decided either by reviewer
feedback or the dogfood that hits it first.

1. **`fdatasync` cost on busy hosts.** M5's bench reports
   µs/event but does not gate on it. Operators on slow NFS or
   Docker overlay storage may see >5 ms/event and a campaign
   with thousands of `Retry`/`Forward`/`Worktree` events
   could spend a non-trivial fraction of wall time in
   fdatasync. The drafter believes this is below noise next to
   LLM I/O, but reviewer should challenge if there is a known
   slow-storage profile.

2. **Where does the v2 I/O-liveness watchdog read from when
   the inner pi runtime is multiplexed across multiple
   provider streams?** M7 reads from the subprocess's stdout
   *and* stderr handles (v0.8); in a pi run with parallel
   `task`-tool subagents the parent's stdout/stderr reflect
   only the outermost stream. That covers the wedge case we
   observed (Transport error was at the outermost stream) and
   the long-tool-call case (subprocess stderr from
   `print.rs:46` etc.), but it would miss a wedge in a deep
   `task`-tool descendant whose output is consumed by an
   intermediate runtime layer instead of being printed. v3 may
   need a cross-process heartbeat (Open question: should the
   heartbeat be a `task`-tool obligation, a runtime obligation,
   or a `pi -p` flag?).

3. **Sandbox/worktree containment overlay.** M13 admits writes
   outside the worktree (e.g. to `~/.cache/cargo/registry`)
   are not contained. The alternative is a Linux-only
   `unshare --user --mount` overlay so the worktree appears
   as `/repo` and `$HOME` is read-only. The drafter leans
   against shipping that in v2 (Linux-only, complicates macOS
   dev loops); reviewer should challenge if
   `cargo-registry-poison` is more realistic than we think.

4. **Forward dedup header line numbers vs hash-only.** M10
   includes both `concern.line_start` *and*
   `body_sha256_prefix` in the header. That is belt-and-
   braces. If reviewer wants a tighter header,
   hash-only is sufficient for correctness; the line number
   is just an operator readability aid. The drafter kept
   both.

5. **Watchdog default of 30 min.** Long enough that a slow
   xhigh implementer doesn't trip it; short enough that a
   Transport-wedged subprocess gets reaped. The 14-min hang
   is the only data point. M7 pins 50 ms in tests but the
   production default is unverified.

6. **Subagent cost recursion.** §3.7 explicitly under-reports
   cost when a milestone delegates via the `task` tool
   (RFD 0005); descendants write their own session JSONLs
   but `TaskBatchResult.usage` is zeroed in
   `crates/pi-coding-agent/src/native/task/executor.rs`. The
   alternative is to recurse into descendant sessions during
   M8b, accepting that descendant lookup is fragile (the
   parent's session does not currently record child session
   paths). The drafter chose to under-report with a footer
   rather than hand-roll a fragile recursion in v2; reviewer
   should challenge if dogfood operators care more about cost
   completeness than the RFD assumes.

## References

- **RFD 0005** — Subagents and the `task` tool
  (`rfd/0005-subagents-task-tool.md`).
- **RFD 0006** — Worktree-isolated tasks
  (`rfd/0006-worktree-isolated-tasks.md`). Reconciler at
  `crates/pi-coding-agent/src/native/worktree/{mod,git,
  reconcile,baseline}.rs` (today). M12 first extracts these
  four files into a new lower-level crate `pi-worktree` to
  break the cycle described in §3.11; existing
  `pi-coding-agent` callers continue via a `pub use`
  re-export. M12 uses `worktree_dir(repo_root, task_id)`
  (`mod.rs:57` today, `pi_worktree::worktree_dir`
  post-extraction), `ensure(repo_root, task_id)` (`mod.rs:61`
  today, `pi_worktree::ensure` post-extraction), and the
  `ConflictedBranch` outcome at `reconcile.rs:124–163`.
- **RFD 0017** — `monitor` tool
  (`rfd/0017-monitor-tool.md`). Streaming-stdout reference
  M7's watchdog explicitly defers.
- **RFD 0021** — `pi --orchestrate` v1
  (`rfd/0021-pi-orchestrate-mode.md`, Implemented).
- **RFD 0022** — Sandbox execution
  (`rfd/0022-sandbox-execution.md`).
  `crates/pi-sandbox/src/{provider.rs::SandboxProvider,
  local.rs::LocalProcessProvider}`. M13 depends on
  follow-up lower-layer hooks listed in §3.12.
- **`crates/pi-orchestrate/src/`** — current v1
  implementation; concrete cite anchors (verified against
  the live tree on this branch):
  - `runner.rs::StateEvent` (lines 56–63) — replaced by
    `CampaignEvent` in §3.2.
  - `runner.rs::emit_event` (lines 520–541) — M5.
  - `runner.rs::replay` (lines 561–620) — M6, §3.2.
  - `runner.rs::state_path_for` (line 112) — §3.1 (replaced
    by `campaign_id_for`).
  - `dispatch.rs::RealDispatch::dispatch` (lines 195–280) —
    M7, M8a, M13.
  - `merge.rs::cherry_pick_to_target` (line 73) — M6, M7.
  - `verdict.rs::parse_verdict` — M9.
- **`crates/pi-coding-agent/src/bin/pi.rs:113–167`** — v1
  orchestrate dispatch site. M6 grows this block from ~55
  LOC to ~140 LOC for the four new flags.
- **`crates/pi-coding-agent/src/cli.rs:242–256`** — full v1
  orchestrate CLI surface (`--orchestrate-dry-run`,
  `--orchestrate`, `--orchestrate-state-root`).
- **`crates/pi-coding-agent/src/cli.rs:77` +
  `startup.rs:197–205`** — child-side `--session-dir` flag
  that M8a plumbs through.
- **`crates/pi-ai/src/message.rs:117–127`** — the `Usage`
  struct whose persisted `cost_usd` field M8b sums directly
  (RFD 0010 wrote it; v2 trusts it).
- **`crates/pi-ai/src/cost.rs::compute_cost`** — canonical
  cost helper. M8b deliberately does **not** call this; it
  takes `(ModelInfo, UsageAcc)` and the session JSONL stores
  neither directly. v3 may parse session `Meta` to recompute.
- **`crates/pi-agent-core/src/session.rs:11–45`** —
  `SessionEntry` + `SessionEntryKind` schema. M8b's
  subagent-footer trigger keys on
  `SessionEntryKind::ToolCall { call: ToolCall { name:
  "task", .. } }`; there is no `SessionEntryKind::Task`
  variant (an earlier draft mistakenly cited one).
- **`crates/pi-coding-agent/src/native/worktree/git.rs:102`**
  — `worktree_try_remove` invokes `git worktree remove
  --force` only. M12 follows it with an explicit `git
  worktree prune` call through `git::run` to clear the
  administrative entry. Both names move to `pi_worktree::git`
  in the M12 prereq extraction.
- **`crates/pi-coding-agent/Cargo.toml:47`** —
  `pi-orchestrate.workspace = true`. This is the dependency
  edge that forces M12's prereq refactor: with this edge in
  place, `pi-orchestrate` cannot import `pi-coding-agent`
  internals without creating a cycle.
- **Linux `fdatasync(2)`** — `man 2 fdatasync`. Skips
  `atime`/`mtime`-only metadata syncs but does flush size and
  the data needed for subsequent reads.
- **`git patch-id(1)`** — `man git-patch-id`. M6 pins the
  `--stable` flag. Per the manpage, `--stable` makes the
  computed id stable across **reordered file diffs** within
  one patch (e.g. `git format-patch` ordering changes); it
  does **not** promise stability across hunk-line reordering
  inside a single file. The §3.4 acceptance test exercises
  the file-diff guarantee specifically.
- **`tokio::sync::Semaphore`** —
  https://docs.rs/tokio/1/tokio/sync/struct.Semaphore.html
  (M11).
- **`tokio::sync::Mutex`** —
  https://docs.rs/tokio/1/tokio/sync/struct.Mutex.html
  (M11 merge queue + state.jsonl writer).
