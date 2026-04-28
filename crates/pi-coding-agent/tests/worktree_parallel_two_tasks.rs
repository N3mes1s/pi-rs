mod worktree_common;
use worktree_common::*;

use pi_coding_agent::native::worktree as wt;

#[tokio::test]
async fn parallel_ensure_two_distinct_paths() {
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

    let (a, b) = tokio::join!(wt::ensure(&repo, "task-a"), wt::ensure(&repo, "task-b"),);
    let a = a.expect("task-a ensure");
    let b = b.expect("task-b ensure");

    assert_ne!(a, b);
    assert!(a.exists() && b.exists());
    assert!(a.join(".git").exists());
    assert!(b.join(".git").exists());

    wt::cleanup(&a).await;
    wt::cleanup(&b).await;
}
