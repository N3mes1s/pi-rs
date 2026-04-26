use pi_tools::{ToolContext, ToolRegistry};
use serde_json::json;

#[tokio::test]
async fn bash_times_out_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ToolContext {
        cwd: dir.path().to_path_buf(),
        max_output_bytes: 64 * 1024,
    };
    let reg = ToolRegistry::with_defaults();
    let bash = reg.get("bash").unwrap();
    let r = bash
        .invoke(
            &ctx,
            "1",
            json!({"command": "sleep 5", "timeout_ms": 100}),
        )
        .await
        .unwrap();
    assert!(r.is_error, "expected timeout to be reported as error");
    assert!(
        r.model_output.contains("timed out"),
        "model_output was: {}",
        r.model_output
    );
    assert!(r.model_output.contains("100ms"));
    if let Some(d) = &r.display {
        assert_eq!(d.get("timeout"), Some(&serde_json::Value::Bool(true)));
    }
}

#[tokio::test]
async fn read_truncates_large_files_via_model_output_cap() {
    let dir = tempfile::tempdir().unwrap();
    let big_path = dir.path().join("big.txt");
    // Build a file substantially larger than the cap so truncation kicks in.
    let line = "abcdefghij".repeat(100); // 1000 bytes per line
    let mut content = String::new();
    for _ in 0..200 {
        content.push_str(&line);
        content.push('\n');
    }
    std::fs::write(&big_path, &content).unwrap();

    let ctx = ToolContext {
        cwd: dir.path().to_path_buf(),
        // tiny cap so truncation is forced
        max_output_bytes: 4 * 1024,
    };
    let reg = ToolRegistry::with_defaults();
    let read = reg.get("read").unwrap();
    let r = read
        .invoke(&ctx, "1", json!({"path": "big.txt"}))
        .await
        .unwrap();
    assert!(!r.is_error);
    assert!(
        r.model_output.contains("truncated"),
        "expected truncation marker, got: {}",
        &r.model_output[..r.model_output.len().min(200)]
    );
    // It should not be vastly larger than the cap (some slack for the marker).
    assert!(r.model_output.len() < 4 * 1024 + 256);
}

#[tokio::test]
async fn read_returns_image_attachment_as_base64() {
    let dir = tempfile::tempdir().unwrap();
    // A 1x1 PNG (smallest valid-ish payload). We don't need a valid PNG —
    // ReadTool only branches on extension and base64-encodes the bytes.
    let payload: &[u8] = b"\x89PNG\r\n\x1a\n-fakebytes-";
    let p = dir.path().join("pixel.png");
    std::fs::write(&p, payload).unwrap();

    let ctx = ToolContext {
        cwd: dir.path().to_path_buf(),
        max_output_bytes: 64 * 1024,
    };
    let reg = ToolRegistry::with_defaults();
    let read = reg.get("read").unwrap();
    let r = read
        .invoke(&ctx, "1", json!({"path": "pixel.png"}))
        .await
        .unwrap();
    assert!(!r.is_error);
    assert!(r.model_output.contains("[image"));
    let d = r.display.expect("image read should have display payload");
    assert_eq!(d.get("kind").and_then(|v| v.as_str()), Some("image"));
    assert_eq!(d.get("mime").and_then(|v| v.as_str()), Some("image/png"));
    let b64 = d.get("base64").and_then(|v| v.as_str()).unwrap();
    use base64::Engine;
    let decoded = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
    assert_eq!(decoded, payload);

    // jpg/jpeg should map to image/jpeg
    let p2 = dir.path().join("pic.jpg");
    std::fs::write(&p2, b"jpeg-bytes").unwrap();
    let r2 = read
        .invoke(&ctx, "2", json!({"path": "pic.jpg"}))
        .await
        .unwrap();
    let d2 = r2.display.unwrap();
    assert_eq!(d2.get("mime").and_then(|v| v.as_str()), Some("image/jpeg"));
}
