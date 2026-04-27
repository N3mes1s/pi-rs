mod worktree_common;
use worktree_common::*;

use pi_coding_agent::native::worktree as wt;

#[tokio::test]
async fn ensure_then_cleanup_round_trip() {
    if !require_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    isolate_wt_root(tmp.path());
    let repo = init_seed_repo(&tmp.path().join("repo").tap_create());

    let dir = wt::ensure(&repo, "task-abc").await.expect("ensure");
    assert!(dir.exists(), "worktree dir created");
    assert!(dir.join(".git").exists(), ".git inside worktree");

    let status = git(&dir, &["status", "--porcelain"]);
    assert!(
        status.stdout.is_empty(),
        "fresh worktree should be clean, got: {}",
        String::from_utf8_lossy(&status.stdout)
    );

    wt::cleanup(&dir).await;
    assert!(!dir.exists(), "worktree gone after cleanup");
}

trait TapCreate {
    fn tap_create(self) -> Self;
}
impl TapCreate for std::path::PathBuf {
    fn tap_create(self) -> Self {
        std::fs::create_dir_all(&self).unwrap();
        self
    }
}
