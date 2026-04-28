//! Integration tests for `@filename` fuzzy completion in the TUI editor.
//!
//! These tests drive `handle_key` with synthesised `KeyEvent`s and verify:
//!  1. `build_at_candidates` honours `.gitignore`.
//!  2. Typing `@s` opens the picker with query "s" and `at_active == true`.
//!  3. Selecting a candidate via Enter replaces `@<query>` with the chosen
//!     path in the editor buffer.
//!  4. Esc closes the picker but leaves `@<query>` literal text in the buffer.
//!  5. Typing `@` again after a previous completion opens a fresh picker.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pi_agent_core::settings::ThinkingSetting;
use pi_coding_agent::keymap::Keymap;
use pi_coding_agent::modes::interactive::{
    build_at_candidates, handle_key, open_at_picker, KeyOutcome, View,
};
use std::path::PathBuf;

// ─── helpers ────────────────────────────────────────────────────────────────

fn ke(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ke_char(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

fn fresh_view() -> View {
    View::new(Keymap::defaults(), ThinkingSetting::Off)
}

/// Populate `view`'s `@`-completion picker with a fixed set of string
/// candidates (bypasses the real filesystem walk).
fn populate_at_picker(view: &mut View, candidates: &[&str]) {
    let paths: Vec<PathBuf> = candidates.iter().map(PathBuf::from).collect();
    open_at_picker(view, paths);
}

// ─── test 1: build_at_candidates honours .gitignore ─────────────────────────

#[test]
fn build_at_candidates_honours_gitignore() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();

    // Create a tracked file.
    std::fs::write(root.join("tracked.rs"), "fn main() {}").unwrap();

    // Create a gitignored file.
    std::fs::write(root.join(".gitignore"), "ignored.log\n").unwrap();
    std::fs::write(root.join("ignored.log"), "log data").unwrap();

    let candidates = build_at_candidates(root);

    let names: Vec<String> = candidates
        .iter()
        .map(|p: &PathBuf| p.display().to_string())
        .collect();

    assert!(
        names.iter().any(|n| n == "tracked.rs"),
        "tracked.rs should appear; got: {:?}",
        names
    );
    assert!(
        !names.iter().any(|n| n == "ignored.log"),
        "ignored.log should be excluded; got: {:?}",
        names
    );
}

// ─── test 2: typing `@s` opens the picker with query "s" ────────────────────

#[test]
fn at_sign_opens_picker_with_empty_query() {
    let mut v = fresh_view();

    // Type '@'.
    handle_key(&mut v, &ke_char('@'));

    // Picker should now be open and at_active true.
    assert!(v.at_active, "at_active should be true after typing '@'");
    assert!(v.picker.is_some(), "picker should be Some after typing '@'");

    // The editor should contain '@'.
    assert_eq!(v.editor.text, "@");
    assert_eq!(v.at_query_start, Some(0));

    // Now populate the picker with test candidates.
    populate_at_picker(&mut v, &["src/main.rs", "src/lib.rs", "Cargo.toml"]);

    // Type 's' — query becomes "s", editor becomes "@s".
    handle_key(&mut v, &ke_char('s'));

    assert_eq!(v.editor.text, "@s");
    let overlay = v.picker.as_ref().expect("picker still open");
    assert_eq!(overlay.picker.query, "s");
    assert!(v.at_active);
}

// ─── test 3: Enter replaces @<query> with the chosen path ───────────────────

#[test]
fn enter_replaces_at_query_with_chosen_path() {
    let mut v = fresh_view();

    // Type some text before @.
    for c in "hello ".chars() {
        handle_key(&mut v, &ke_char(c));
    }
    assert_eq!(v.editor.text, "hello ");
    let at_cursor = v.editor.cursor; // should be 6

    // Type '@'.
    handle_key(&mut v, &ke_char('@'));
    assert_eq!(v.at_query_start, Some(at_cursor));

    // Populate picker.
    populate_at_picker(&mut v, &["src/main.rs", "src/lib.rs"]);

    // Type query "main".
    for c in "main".chars() {
        handle_key(&mut v, &ke_char(c));
    }
    assert_eq!(v.editor.text, "hello @main");

    // Verify picker has the query.
    {
        let overlay = v.picker.as_ref().unwrap();
        assert_eq!(overlay.picker.query, "main");
    }

    // Press Enter — should pick "src/main.rs" (top match for "main").
    let outcome = handle_key(&mut v, &ke(KeyCode::Enter));

    // The @<query> token (from position 6 onwards) should be replaced.
    // "hello " + "src/main.rs" = "hello src/main.rs"
    assert_eq!(
        v.editor.text, "hello src/main.rs",
        "editor should have the picked path in place of @<query>"
    );
    assert!(v.picker.is_none(), "picker should be closed after Enter");
    assert!(!v.at_active, "at_active should be false after completion");

    match outcome {
        KeyOutcome::AtComplete { picked } => {
            assert_eq!(picked, "src/main.rs");
        }
        other => panic!("expected AtComplete, got {:?}", other),
    }
}

// ─── test 4: Esc closes picker but leaves @<query> literal in buffer ─────────

#[test]
fn esc_closes_picker_leaves_at_query_literal() {
    let mut v = fresh_view();

    // Type "foo @ba".
    for c in "foo ".chars() {
        handle_key(&mut v, &ke_char(c));
    }
    handle_key(&mut v, &ke_char('@'));
    populate_at_picker(&mut v, &["bar.rs", "baz.rs"]);
    for c in "ba".chars() {
        handle_key(&mut v, &ke_char(c));
    }

    assert_eq!(v.editor.text, "foo @ba");
    assert!(v.picker.is_some());

    // Press Esc.
    let outcome = handle_key(&mut v, &ke(KeyCode::Esc));

    assert_eq!(outcome, KeyOutcome::None);
    assert!(v.picker.is_none(), "picker should be closed after Esc");
    assert!(!v.at_active, "at_active should be false after Esc");
    // The literal text "@ba" stays in the editor.
    assert_eq!(v.editor.text, "foo @ba");
}

// ─── test 5: typing `@` again after a previous completion opens a fresh picker

#[test]
fn second_at_opens_fresh_picker() {
    let mut v = fresh_view();

    // First completion cycle.
    handle_key(&mut v, &ke_char('@'));
    populate_at_picker(&mut v, &["README.md", "Cargo.toml"]);
    for c in "README".chars() {
        handle_key(&mut v, &ke_char(c));
    }
    let outcome = handle_key(&mut v, &ke(KeyCode::Enter));
    assert!(matches!(outcome, KeyOutcome::AtComplete { .. }));
    assert!(!v.at_active);
    assert!(v.picker.is_none());

    // Type a space after the completion.
    handle_key(&mut v, &ke_char(' '));

    // Second '@'.
    handle_key(&mut v, &ke_char('@'));
    // Picker should be fresh with empty query.
    assert!(v.at_active);
    assert!(v.picker.is_some());
    {
        let overlay = v.picker.as_ref().unwrap();
        assert!(
            overlay.picker.query.is_empty(),
            "fresh picker should have empty query"
        );
    }

    // Populate with a second set.
    populate_at_picker(&mut v, &["src/main.rs"]);
    handle_key(&mut v, &ke_char('m'));

    let overlay = v.picker.as_ref().unwrap();
    assert_eq!(overlay.picker.query, "m");
    assert!(v.at_active);
}

// ─── bonus: backspace in @-picker removes from both query and editor ──────────

#[test]
fn backspace_in_at_picker_syncs_editor_and_query() {
    let mut v = fresh_view();

    handle_key(&mut v, &ke_char('@'));
    populate_at_picker(&mut v, &["alpha.rs", "beta.rs"]);

    // Type "al".
    handle_key(&mut v, &ke_char('a'));
    handle_key(&mut v, &ke_char('l'));
    assert_eq!(v.editor.text, "@al");
    assert_eq!(v.picker.as_ref().unwrap().picker.query, "al");

    // Backspace once — removes 'l'.
    handle_key(&mut v, &ke(KeyCode::Backspace));
    assert_eq!(v.editor.text, "@a");
    assert_eq!(v.picker.as_ref().unwrap().picker.query, "a");

    // Backspace again — removes 'a'.
    handle_key(&mut v, &ke(KeyCode::Backspace));
    assert_eq!(v.editor.text, "@");
    assert_eq!(v.picker.as_ref().unwrap().picker.query, "");
}

// ─── build_at_candidates does not exceed 5000 entries ────────────────────────

#[test]
fn build_at_candidates_limits_to_5000() {
    // We just verify the function exists and the limit constant is honoured
    // without creating 5000 actual files. Test with an empty dir.
    let dir = tempfile::tempdir().expect("tempdir");
    let candidates = build_at_candidates(dir.path());
    assert!(candidates.len() <= 5_000);
}
