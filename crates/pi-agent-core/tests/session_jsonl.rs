use pi_agent_core::{SessionEntryKind, SessionManager};
use pi_ai::Message;

#[test]
fn session_appends_jsonl_with_branching() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let mgr = SessionManager::on_disk(dir.path().to_path_buf(), cwd.path().to_path_buf()).unwrap();
    let meta = mgr.create("anthropic", "sonnet").unwrap();

    let user = mgr
        .append(&meta.id, SessionEntryKind::User { message: Message::user_text("hi") })
        .unwrap();
    let assistant = mgr
        .append(
            &meta.id,
            SessionEntryKind::Assistant { message: Message::assistant_text("hello") },
        )
        .unwrap();
    assert_eq!(assistant.parent_id.as_deref(), Some(user.id.as_str()));

    // Fork from the user message and add a different assistant reply.
    mgr.fork(&meta.id, &user.id).unwrap();
    let assistant2 = mgr
        .append(
            &meta.id,
            SessionEntryKind::Assistant { message: Message::assistant_text("hola") },
        )
        .unwrap();
    assert_eq!(assistant2.parent_id.as_deref(), Some(user.id.as_str()));

    // Tree should now have two leaf assistants under the same user.
    let tree = mgr.tree(&meta.id).unwrap();
    let leaves = tree.tips();
    assert_eq!(leaves.len(), 2);

    // JSONL file should be there.
    let txt = std::fs::read_to_string(&meta.path).unwrap();
    let lines: Vec<&str> = txt.lines().filter(|l| !l.is_empty()).collect();
    assert!(lines.len() >= 4);
}
