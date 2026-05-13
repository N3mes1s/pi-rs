mod worktree_common;
use worktree_common::*;

use pi_coding_agent::native::worktree as wt;

#[tokio::test]
async fn branch_mode_creates_branch_and_cherry_picks() {
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

    let dir = wt::ensure(&repo, "task-br").await.unwrap();
    let baseline = wt::capture_baseline(&repo).await.unwrap();
    wt::apply_baseline(&dir, &baseline).await.unwrap();

    // "Subagent": write a new file inside the worktree.
    std::fs::write(dir.join("hello.txt"), "hello from subagent\n").unwrap();

    let outcome = wt::finish(&repo, &dir, &baseline, "task-br", wt::ReconcileMode::Branch)
        .await
        .unwrap();

    match outcome {
        wt::ReconcileOutcome::Branch {
            branch,
            sha,
            merged,
        } => {
            assert_eq!(branch, "pi/task/task-br");
            assert!(!sha.is_empty());
            assert!(merged, "clean parent ⇒ cherry-pick should succeed");

            // Branch exists.
            let out = git(&repo, &["rev-parse", "--verify", "pi/task/task-br"]);
            let listed = String::from_utf8(out.stdout).unwrap();
            assert_eq!(listed.trim(), sha);

            // The commit's tree contains hello.txt.
            let show = git(&repo, &["show", "--name-only", "--pretty=", &sha]).stdout;
            let names = String::from_utf8(show).unwrap();
            assert!(
                names.contains("hello.txt"),
                "commit touches hello.txt: {names}"
            );

            // Cherry-pick landed on parent HEAD.
            assert!(
                repo.join("hello.txt").exists(),
                "parent worktree has the file post-cherry-pick"
            );
        }
        other => panic!("expected Branch, got {other:?}"),
    }

    wt::cleanup(&dir).await;
}

#[tokio::test]
async fn patch_mode_writes_apply_check_clean_patch() {
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

    let dir = wt::ensure(&repo, "task-p").await.unwrap();
    let baseline = wt::capture_baseline(&repo).await.unwrap();
    wt::apply_baseline(&dir, &baseline).await.unwrap();
    std::fs::write(dir.join("hello.txt"), "patched\n").unwrap();

    let outcome = wt::finish(&repo, &dir, &baseline, "task-p", wt::ReconcileMode::Patch)
        .await
        .unwrap();

    match outcome {
        wt::ReconcileOutcome::Patch { path } => {
            assert!(path.exists(), "patch artifact written");
            assert!(path.starts_with(wt::patches_root()));
            // Parent must be untouched.
            assert!(!repo.join("hello.txt").exists());
            // `git apply --check` against parent succeeds.
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["apply", "--check", "--binary"])
                .arg(&path)
                .output()
                .expect("git apply --check");
            assert!(
                out.status.success(),
                "git apply --check failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        other => panic!("expected Patch, got {other:?}"),
    }

    wt::cleanup(&dir).await;
}
