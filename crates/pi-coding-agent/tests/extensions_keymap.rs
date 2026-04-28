//! Tests for extension-registered keybindings (Step 5).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pi_agent_core::settings::ThinkingSetting;
use pi_coding_agent::keymap::{parse_chord, Keymap};
use pi_coding_agent::modes::interactive::{handle_key, KeyOutcome, View};

fn ke(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, mods)
}

fn fresh_view_with_keymap(keymap: Keymap) -> View {
    View::new(keymap, ThinkingSetting::Off)
}

// ── Test 1 ──────────────────────────────────────────────────────────────────
// bind_extension inserts the right chord into extension_bindings.
#[test]
fn bind_extension_inserts_entry() {
    let mut km = Keymap::defaults();
    let ok = km.bind_extension("Ctrl+B", 0, "deploy".into());
    assert!(ok, "bind_extension should return true for a valid chord");

    let chord = parse_chord("Ctrl+B").expect("Ctrl+B must parse");
    assert!(
        km.extension_bindings.contains_key(&chord),
        "extension_bindings must contain Ctrl+B after bind_extension"
    );
    let entry = &km.extension_bindings[&chord];
    assert_eq!(entry.0, 0, "extension index should be 0");
    assert_eq!(entry.1, "deploy", "command name should be 'deploy'");
}

// ── Test 2 ──────────────────────────────────────────────────────────────────
// lookup_extension returns Some((0, "deploy")) for Ctrl+B.
#[test]
fn lookup_extension_returns_correct_entry() {
    let mut km = Keymap::defaults();
    km.bind_extension("Ctrl+B", 0, "deploy".into());

    let ev = ke(KeyCode::Char('b'), KeyModifiers::CONTROL);
    let result = km.lookup_extension(&ev);
    assert_eq!(
        result,
        Some((0, "deploy".into())),
        "lookup_extension must return Some((0, \"deploy\")) for Ctrl+B"
    );
}

// ── Test 3 ──────────────────────────────────────────────────────────────────
// lookup_extension returns None for an unbound chord.
#[test]
fn lookup_extension_returns_none_for_unbound() {
    let km = Keymap::defaults();
    // Ctrl+B is not bound in defaults.
    let ev = ke(KeyCode::Char('b'), KeyModifiers::CONTROL);
    assert_eq!(
        km.lookup_extension(&ev),
        None,
        "lookup_extension should return None for an unbound chord"
    );
}

// ── Test 4 ──────────────────────────────────────────────────────────────────
// handle_key returns KeyOutcome::ExtensionCommand for Ctrl+B when bound.
#[test]
fn handle_key_returns_extension_command_for_bound_chord() {
    let mut km = Keymap::defaults();
    km.bind_extension("Ctrl+B", 0, "deploy".into());

    let mut view = fresh_view_with_keymap(km);
    let ev = ke(KeyCode::Char('b'), KeyModifiers::CONTROL);
    let outcome = handle_key(&mut view, &ev);

    match outcome {
        KeyOutcome::ExtensionCommand {
            extension_index,
            command_name,
            args,
        } => {
            assert_eq!(extension_index, 0);
            assert_eq!(command_name, "deploy");
            assert_eq!(args, "");
        }
        other => panic!("expected ExtensionCommand, got {:?}", other),
    }
}

// ── Test 5 ──────────────────────────────────────────────────────────────────
// bind_extension silently ignores (returns false, no panic) invalid chords.
#[test]
fn bind_extension_ignores_invalid_chord() {
    let mut km = Keymap::defaults();
    let ok = km.bind_extension("Ctrl+Bogus", 0, "deploy".into());
    assert!(
        !ok,
        "bind_extension should return false for an invalid chord"
    );
    // extension_bindings should remain empty.
    assert!(
        km.extension_bindings.is_empty(),
        "extension_bindings should not grow after a failed bind"
    );
    // A second obviously-broken string also should not panic.
    let ok2 = km.bind_extension("", 1, "noop".into());
    assert!(!ok2);
}
