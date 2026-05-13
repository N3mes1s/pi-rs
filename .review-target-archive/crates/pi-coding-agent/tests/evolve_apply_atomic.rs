//! RFD 0013 — `evolve::apply::commit` atomic-swap test.

use pi_coding_agent::evolve::apply::{commit, decide, read_history};

#[test]
fn commit_swaps_agents_md_and_appends_history() {
    let tmp = tempfile::tempdir().unwrap();
    let agents = tmp.path().join("AGENTS.md");
    let history = tmp.path().join("history.jsonl");
    std::fs::write(&agents, "# old body\n").unwrap();

    let dec = decide(&[0.5, 0.6], &[0.7, 0.8], 0.10);
    assert!(dec.apply);

    let entry = commit(&agents, "# new body\n", &history, &dec).expect("commit ok");
    assert_eq!(entry.action, "apply");
    assert_eq!(
        std::fs::read_to_string(&agents).unwrap(),
        "# new body\n",
        "AGENTS.md must be swapped"
    );

    let rows = read_history(&history);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].action, "apply");
    assert_eq!(rows[0].prev_body.as_deref(), Some("# old body\n"));
    assert!(rows[0].pre_mean.unwrap() > 0.0);
}

#[test]
fn partial_write_via_unwritable_history_leaves_agents_md_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let agents = tmp.path().join("AGENTS.md");
    std::fs::write(&agents, "ORIGINAL\n").unwrap();

    // Force history.jsonl to be a directory so opening for append fails.
    let history = tmp.path().join("history.jsonl");
    std::fs::create_dir_all(&history).unwrap();

    let dec = decide(&[0.5], &[0.9], 0.10);
    let err = commit(&agents, "REPLACEMENT\n", &history, &dec).expect_err("must fail");
    drop(err);

    assert_eq!(
        std::fs::read_to_string(&agents).unwrap(),
        "ORIGINAL\n",
        "history-write failure must not have swapped AGENTS.md"
    );
    // No leftover tempfile next to AGENTS.md.
    let leftovers: Vec<_> = std::fs::read_dir(tmp.path())
        .unwrap()
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.starts_with(".AGENTS.md.evolve.tmp.") {
                Some(n)
            } else {
                None
            }
        })
        .collect();
    assert!(leftovers.is_empty(), "tempfile not cleaned: {leftovers:?}");
}

#[test]
fn commit_handles_missing_agents_md() {
    let tmp = tempfile::tempdir().unwrap();
    let agents = tmp.path().join("AGENTS.md");
    let history = tmp.path().join("history.jsonl");
    let dec = decide(&[0.0], &[0.5], 0.10);
    let entry = commit(&agents, "FRESH\n", &history, &dec).unwrap();
    assert_eq!(std::fs::read_to_string(&agents).unwrap(), "FRESH\n");
    assert_eq!(entry.prev_body.as_deref(), Some(""));
}
