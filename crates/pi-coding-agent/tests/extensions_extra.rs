//! Extra coverage for the extensions module.

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
fn discover_returns_empty_for_a_path_that_does_not_exist() {
    let out = discover(&[std::path::PathBuf::from("/nope/does/not/exist")]);
    assert!(out.is_empty());
}

#[test]
fn discover_via_explicit_pi_extension_json_file_path() {
    let root = tempfile::tempdir().unwrap();
    let exe = root.path().join("run.sh");
    make_executable(&exe, "#!/bin/sh\necho ok\n");
    let manifest_path = root.path().join("pi-extension.json");
    std::fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&ExtensionManifest {
            name: "viafile".into(),
            version: "0.0.1".into(),
            executable: "./run.sh".into(),
            tools: vec![],
            commands: vec![],
            timeout_ms: Some(2_000),
            keybindings: vec![],
                hooks: vec![],
        })
        .unwrap(),
    )
    .unwrap();

    // Pass the manifest *file* itself, not the parent dir.
    let exts = discover(&[manifest_path]);
    assert_eq!(exts.len(), 1);
    assert_eq!(exts[0].manifest.name, "viafile");
}

#[test]
fn discover_skips_subdirs_with_invalid_manifest_json() {
    let root = tempfile::tempdir().unwrap();
    let bad = root.path().join("bad");
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::write(bad.join("pi-extension.json"), "{ not valid").unwrap();
    // A second, valid sibling.
    let good = root.path().join("good");
    std::fs::create_dir_all(&good).unwrap();
    let exe = good.join("run.sh");
    make_executable(&exe, "#!/bin/sh\necho ok\n");
    write_manifest(
        &good,
        &ExtensionManifest {
            name: "good".into(),
            version: "0.1".into(),
            executable: "./run.sh".into(),
            tools: vec![],
            commands: vec![],
            timeout_ms: Some(1_000),
            keybindings: vec![],
                hooks: vec![],
        },
    );

    let exts = discover(&[root.path().to_path_buf()]);
    let names: Vec<&str> = exts.iter().map(|e| e.manifest.name.as_str()).collect();
    assert!(names.contains(&"good"));
    assert!(!names.contains(&"bad"));
}

#[test]
fn loaded_extension_executable_path_handles_absolute_executable() {
    let manifest = ExtensionManifest {
        name: "x".into(),
        version: "0.0.1".into(),
        executable: "/usr/bin/true".into(),
        tools: vec![],
        commands: vec![],
        timeout_ms: None,
        keybindings: vec![],
                hooks: vec![],
    };
    let loaded = LoadedExtension {
        manifest,
        root: std::path::PathBuf::from("/somewhere/else"),
    };
    assert_eq!(
        loaded.executable_path(),
        std::path::PathBuf::from("/usr/bin/true")
    );
}

#[test]
fn loaded_extension_default_timeout_is_two_minutes() {
    let manifest = ExtensionManifest {
        name: "x".into(),
        version: "0.0.1".into(),
        executable: "./e".into(),
        tools: vec![],
        commands: vec![],
        timeout_ms: None,
        keybindings: vec![],
                hooks: vec![],
    };
    let l = LoadedExtension {
        manifest,
        root: std::path::PathBuf::from("/r"),
    };
    assert_eq!(l.timeout(), std::time::Duration::from_millis(120_000));
}

#[test]
fn extension_tool_is_never_read_only() {
    let manifest = ExtensionManifest {
        name: "ext".into(),
        version: "0.0.1".into(),
        executable: "./run.sh".into(),
        tools: vec![ExtensionToolManifest {
            name: "do".into(),
            description: "d".into(),
            input_schema: serde_json::Value::Null,
        }],
        commands: vec![],
        timeout_ms: Some(1_000),
        keybindings: vec![],
                hooks: vec![],
    };
    let loaded = Arc::new(LoadedExtension {
        manifest: manifest.clone(),
        root: std::path::PathBuf::from("/tmp"),
    });
    let tool = ExtensionTool {
        ext: loaded,
        spec: manifest.tools[0].clone(),
    };
    assert!(!tool.read_only());
}

#[tokio::test]
async fn extension_invoke_with_nonexistent_executable_errors() {
    // Manifest pointing at an exe that doesn't exist → spawn fails.
    let root = tempfile::tempdir().unwrap();
    let manifest = ExtensionManifest {
        name: "ext".into(),
        version: "0.0.1".into(),
        executable: "./does-not-exist.sh".into(),
        tools: vec![ExtensionToolManifest {
            name: "do".into(),
            description: "d".into(),
            input_schema: serde_json::Value::Null,
        }],
        commands: vec![],
        timeout_ms: Some(1_000),
        keybindings: vec![],
                hooks: vec![],
    };
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
        max_output_bytes: 4096,
    };
    let r = tool.invoke(&ctx, "c1", json!({})).await;
    assert!(r.is_err(), "spawn of missing exe should fail");
}

#[tokio::test]
async fn extension_invoke_with_failing_exit_status_returns_error_result() {
    let root = tempfile::tempdir().unwrap();
    let exe = root.path().join("run.sh");
    // Write to stderr, then exit non-zero.
    make_executable(&exe, "#!/bin/sh\necho boom 1>&2\nsleep 0.05\nexit 7\n");
    let manifest = ExtensionManifest {
        name: "ext".into(),
        version: "0.0.1".into(),
        executable: "./run.sh".into(),
        tools: vec![ExtensionToolManifest {
            name: "do".into(),
            description: "d".into(),
            input_schema: serde_json::Value::Null,
        }],
        commands: vec![],
        timeout_ms: Some(2_000),
        keybindings: vec![],
                hooks: vec![],
    };
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
        max_output_bytes: 4096,
    };
    let res = tool.invoke(&ctx, "c1", json!({})).await.unwrap();
    assert!(res.is_error);
    assert!(res.model_output.contains("exited"));
    assert!(res.model_output.contains("boom"));
}

#[tokio::test]
async fn extension_invoke_with_json_lacking_output_field_falls_back_to_stdout() {
    let root = tempfile::tempdir().unwrap();
    let exe = root.path().join("run.sh");
    // Valid JSON, no `output` field. The fallback uses the raw stdout.
    make_executable(
        &exe,
        "#!/bin/sh\nprintf '{\"misc\":\"value\"}\\n'\n",
    );
    let manifest = ExtensionManifest {
        name: "ext".into(),
        version: "0.0.1".into(),
        executable: "./run.sh".into(),
        tools: vec![ExtensionToolManifest {
            name: "do".into(),
            description: "d".into(),
            input_schema: serde_json::Value::Null,
        }],
        commands: vec![],
        timeout_ms: Some(2_000),
        keybindings: vec![],
                hooks: vec![],
    };
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
        max_output_bytes: 4096,
    };
    let r = tool.invoke(&ctx, "c1", json!({})).await.unwrap();
    assert!(!r.is_error);
    // Fallback prints the raw stdout JSON.
    assert!(r.model_output.contains("misc"));
}

#[tokio::test]
async fn extension_invoke_times_out_when_exe_sleeps_longer_than_timeout() {
    let root = tempfile::tempdir().unwrap();
    let exe = root.path().join("run.sh");
    make_executable(&exe, "#!/bin/sh\nsleep 30\n");
    let manifest = ExtensionManifest {
        name: "slow".into(),
        version: "0.0.1".into(),
        executable: "./run.sh".into(),
        tools: vec![ExtensionToolManifest {
            name: "do".into(),
            description: "d".into(),
            input_schema: serde_json::Value::Null,
        }],
        commands: vec![],
        timeout_ms: Some(200),
        keybindings: vec![],
                hooks: vec![],
    };
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
        max_output_bytes: 4096,
    };
    let res = tool.invoke(&ctx, "c1", json!({})).await.unwrap();
    assert!(res.is_error, "expected timeout to surface as is_error");
    assert!(res.model_output.contains("timed out"));
}

#[test]
fn loaded_extension_spec_falls_back_to_object_schema_when_input_schema_null() {
    let manifest = ExtensionManifest {
        name: "ext".into(),
        version: "0.0.1".into(),
        executable: "./run.sh".into(),
        tools: vec![ExtensionToolManifest {
            name: "do".into(),
            description: "d".into(),
            input_schema: serde_json::Value::Null,
        }],
        commands: vec![],
        timeout_ms: None,
        keybindings: vec![],
                hooks: vec![],
    };
    let loaded = Arc::new(LoadedExtension {
        manifest: manifest.clone(),
        root: std::path::PathBuf::from("/tmp"),
    });
    let tool = ExtensionTool {
        ext: loaded,
        spec: manifest.tools[0].clone(),
    };
    let s = tool.spec();
    assert_eq!(s.input_schema, serde_json::json!({"type": "object"}));
}
