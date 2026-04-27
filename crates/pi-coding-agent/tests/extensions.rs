use pi_coding_agent::extensions::{
    discover, ExtensionManifest, ExtensionTool, ExtensionToolManifest, LoadedExtension,
};
use pi_tools::{Tool, ToolContext};
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

fn make_executable(path: &std::path::Path, body: &str) {
    std::fs::write(path, body).unwrap();
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

fn write_manifest(dir: &std::path::Path, manifest: &ExtensionManifest) {
    let path = dir.join("pi-extension.json");
    std::fs::write(&path, serde_json::to_string_pretty(manifest).unwrap()).unwrap();
}

#[test]
fn discover_walks_nested_extension_dirs() {
    let root = tempfile::tempdir().unwrap();
    // Layout: <root>/ext-a/pi-extension.json, <root>/ext-b/pi-extension.json
    for name in ["ext-a", "ext-b"] {
        let d = root.path().join(name);
        std::fs::create_dir_all(&d).unwrap();
        let exe = d.join("run.sh");
        make_executable(
            &exe,
            "#!/bin/sh\nprintf '{\"output\":\"hi\",\"is_error\":false}\\n'\n",
        );
        write_manifest(
            &d,
            &ExtensionManifest {
                name: name.into(),
                version: "0.1.0".into(),
                executable: "./run.sh".into(),
                tools: vec![ExtensionToolManifest {
                    name: format!("{name}-tool"),
                    description: "tool".into(),
                    input_schema: serde_json::Value::Null,
                }],
                commands: vec![],
                timeout_ms: Some(5_000),
                keybindings: vec![],
                hooks: vec![],
            },
        );
    }
    let exts = discover(&[root.path().to_path_buf()]);
    let names: Vec<&str> = exts.iter().map(|e| e.manifest.name.as_str()).collect();
    assert!(names.contains(&"ext-a"), "found: {:?}", names);
    assert!(names.contains(&"ext-b"));
}

#[test]
fn discover_walks_single_extension_root_with_manifest_at_root() {
    let root = tempfile::tempdir().unwrap();
    let exe = root.path().join("run.sh");
    make_executable(
        &exe,
        "#!/bin/sh\nprintf '{\"output\":\"hi\",\"is_error\":false}\\n'\n",
    );
    write_manifest(
        root.path(),
        &ExtensionManifest {
            name: "single".into(),
            version: "0.1.0".into(),
            executable: "./run.sh".into(),
            tools: vec![ExtensionToolManifest {
                name: "do".into(),
                description: "do thing".into(),
                input_schema: serde_json::Value::Null,
            }],
            commands: vec![],
            timeout_ms: Some(5_000),
            keybindings: vec![],
                hooks: vec![],
        },
    );
    let exts = discover(&[root.path().to_path_buf()]);
    assert_eq!(exts.len(), 1);
    assert_eq!(exts[0].manifest.name, "single");
}

#[tokio::test]
async fn extension_tool_converts_json_output_to_tool_result() {
    let root = tempfile::tempdir().unwrap();
    let exe = root.path().join("run.sh");
    make_executable(
        &exe,
        "#!/bin/sh\nprintf '{\"output\":\"hi\",\"is_error\":false}\\n'\n",
    );
    let manifest = ExtensionManifest {
        name: "single".into(),
        version: "0.1.0".into(),
        executable: "./run.sh".into(),
        tools: vec![ExtensionToolManifest {
            name: "do".into(),
            description: "do thing".into(),
            input_schema: serde_json::Value::Null,
        }],
        commands: vec![],
        timeout_ms: Some(5_000),
        keybindings: vec![],
                hooks: vec![],
    };
    write_manifest(root.path(), &manifest);

    let loaded = Arc::new(LoadedExtension {
        manifest: manifest.clone(),
        root: root.path().to_path_buf(),
    });
    let tool = ExtensionTool {
        ext: loaded,
        spec: manifest.tools[0].clone(),
    };
    let ctx = ToolContext {
        cwd: root.path().to_path_buf(),
        max_output_bytes: 16 * 1024,
    };
    let r = tool.invoke(&ctx, "call-1", json!({})).await.unwrap();
    assert!(!r.is_error);
    assert_eq!(r.model_output, "hi");
    assert_eq!(r.tool_use_id, "call-1");
}
