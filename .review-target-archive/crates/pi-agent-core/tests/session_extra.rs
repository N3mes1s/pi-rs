//! Extra coverage for SessionManager — listing, peeking, opening an
//! existing session, in-memory mode, and the helpers on `SessionTree`.

use pi_agent_core::{SessionEntry, SessionEntryKind, SessionManager, SessionTree};
use pi_ai::{Message, ToolResult};

#[test]
fn in_memory_session_manager_creates_session_without_persisting() {
    let mgr = SessionManager::in_memory();
    let meta = mgr.create("anthropic", "sonnet").unwrap();
    assert!(!meta.id.is_empty());
    assert_eq!(meta.provider, "anthropic");
    assert_eq!(meta.model, "sonnet");
    // No on-disk path.
    assert!(meta.path.as_os_str().is_empty());

    // Most recent of an in-memory manager is None — it's not on disk.
    assert!(mgr.most_recent().is_none());
}

#[test]
fn current_branch_walks_from_tip_back_to_root() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let mgr = SessionManager::on_disk(dir.path().to_path_buf(), cwd.path().to_path_buf()).unwrap();
    let meta = mgr.create("anthropic", "sonnet").unwrap();

    let u = mgr
        .append(
            &meta.id,
            SessionEntryKind::User {
                message: Message::user_text("hi"),
            },
        )
        .unwrap();
    let a = mgr
        .append(
            &meta.id,
            SessionEntryKind::Assistant {
                message: Message::assistant_text("hello"),
            },
        )
        .unwrap();
    let branch = mgr.current_branch(&meta.id);
    let ids: Vec<&str> = branch.iter().map(|e| e.id.as_str()).collect();
    // `Meta` entry is created up-front, then `u`, then `a`.
    assert!(ids.contains(&u.id.as_str()));
    assert!(ids.contains(&a.id.as_str()));
    // The tip (last appended) must come last in the branch.
    assert_eq!(branch.last().unwrap().id, a.id);
}

#[test]
fn current_branch_for_unknown_session_id_returns_empty() {
    let mgr = SessionManager::in_memory();
    assert!(mgr.current_branch("does-not-exist").is_empty());
}

#[test]
fn meta_returns_none_for_unknown_session() {
    let mgr = SessionManager::in_memory();
    assert!(mgr.meta("nope").is_none());
    assert!(mgr.tree("nope").is_none());
}

#[test]
fn list_orders_sessions_by_updated_at_descending() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let mgr = SessionManager::on_disk(dir.path().to_path_buf(), cwd.path().to_path_buf()).unwrap();
    let m1 = mgr.create("anthropic", "sonnet").unwrap();
    // Force a later updated_at on the second session.
    std::thread::sleep(std::time::Duration::from_millis(5));
    let m2 = mgr.create("openai", "gpt-4").unwrap();

    let listed = mgr.list();
    let ids: Vec<&str> = listed.iter().map(|m| m.id.as_str()).collect();
    // Most recent should appear first.
    let pos1 = ids.iter().position(|i| *i == m1.id).unwrap();
    let pos2 = ids.iter().position(|i| *i == m2.id).unwrap();
    assert!(pos2 < pos1, "m2 should sort before m1: {ids:?}");
}

#[test]
fn most_recent_returns_the_top_of_list() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let mgr = SessionManager::on_disk(dir.path().to_path_buf(), cwd.path().to_path_buf()).unwrap();
    let _m1 = mgr.create("anthropic", "sonnet").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    let m2 = mgr.create("openai", "gpt-4").unwrap();
    let recent = mgr.most_recent().unwrap();
    assert_eq!(recent.id, m2.id);
}

#[test]
fn open_existing_reads_jsonl_back_into_a_meta() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let mgr = SessionManager::on_disk(dir.path().to_path_buf(), cwd.path().to_path_buf()).unwrap();
    let meta = mgr.create("anthropic", "sonnet").unwrap();
    mgr.append(
        &meta.id,
        SessionEntryKind::User {
            message: Message::user_text("hi"),
        },
    )
    .unwrap();
    mgr.append(
        &meta.id,
        SessionEntryKind::ToolResult {
            result: ToolResult {
                tool_use_id: "x".into(),
                model_output: "out".into(),
                display: None,
                is_error: false,
            },
        },
    )
    .unwrap();

    // Re-open the same session by id from a fresh manager.
    let mgr2 = SessionManager::on_disk(dir.path().to_path_buf(), cwd.path().to_path_buf()).unwrap();
    let meta2 = mgr2.open_existing(&meta.id).unwrap();
    assert_eq!(meta2.id, meta.id);
    assert_eq!(meta2.provider, "anthropic");
    assert_eq!(meta2.model, "sonnet");

    let branch = mgr2.current_branch(&meta.id);
    assert!(!branch.is_empty(), "loaded branch should have entries");
}

#[test]
fn open_existing_via_explicit_jsonl_path() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let mgr = SessionManager::on_disk(dir.path().to_path_buf(), cwd.path().to_path_buf()).unwrap();
    let meta = mgr.create("anthropic", "sonnet").unwrap();
    mgr.append(
        &meta.id,
        SessionEntryKind::User {
            message: Message::user_text("hi"),
        },
    )
    .unwrap();

    let path = meta.path.clone();
    let mgr2 = SessionManager::on_disk(dir.path().to_path_buf(), cwd.path().to_path_buf()).unwrap();
    let meta2 = mgr2.open_existing(&path.display().to_string()).unwrap();
    assert_eq!(meta2.id, meta.id);
}

#[test]
fn open_existing_in_memory_manager_returns_not_found() {
    let mgr = SessionManager::in_memory();
    let r = mgr.open_existing("anything");
    assert!(r.is_err());
}

#[test]
fn append_to_unopened_session_returns_error() {
    let mgr = SessionManager::in_memory();
    let r = mgr.append(
        "no-such-session",
        SessionEntryKind::User {
            message: Message::user_text("x"),
        },
    );
    assert!(r.is_err());
}

#[test]
fn fork_to_unknown_entry_returns_not_found() {
    let mgr = SessionManager::in_memory();
    let meta = mgr.create("anthropic", "sonnet").unwrap();
    let r = mgr.fork(&meta.id, "no-entry-with-this-id");
    assert!(r.is_err());
}

#[test]
fn cwd_accessor_returns_the_managers_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let mgr = SessionManager::on_disk(dir.path().to_path_buf(), cwd.path().to_path_buf()).unwrap();
    assert_eq!(mgr.cwd(), cwd.path());
}

#[test]
fn session_tree_branch_walks_from_tip_to_root() {
    let now = chrono::Utc::now().timestamp_millis();
    let entries = vec![
        SessionEntry {
            id: "root".into(),
            parent_id: None,
            timestamp: now,
            kind: SessionEntryKind::Meta {
                cwd: ".".into(),
                provider: "p".into(),
                model: "m".into(),
                title: None,
            },
        },
        SessionEntry {
            id: "a".into(),
            parent_id: Some("root".into()),
            timestamp: now + 1,
            kind: SessionEntryKind::User {
                message: Message::user_text("hi"),
            },
        },
        SessionEntry {
            id: "b".into(),
            parent_id: Some("a".into()),
            timestamp: now + 2,
            kind: SessionEntryKind::Assistant {
                message: Message::assistant_text("hello"),
            },
        },
        SessionEntry {
            id: "fork".into(),
            parent_id: Some("a".into()),
            timestamp: now + 3,
            kind: SessionEntryKind::Assistant {
                message: Message::assistant_text("hola"),
            },
        },
    ];
    let tree = SessionTree { entries };
    let branch = tree.branch("b");
    let ids: Vec<&str> = branch.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, vec!["root", "a", "b"]);

    let tips = tree.tips();
    let tip_ids: Vec<&str> = tips.iter().map(|e| e.id.as_str()).collect();
    assert!(tip_ids.contains(&"b"));
    assert!(tip_ids.contains(&"fork"));
    assert!(!tip_ids.contains(&"a"));
}
