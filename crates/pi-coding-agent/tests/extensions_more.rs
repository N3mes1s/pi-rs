//! Additional coverage for the extensions subprocess machinery.

use pi_coding_agent::extensions::{
    discover, extension_tools, run_command, ExtensionManifest, ExtensionTool,
    ExtensionToolManifest, LoadedExtension,
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
fn manifest_with_two_tools_produces_two_tool_entries() {
    let root = tempfile::tempdir().unwrap();
    let exe = root.path().join("run.sh");
    make_executable(
        &exe,
        "#!/bin/sh\nprintf '{\"output\":\"ok\",\"is_error\":false}\\n'\n",
    );
    let manifest = ExtensionManifest {
        name: "multi".into(),
        version: "0.1.0".into(),
        executable: "./run.sh".into(),
        tools: vec![
            ExtensionToolManifest {
                name: "alpha".into(),
                description: "first".into(),
                input_schema: serde_json::Value::Null,
            },
            ExtensionToolManifest {
                name: "beta".into(),
                description: "second".into(),
                input_schema: json!({"type": "object"}),
            },
        ],
        commands: vec![],
        timeout_ms: Some(5_000),
        keybindings: vec![],
                hooks: vec![],
        replaces_builtin: vec![],
        startup_executable: None,
    };
    write_manifest(root.path(), &manifest);

    let exts = discover(&[root.path().to_path_buf()]);
    assert_eq!(exts.len(), 1);
    let tools = extension_tools(&exts);
    assert_eq!(tools.len(), 2);
    let names: Vec<String> = tools.iter().map(|t| t.spec().name).collect();
    assert!(names.contains(&"alpha".to_string()));
    assert!(names.contains(&"beta".to_string()));
}

#[tokio::test]
async fn extension_with_non_json_stdout_still_succeeds() {
    let root = tempfile::tempdir().unwrap();
    let exe = root.path().join("run.sh");
    make_executable(&exe, "#!/bin/sh\nprintf 'just plain text\\n'\n");
    let manifest = ExtensionManifest {
        name: "plain".into(),
        version: "0.1.0".into(),
        executable: "./run.sh".into(),
        tools: vec![ExtensionToolManifest {
            name: "do".into(),
            description: "d".into(),
            input_schema: serde_json::Value::Null,
        }],
        commands: vec![],
        timeout_ms: Some(5_000),
        keybindings: vec![],
                hooks: vec![],
        replaces_builtin: vec![],
        startup_executable: None,
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
    let r = tool.invoke(&ctx, "c1", json!({})).await.unwrap();
    assert!(!r.is_error);
    assert!(r.model_output.contains("just plain text"));
}

#[tokio::test]
async fn extension_json_output_with_is_error_true_surfaces_as_error() {
    let root = tempfile::tempdir().unwrap();
    let exe = root.path().join("run.sh");
    make_executable(
        &exe,
        "#!/bin/sh\nprintf '{\"output\":\"x\",\"is_error\":true}\\n'\n",
    );
    let manifest = ExtensionManifest {
        name: "errs".into(),
        version: "0.1.0".into(),
        executable: "./run.sh".into(),
        tools: vec![ExtensionToolManifest {
            name: "do".into(),
            description: "d".into(),
            input_schema: serde_json::Value::Null,
        }],
        commands: vec![],
        timeout_ms: Some(5_000),
        keybindings: vec![],
                hooks: vec![],
        replaces_builtin: vec![],
        startup_executable: None,
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
    let r = tool.invoke(&ctx, "c1", json!({})).await.unwrap();
    assert!(r.is_error, "should surface as error: {:?}", r);
    assert_eq!(r.model_output, "x");
}

#[tokio::test]
async fn run_command_passes_argv_to_executable() {
    let root = tempfile::tempdir().unwrap();
    let exe = root.path().join("run.sh");
    // Echo argv as plain text. run_command calls: <exe> command <name> <args>
    make_executable(&exe, "#!/bin/sh\nprintf 'argv:%s\\n' \"$@\"\n");
    let manifest = ExtensionManifest {
        name: "cmds".into(),
        version: "0.1.0".into(),
        executable: "./run.sh".into(),
        tools: vec![],
        commands: vec![],
        timeout_ms: Some(5_000),
        keybindings: vec![],
                hooks: vec![],
        replaces_builtin: vec![],
        startup_executable: None,
    };
    write_manifest(root.path(), &manifest);
    let loaded = LoadedExtension {
        manifest,
        root: root.path().to_path_buf(),
    };

    let out = run_command(&loaded, "status", "--verbose")
        .await
        .expect("run_command should succeed");
    assert!(out.contains("argv:command"), "stdout was: {out:?}");
    assert!(out.contains("argv:status"), "stdout was: {out:?}");
    assert!(out.contains("argv:--verbose"), "stdout was: {out:?}");
}
