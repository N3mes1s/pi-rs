//! Integration tests for extension-registered slash commands.

use pi_coding_agent::extensions::{ExtensionCommandManifest, ExtensionManifest, LoadedExtension};
use pi_coding_agent::slash::{SlashKind, SlashRegistry};
use std::path::PathBuf;

// ─── helper ──────────────────────────────────────────────────────────────────

/// Build a `LoadedExtension` with the given commands list and a dummy root /
/// executable path.
fn mock_extension(commands: Vec<ExtensionCommandManifest>, executable: &str) -> LoadedExtension {
    LoadedExtension {
        manifest: ExtensionManifest {
            name: "test-ext".into(),
            version: "0.0.0".into(),
            executable: executable.to_string(),
            tools: vec![],
            commands,
            timeout_ms: None,
            keybindings: vec![],
            hooks: vec![],
            replaces_builtin: vec![],
            startup_executable: None,
        },
        root: PathBuf::from("/tmp"),
    }
}

// ─── test 1: registration ─────────────────────────────────────────────────

#[test]
fn extensions_slash_register_inserts_with_correct_kind() {
    let cmd = ExtensionCommandManifest {
        name: "deploy".into(),
        description: "Deploy to production".into(),
    };

    let ext = mock_extension(vec![cmd.clone()], "/bin/true");

    let mut registry = SlashRegistry::new();

    // Build the items slice as startup.rs does.
    let ext_cmds: Vec<(usize, &ExtensionCommandManifest)> =
        std::iter::once((0usize, &cmd)).collect();
    registry.register_extension_commands(&ext_cmds);

    // The command must now be retrievable.
    let slash_cmd = registry
        .get("deploy")
        .expect("deploy command should be registered");

    assert_eq!(slash_cmd.name, "deploy");
    assert_eq!(slash_cmd.description, "Deploy to production");

    match &slash_cmd.kind {
        SlashKind::Extension {
            extension_index,
            command_name,
        } => {
            assert_eq!(*extension_index, 0);
            assert_eq!(command_name, "deploy");
        }
        other => panic!("expected SlashKind::Extension, got {:?}", other),
    }

    // The extension is only used for assertion; suppress unused warning.
    drop(ext);
}

#[test]
fn extensions_slash_does_not_overwrite_builtins() {
    // "quit" is a built-in; an extension must not shadow it.
    let cmd = ExtensionCommandManifest {
        name: "quit".into(),
        description: "extension quit".into(),
    };
    let mut registry = SlashRegistry::new();
    let items = vec![(0usize, &cmd)];
    registry.register_extension_commands(&items);

    let slash_cmd = registry.get("quit").expect("quit should still exist");
    assert!(
        matches!(slash_cmd.kind, SlashKind::Builtin),
        "built-in must not be overwritten by extension"
    );
}

#[test]
fn extensions_slash_names_includes_registered_command() {
    let cmd = ExtensionCommandManifest {
        name: "my-cmd".into(),
        description: "does stuff".into(),
    };
    let mut registry = SlashRegistry::new();
    let items = vec![(0usize, &cmd)];
    registry.register_extension_commands(&items);

    assert!(
        registry.names().contains(&"my-cmd".to_string()),
        "names() should include extension-registered command"
    );
}

// ─── test 2: run_command ──────────────────────────────────────────────────

/// Write a small shell script to a temp file, make it executable, and return
/// its path.
fn write_echo_script(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("echo_args.sh");
    std::fs::write(&path, "#!/bin/sh\necho \"$@\"\n").expect("write script");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
    }

    path
}

#[tokio::test]
async fn extensions_slash_run_command_captures_stdout_with_args() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = write_echo_script(dir.path());

    let ext = LoadedExtension {
        manifest: ExtensionManifest {
            name: "echo-ext".into(),
            version: "0.1.0".into(),
            executable: script.to_string_lossy().to_string(),
            tools: vec![],
            commands: vec![ExtensionCommandManifest {
                name: "echo".into(),
                description: "echoes args".into(),
            }],
            timeout_ms: Some(5_000),
            keybindings: vec![],
            hooks: vec![],
            replaces_builtin: vec![],
            startup_executable: None,
        },
        root: dir.path().to_path_buf(),
    };

    let stdout = pi_coding_agent::extensions::run_command(&ext, "echo", "hello world")
        .await
        .expect("run_command should succeed");

    // The shell script echoes its argv, which will be: "command echo hello world"
    // (the three args passed: "command", "echo", "hello world").
    assert!(
        stdout.contains("hello world"),
        "stdout should contain the args; got: {:?}",
        stdout
    );
}
