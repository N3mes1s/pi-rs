use pi_tools::{ToolContext, ToolRegistry};
use serde_json::json;

#[tokio::test]
async fn read_write_edit_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ToolContext {
        cwd: dir.path().to_path_buf(),
        max_output_bytes: 64 * 1024,
    };
    let reg = ToolRegistry::with_extras();
    let write = reg.get("write").unwrap();
    let read = reg.get("read").unwrap();
    let edit = reg.get("edit").unwrap();
    let bash = reg.get("bash").unwrap();
    let ls = reg.get("ls").unwrap();
    let grep = reg.get("grep").unwrap();
    let find = reg.get("find").unwrap();

    let r = write
        .invoke(&ctx, "1", json!({"path": "hello.txt", "content": "alpha\nbeta\ngamma\n"}))
        .await
        .unwrap();
    assert!(!r.is_error);

    let r = read.invoke(&ctx, "2", json!({"path": "hello.txt"})).await.unwrap();
    assert!(r.model_output.contains("alpha"));
    assert!(r.model_output.contains("gamma"));

    let r = edit
        .invoke(&ctx, "3", json!({"path": "hello.txt", "old_string": "beta", "new_string": "BETA"}))
        .await
        .unwrap();
    assert!(!r.is_error);

    let r = read.invoke(&ctx, "4", json!({"path": "hello.txt"})).await.unwrap();
    assert!(r.model_output.contains("BETA"));

    let r = bash.invoke(&ctx, "5", json!({"command": "echo from-bash"})).await.unwrap();
    assert!(r.model_output.contains("from-bash"));
    assert!(r.model_output.contains("[exit 0]"));

    let r = ls.invoke(&ctx, "6", json!({})).await.unwrap();
    assert!(r.model_output.contains("hello.txt"));

    let r = grep
        .invoke(&ctx, "7", json!({"pattern": "BETA"}))
        .await
        .unwrap();
    assert!(r.model_output.contains("hello.txt"));

    let r = find
        .invoke(&ctx, "8", json!({"glob": "**/*.txt"}))
        .await
        .unwrap();
    assert!(r.model_output.contains("hello.txt"));
}

#[tokio::test]
async fn edit_rejects_non_unique() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ToolContext {
        cwd: dir.path().to_path_buf(),
        max_output_bytes: 64 * 1024,
    };
    let reg = ToolRegistry::with_extras();
    reg.get("write")
        .unwrap()
        .invoke(&ctx, "1", json!({"path": "x", "content": "foo foo"}))
        .await
        .unwrap();
    let r = reg
        .get("edit")
        .unwrap()
        .invoke(&ctx, "2", json!({"path": "x", "old_string": "foo", "new_string": "bar"}))
        .await
        .unwrap();
    assert!(r.is_error);
    let r = reg
        .get("edit")
        .unwrap()
        .invoke(
            &ctx,
            "3",
            json!({"path": "x", "old_string": "foo", "new_string": "bar", "replace_all": true}),
        )
        .await
        .unwrap();
    assert!(!r.is_error);
}
