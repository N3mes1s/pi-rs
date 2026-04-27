mod worktree_common;
use worktree_common::*;

use pi_coding_agent::native::worktree as wt;

#[tokio::test]
async fn parent_head_advanced_means_conflicted_branch() {
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

    // Create file shared between parent advance and subagent.
    std::fs::write(repo.join("conflict.txt"), "v0\n").unwrap();
    git(&repo, &["add", "conflict.txt"]);
    git(&repo, &["commit", "-q", "-m", "v0", "--no-verify"]);

    let dir = wt::ensure(&repo, "task-conf").await.unwrap();
    let baseline = wt::capture_baseline(&repo).await.unwrap();
    wt::apply_baseline(&dir, &baseline).await.unwrap();

    // Subagent: rewrite conflict.txt inside the worktree.
    std::fs::write(dir.join("conflict.txt"), "subagent-version\n").unwrap();

    // Parent HEAD advances independently — and touches the same file
    // so even if cherry-pick *were* attempted it would conflict.
    std::fs::write(repo.join("conflict.txt"), "parent-version\n").unwrap();
    git(&repo, &["add", "conflict.txt"]);
    git(&repo, &["commit", "-q", "-m", "parent advances", "--no-verify"]);

    let outcome = wt::finish(&repo, &dir, &baseline, "task-conf", wt::ReconcileMode::Branch)
        .await
        .unwrap();

    match outcome {
        wt::ReconcileOutcome::ConflictedBranch { branch } => {
            assert_eq!(branch, "pi/task/task-conf");
            // Branch still resolves.
            let out = git(&repo, &["rev-parse", "--verify", &branch]);
            assert!(out.status.success());
        }
        other => panic!("expected ConflictedBranch, got {other:?}"),
    }

    // Parent HEAD's file is still parent-version (unmutated).
    let s = std::fs::read_to_string(repo.join("conflict.txt")).unwrap();
    assert_eq!(s, "parent-version\n");

    wt::cleanup(&dir).await;
}
