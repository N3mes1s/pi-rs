# RFD 0006 — Worktree-isolated tasks

- **Status:** Implemented
- **Author:** pi-rs maintainers
- **Created:** 2026-04-27
- **Implemented:** f877620

## Summary

Run a `task` subagent (RFD 0005) inside a dedicated `git worktree`
allocated under `~/.pi/wt/data/<encoded-repo>/<task-id>/` so its file
mutations land in an isolated working copy, then fold the result back
into the parent branch as either (a) a cherry-picked commit on a
`pi/task/<id>` branch or (b) a unified `.patch` artifact. The parent's
working tree is **never** touched while the subagent runs. A new
top-level `pi --worktree` flag exposes the same isolation to anyone
calling `pi --print` from a CI or evolution-daemon driver.

## Background

Subagents that mutate files (RFD 0005's `code-reviewer` is read-only;
`refactor-and-fix` is not) need a sandbox. Without one, two parallel
subagents would race for the same `src/lib.rs`. With one, you get the
oh-my-pi setup: `task/worktree.ts` allocates a worktree per task,
captures a baseline, runs the subagent there, then either
cherry-picks or emits a patch.

Pi-rs already shells out to `git` from
`crates/pi-coding-agent/src/footer.rs` for the status-line. We extend
that pattern rather than pulling in libgit2. References:

* Git docs: [`git worktree`](https://git-scm.com/docs/git-worktree).
* Oh-my-pi: `packages/coding-agent/src/task/worktree.ts` (the source
  of truth for the patch/branch reconciliation logic we copy).
* Discussion of `--detach` semantics: [libgit2 #6720](https://github.com/libgit2/libgit2/discussions/6720).

## Proposal

### Crate layout

```
crates/pi-coding-agent/src/native/worktree/
├── mod.rs              # public API: ensure, capture_baseline, finish, cleanup
├── git.rs              # tiny wrappers around `git` invocations
├── baseline.rs         # WorktreeBaseline + apply_baseline
├── reconcile.rs        # commit_to_branch, write_patch
└── tests/
    ├── ensure_lifecycle.rs
    ├── baseline_round_trip.rs
    └── reconcile_branch_vs_patch.rs
```

The module hangs off `pi_coding_agent::native::worktree` so it can be
called from `native::task::executor` (RFD 0005). No new crate; this is
a small enough surface that a sibling module is right.

### Disk layout

```
~/.pi/wt/
└── data/
    └── --home-user-pi-rs--/             ← encoded repo path
        ├── 1f3c...task-id/              ← worktree #1
        │   ├── .git                     ← gitfile pointing back
        │   ├── src/...
        │   └── ...
        └── 8ab2...task-id/              ← worktree #2
```

Path helpers:

```rust
// mod.rs
fn worktrees_root() -> PathBuf {
    pi_coding_agent::context::agent_dir().join("wt").join("data")
}

fn encode_repo(repo_root: &Path) -> String {
    // /home/user/pi-rs → "--home-user-pi-rs--"
    let s = repo_root.to_string_lossy().replace('/', "-").replace(':', "-");
    format!("--{}--", s.trim_matches('-'))
}

pub fn worktree_dir(repo_root: &Path, task_id: &str) -> PathBuf {
    worktrees_root().join(encode_repo(repo_root)).join(task_id)
}
```

### Lifecycle

```rust
// mod.rs — top-level API
pub async fn ensure(repo_root: &Path, task_id: &str) -> Result<PathBuf, WorktreeError> {
    let dir = worktree_dir(repo_root, task_id);
    tokio::fs::create_dir_all(dir.parent().unwrap()).await?;
    git::worktree_try_remove(repo_root, &dir).await.ok();   // best-effort
    if dir.exists() {
        tokio::fs::remove_dir_all(&dir).await?;
    }
    git::worktree_add_detached(repo_root, &dir, "HEAD").await?;
    Ok(dir)
}

pub async fn cleanup(dir: &Path) {
    if let Ok(repo) = git::repo_root(dir).await {
        git::worktree_try_remove(&repo, dir).await.ok();
    }
    let _ = tokio::fs::remove_dir_all(dir).await;
}
```

The exact `git` invocation:

```rust
// git.rs
pub async fn worktree_add_detached(repo_root: &Path, path: &Path, refspec: &str)
    -> Result<(), GitError>
{
    let out = tokio::process::Command::new("git")
        .arg("-C").arg(repo_root)
        .args(["worktree", "add", "--detach", "--no-checkout"])
        .arg(path).arg(refspec)
        .output().await?;
    if !out.status.success() {
        return Err(GitError::cmd_failed("worktree add", &out));
    }
    // Second pass: explicit checkout so we can pin index state.
    let out = tokio::process::Command::new("git")
        .arg("-C").arg(path)
        .args(["checkout", "--detach", refspec])
        .output().await?;
    out.status.success().then_some(()).ok_or_else(|| GitError::cmd_failed("checkout", &out))
}
```

`--detach` keeps us off any branch (no name collision risk between
parallel tasks); `--no-checkout` followed by an explicit checkout
gives us a clean two-step we can fail closed on if step two errors.

### Baseline capture

We snapshot the parent repo's working state **before** the subagent
starts so we can reconstruct it inside the worktree (so the subagent
sees the user's WIP, not just `HEAD`).

```rust
// baseline.rs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoBaseline {
    pub repo_root:   PathBuf,
    pub head_sha:    String,        // empty string == detached/orphan
    pub staged:      Vec<u8>,       // `git diff --cached --binary`
    pub unstaged:    Vec<u8>,       // `git diff --binary`
    pub untracked:   Vec<UntrackedFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UntrackedFile {
    pub rel_path: PathBuf,
    pub content:  Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeBaseline {
    pub root:   RepoBaseline,
    /// Nested non-submodule git repos discovered under `root`. Each
    /// gets the same baseline applied independently.
    pub nested: Vec<(PathBuf, RepoBaseline)>,
}

pub async fn capture(repo_root: &Path) -> Result<WorktreeBaseline, BaselineError> { /* ... */ }
pub async fn apply(target_worktree: &Path, baseline: &WorktreeBaseline)
    -> Result<(), BaselineError> { /* ... */ }
```

`apply` is symmetric to `capture`:

1. `git apply --binary --cached <staged>` then commit-less `git apply
   --binary <unstaged>` to recreate the parent's index/working tree.
2. Write each `UntrackedFile` to its `rel_path`.
3. Recurse into `nested[]`.

Failure semantics: a partial apply leaves the worktree dirty; we
`git reset --hard HEAD` then bubble the error back to the executor,
which fails the task without ever running it.

We **skip** binary blob ingest if any single untracked file is
`> WORKTREE_MAX_UNTRACKED_BYTES` (default 16 MiB), with a warning,
to avoid eating disk on stray Cargo target directories or LFS blobs.

### Subagent execution

`crates/pi-coding-agent/src/native/task/executor.rs` (RFD 0005) gains
two lines:

```rust
// run_one() — additive when isolated == true
let cwd = if isolated {
    let dir = worktree::ensure(&repo_root, &task.id).await?;
    let baseline = worktree::capture(&repo_root).await?;
    worktree::apply(&dir, &baseline).await?;
    dir
} else {
    parent_cfg.cwd.clone()
};
let child_cfg = RuntimeConfig { cwd, ..child_cfg };
```

After the subagent's `prompt()` returns, we reconcile.

### Reconciliation

Two modes:

```rust
#[derive(Debug, Clone, Copy)]
pub enum ReconcileMode {
    /// Cherry-pick subagent's net diff onto the parent branch as a
    /// single commit on `pi/task/<task-id>`. Default.
    Branch,
    /// Just emit a `.patch` file under
    /// `~/.pi/wt/patches/<task-id>.patch` and leave the parent alone.
    Patch,
}

pub async fn finish(
    repo_root: &Path,
    worktree:  &Path,
    baseline:  &WorktreeBaseline,
    task_id:   &str,
    mode:      ReconcileMode,
) -> Result<ReconcileOutcome, ReconcileError>;
```

#### `ReconcileMode::Branch` (default)

```text
1. Inside the worktree:
   git add -A
   git commit -m "pi/task/{id}: {description}" --allow-empty
2. Capture the resulting commit sha (= TASK_SHA).
3. In repo_root:
   git branch pi/task/{id} TASK_SHA          # name the orphan commit
4. Best-effort cherry-pick if the parent's HEAD is unchanged from
   baseline.head_sha:
       git cherry-pick TASK_SHA
   On conflict: abort (`git cherry-pick --abort`), leave the branch
   intact for manual `git merge` / `git cherry-pick` later, return
   `ReconcileOutcome::ConflictedBranch`.
5. Cleanup the worktree.
```

#### `ReconcileMode::Patch`

```text
1. Compute delta:    git -C worktree diff --binary HEAD~  > /tmp/_patch
2. Save under       ~/.pi/wt/patches/{task-id}.patch
3. Cleanup worktree.
```

```rust
#[derive(Debug, Clone, Serialize)]
pub enum ReconcileOutcome {
    Branch  { branch: String, sha: String, merged: bool },
    Patch   { path: PathBuf },
    Empty,                                  // subagent made no changes
    ConflictedBranch { branch: String },    // human resolves later
}
```

### CLI

`pi --worktree` toggles isolation for any non-interactive run:

```rust
// cli.rs additions
/// Run this invocation inside a private git worktree. The parent
/// branch is not touched; on success, changes land on
/// `pi/task/<random-id>` (or as a patch artifact when
/// `--worktree-mode=patch`).
#[arg(long, action = ArgAction::SetTrue)]
pub worktree: bool,

#[arg(long, value_name = "MODE", value_parser = ["branch", "patch"])]
pub worktree_mode: Option<String>,        // default "branch"

#[arg(long, value_name = "ID")]
pub worktree_id: Option<String>,           // default = uuid::Uuid::new_v4()
```

Top-level (`crates/pi-coding-agent/src/main.rs`) wraps the agent run:

```rust
if cli.worktree {
    let id  = cli.worktree_id.clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let dir = worktree::ensure(&repo_root, &id).await?;
    let baseline = worktree::capture(&repo_root).await?;
    worktree::apply(&dir, &baseline).await?;
    // tweak cwd that the agent runs under:
    runtime_config.cwd = dir.clone();
    let outcome = run_agent(runtime_config).await;
    let mode = match cli.worktree_mode.as_deref() {
        Some("patch") => ReconcileMode::Patch,
        _             => ReconcileMode::Branch,
    };
    let rec = worktree::finish(&repo_root, &dir, &baseline, &id, mode).await?;
    println!("{}", serde_json::to_string(&rec)?);
    return Ok(outcome);
}
```

Same machinery powers RFD 0005's `task: { isolated: true }`.

### Concurrency / locking

Worktree paths are unique per `(repo_root, task_id)`. `git worktree
add` itself takes a `.git/worktrees.lock` and serialises within the
parent repo. So launching N concurrent isolated tasks against the
same repo is safe: each gets its own subdir, `git` itself prevents
double-allocation.

We additionally write `~/.pi/wt/data/<encoded-repo>/<task-id>/.lock`
on `ensure`, remove on `cleanup`. This is purely a hint for human
operators / cleanup tooling; no OS-level fcntl required.

### OS-specific overlays — explicitly out of scope for v1

Oh-my-pi optionally swaps the worktree backend for a `fuse-overlayfs`
mount on Linux or a ProjFS overlay on Windows. The performance win is
real (avoids `git checkout`'s file copy cost) but the failure modes
are gnarly: needing root/CAP_SYS_ADMIN on Linux, native DLL on
Windows, broken on macOS. **v1 ships worktree mode only.** A
follow-up RFD (0017) can add an opt-in overlay backend if profiling
ever shows `git worktree add` itself as a bottleneck on big repos.

### Cleanup of stale worktrees

A startup pass removes worktrees older than `WORKTREE_MAX_AGE_HOURS`
(default 24) that are no longer registered:

```rust
pub async fn gc(now: SystemTime) -> Result<usize, WorktreeError> {
    let mut removed = 0;
    for entry in walkdir::WalkDir::new(worktrees_root()).max_depth(2) {
        let entry = entry?;
        if entry.file_type().is_dir()
           && entry.depth() == 2
           && older_than(&entry, now, MAX_AGE) {
            cleanup(entry.path()).await;
            removed += 1;
        }
    }
    Ok(removed)
}
```

Call site: `pi --worktree` and the executor's `run_one` both call
`gc` at the start (debounced by a `OnceCell<Instant>`).

## Test plan

1. **`tests/ensure_lifecycle.rs`** — init a tempdir git repo with one
   committed file; call `ensure`; assert `.git` exists in the worktree
   path, `git -C <worktree> status` returns clean; call `cleanup`;
   assert the worktree is gone.
2. **`tests/baseline_round_trip.rs`** — set up a repo with one
   committed file, one staged change, one unstaged change, one
   untracked file. `capture` then `apply` to a fresh worktree; assert
   `git status` outputs match byte-for-byte.
3. **`tests/reconcile_branch_vs_patch.rs`** — drive a fake "subagent"
   that just writes a new file into the worktree; finish in
   `Branch` mode; assert `pi/task/<id>` exists and points at a commit
   touching exactly that file. Repeat in `Patch` mode; assert
   `~/.pi/wt/patches/<id>.patch` is a valid `git apply --check` input.
4. **`tests/conflict_keeps_branch.rs`** — set up a repo where the
   parent's HEAD has moved past `baseline.head_sha` and the moved
   commit touches the same file the subagent did. Run finish; assert
   `ReconcileOutcome::ConflictedBranch` and the branch still exists
   for manual resolution.
5. **`tests/empty_diff_is_empty.rs`** — subagent that touches no
   files; assert `ReconcileOutcome::Empty` and no branch is created.
6. **`tests/parallel_two_tasks.rs`** — `tokio::join!` two `ensure`
   calls with different ids; assert both succeed and produce
   different worktree paths.
7. **`tests/gc_removes_old.rs`** — `mtime` an existing worktree dir
   to two days ago; call `gc(now)`; assert the dir is gone.

All tests skip when `git` is not on `PATH` (mirrors
`tests/lsp_real_rust_analyzer.rs`'s skip-on-missing pattern).

## Out of scope

- **Submodule fidelity.** v1 captures non-submodule nested repos.
  Submodules' `.gitmodules` content is preserved, but submodule HEADs
  inside the worktree are pinned to whatever `git worktree add` chose
  (typically the recorded commit). RFD 0018 would add explicit
  submodule baseline + reconcile.
- **LFS.** v1 doesn't `lfs pull` after `git worktree add`. If the
  parent has unfetched LFS blobs, they'll appear as pointer files in
  the worktree. For most agent tasks (text editing) that's fine.
- **Pre-commit hooks running inside the worktree** (oh-my-pi's
  comment-only "hooks may misfire" caveat). v1 disables hooks during
  `git commit` inside the worktree (`-c core.hooksPath=/dev/null`).
- **macOS APFS clone / Linux FICLONE.** Out of scope; see overlay
  RFD note above.

## Open questions

- **Should `ensure` re-use an existing clean worktree dir to skip the
  `git checkout` cost when the task id is supplied explicitly?**
  Lean no — explicit ids are mainly for the evolve daemon, which
  expects an empty start. Add an `--reuse` opt-in flag if anyone
  needs it.
- **`ReconcileMode::Branch`'s default cherry-pick — should it
  *really* attempt the merge automatically, or always leave it to the
  user?** Lean attempt-then-fall-back: if the cherry-pick is clean
  (the common case during sequential subagent fan-out), the result is
  on the parent's branch immediately. If conflicted, we fall back to
  the named branch for manual resolution.
- **`pi --worktree` printing the reconcile JSON — should we also
  attach it to the agent's final assistant message?** Probably yes;
  the calling driver (CI / evolve daemon) shouldn't have to parse the
  trailing line. Wire as a follow-up.
