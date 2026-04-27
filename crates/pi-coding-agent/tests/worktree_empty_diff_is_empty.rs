mod worktree_common;
use worktree_common::*;

use pi_coding_agent::native::worktree as wt;

#[tokio::test]
async fn no_changes_yields_empty_outcome() {
    if !require_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    isolate_wt_root(tmp.path());
    let repo = init_seed_repo(&{
        let p = tmp.path().join("repo");
        std::fs::create_dir_all(&p).unwrap();
        p
    });

    let dir = wt::ensure(&repo, "task-empty").await.unwrap();
    let baseline = wt::capture_baseline(&repo).await.unwrap();
    wt::apply_baseline(&dir, &baseline).await.unwrap();

    // No mutations.

    let outcome = wt::finish(&repo, &dir, &baseline, "task-empty", wt::ReconcileMode::Branch)
        .await
        .unwrap();

    assert!(matches!(outcome, wt::ReconcileOutcome::Empty));

    // No branch was created.
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["rev-parse", "--verify", "pi/task/task-empty"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "no branch should exist for empty diff");

    wt::cleanup(&dir).await;
}
