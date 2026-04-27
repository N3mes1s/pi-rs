mod worktree_common;
use worktree_common::*;

use pi_coding_agent::native::worktree as wt;

#[tokio::test]
async fn baseline_round_trip_replays_wip_into_fresh_worktree() {
    if !require_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    isolate_wt_root(tmp.path());
    let repo_dir = tmp.path().join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    let repo = init_seed_repo(&repo_dir);

    // Add a tracked file and commit so we have something to modify.
    std::fs::write(repo.join("a.txt"), "alpha\n").unwrap();
    git(&repo, &["add", "a.txt"]);
    git(&repo, &["commit", "-q", "-m", "add a", "--no-verify"]);

    // Staged change: modify a.txt and `git add` it.
    std::fs::write(repo.join("a.txt"), "alpha-staged\n").unwrap();
    git(&repo, &["add", "a.txt"]);

    // Unstaged change: a brand new file's add (we'll create another
    // tracked file b.txt, commit, then mutate without `git add`).
    std::fs::write(repo.join("b.txt"), "bravo\n").unwrap();
    git(&repo, &["add", "b.txt"]);
    git(&repo, &["commit", "-q", "-m", "add b", "--no-verify"]);
    std::fs::write(repo.join("b.txt"), "bravo-modified\n").unwrap();

    // Untracked file.
    std::fs::write(repo.join("u.txt"), "untracked\n").unwrap();

    let baseline = wt::capture_baseline(&repo).await.expect("capture");

    // Spin up an isolated worktree and apply.
    let dir = wt::ensure(&repo, "task-rt").await.expect("ensure");
    wt::apply_baseline(&dir, &baseline).await.expect("apply");

    // Now `git status --porcelain` in both should yield the same set
    // of lines (order-independent).
    let parent_status = git(&repo, &["status", "--porcelain"]).stdout;
    let wt_status = git(&dir, &["status", "--porcelain"]).stdout;
    let mut p: Vec<_> = parent_status.split(|b| *b == b'\n').collect();
    let mut w: Vec<_> = wt_status.split(|b| *b == b'\n').collect();
    p.sort();
    w.sort();
    assert_eq!(p, w, "porcelain status differs after baseline replay");

    // Untracked content survives byte-exactly.
    let copied = std::fs::read(dir.join("u.txt")).unwrap();
    assert_eq!(copied, b"untracked\n");

    wt::cleanup(&dir).await;
}
