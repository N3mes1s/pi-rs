//! Tests for the `apply_replacements` free function and the
//! `replaces_builtin` manifest field.
//!
//! Scenario:
//!   1. Build a `ToolRegistry::with_defaults()` — it includes a built-in
//!      tool named `bash`.
//!   2. Create a fake extension that declares `replaces_builtin: ["bash"]`
//!      and also exports a tool named `bash` (stub implementation).
//!   3. Call `extensions::apply_replacements` to strip the builtin, then
//!      register the extension tool.
//!   4. Assert that `bash` now resolves to the extension tool (not the
//!      builtin) and that there is only one registration.

use pi_coding_agent::extensions::{
    apply_replacements, extension_tools, ExtensionManifest, ExtensionToolManifest, LoadedExtension,
};
use pi_tools::ToolRegistry;
use std::path::PathBuf;

// ── helper ────────────────────────────────────────────────────────────────────

/// Construct a `LoadedExtension` that declares it replaces the `bash` builtin
/// and exports a tool also named `bash`.
fn fake_bash_replacement_ext() -> LoadedExtension {
    LoadedExtension {
        manifest: ExtensionManifest {
            name: "custom-bash".into(),
            version: "0.1.0".into(),
            executable: "/bin/true".into(),
            tools: vec![ExtensionToolManifest {
                name: "bash".into(),
                description: "Custom bash replacement from extension".into(),
                input_schema: serde_json::json!({"type": "object", "properties": {
                    "command": {"type": "string"}
                }}),
            }],
            commands: vec![],
            timeout_ms: Some(5_000),
            keybindings: vec![],
            hooks: vec![],
            replaces_builtin: vec!["bash".to_string()],
            startup_executable: None,
        },
        root: PathBuf::from("/tmp/custom-bash"),
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// `ToolRegistry::with_defaults()` must contain a built-in `bash` tool before
/// any replacement logic runs.
#[test]
fn default_registry_has_builtin_bash() {
    let reg = ToolRegistry::with_defaults();
    assert!(
        reg.get("bash").is_some(),
        "expected 'bash' in default registry; found: {:?}",
        reg.names()
    );
}

/// After `apply_replacements` + registering the extension tool, `bash` must
/// resolve to the extension tool (identified by its description), and no
/// duplicate must exist.
#[test]
fn apply_replacements_swaps_builtin_bash_for_extension_tool() {
    let mut reg = ToolRegistry::with_defaults();

    // Sanity: builtin is present before replacement.
    assert!(
        reg.get("bash").is_some(),
        "precondition: builtin bash absent"
    );

    let ext = fake_bash_replacement_ext();
    let exts = vec![ext];

    // Strip builtins declared in replaces_builtin.
    apply_replacements(&mut reg, &exts);

    // After stripping, the builtin must be gone.
    assert!(
        reg.get("bash").is_none(),
        "builtin bash should have been unregistered; tools: {:?}",
        reg.names()
    );

    // Now register the extension tools. Per RFD 0027 §4.5 #5: extension
    // tools intentionally override matching builtins, so use the
    // explicit override path.
    for t in extension_tools(&exts) {
        reg.register_or_replace(t);
    }

    // The extension tool must now be registered.
    let tool = reg
        .get("bash")
        .expect("extension bash must be registered after apply_replacements");

    // Confirm it is the extension tool by checking the spec description.
    assert_eq!(tool.spec().name, "bash", "tool name must be 'bash'");
    assert!(
        tool.spec().description.contains("Custom bash replacement"),
        "expected extension description; got: {:?}",
        tool.spec().description
    );

    // Exactly one tool named 'bash' — no duplicates.
    let bash_count = reg.names().iter().filter(|n| n.as_str() == "bash").count();
    assert_eq!(
        bash_count,
        1,
        "expected exactly one 'bash' tool; names: {:?}",
        reg.names()
    );
}

/// `apply_replacements` with an extension that declares no `replaces_builtin`
/// entries must be a no-op (all defaults stay registered).
#[test]
fn apply_replacements_noop_when_replaces_builtin_is_empty() {
    let mut reg = ToolRegistry::with_defaults();
    let names_before = reg.names();

    let ext = LoadedExtension {
        manifest: ExtensionManifest {
            name: "no-replace".into(),
            version: "0.1.0".into(),
            executable: "/bin/true".into(),
            tools: vec![],
            commands: vec![],
            timeout_ms: None,
            keybindings: vec![],
            hooks: vec![],
            replaces_builtin: vec![], // nothing to replace
            startup_executable: None,
        },
        root: PathBuf::from("/tmp"),
    };

    apply_replacements(&mut reg, &[ext]);

    let names_after = reg.names();
    assert_eq!(
        names_before, names_after,
        "apply_replacements with empty list must not change the registry"
    );
}

/// Replacing a tool that does not exist in the registry must be a no-op (must
/// not panic).
#[test]
fn apply_replacements_nonexistent_name_does_not_panic() {
    let mut reg = ToolRegistry::with_defaults();
    let ext = LoadedExtension {
        manifest: ExtensionManifest {
            name: "ghost-ext".into(),
            version: "0.1.0".into(),
            executable: "/bin/true".into(),
            tools: vec![],
            commands: vec![],
            timeout_ms: None,
            keybindings: vec![],
            hooks: vec![],
            replaces_builtin: vec!["nonexistent_tool_xyz".to_string()],
            startup_executable: None,
        },
        root: PathBuf::from("/tmp"),
    };
    // Must not panic.
    apply_replacements(&mut reg, &[ext]);
    // All original tools still present.
    assert!(reg.get("bash").is_some());
}
