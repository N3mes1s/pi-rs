//! Unit tests for `!command` / `!!command` (bang) detection in the TUI editor.
//!
//! These tests drive `handle_key` with synthesised `KeyEvent`s and verify that
//! the bang detection fires before slash-command or plain-submit routing. No
//! shell processes are spawned — we only test the detection logic itself.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pi_agent_core::settings::ThinkingSetting;
use pi_coding_agent::keymap::Keymap;
use pi_coding_agent::modes::interactive::{handle_key, KeyOutcome, View};

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

/// Type every character in `s` into `view` via `handle_key`.
fn type_str(view: &mut View, s: &str) {
    for c in s.chars() {
        handle_key(view, &ke_char(c));
    }
}

/// Press Enter (plain, no modifiers).
fn press_enter(view: &mut View) -> KeyOutcome {
    handle_key(view, &ke(KeyCode::Enter))
}

// ─── tests ──────────────────────────────────────────────────────────────────

/// Typing `!echo hi` then Enter returns `KeyOutcome::Bang { command: "echo hi", silent: false }`.
#[test]
fn bang_single_is_detected_as_not_silent() {
    let mut v = fresh_view();
    type_str(&mut v, "!echo hi");
    let outcome = press_enter(&mut v);
    match outcome {
        KeyOutcome::Bang { command, silent } => {
            assert_eq!(command, "echo hi");
            assert!(!silent, "single ! should have silent=false");
        }
        other => panic!("expected Bang, got {:?}", other),
    }
}

/// Typing `!!echo hi` then Enter returns `KeyOutcome::Bang { command: "echo hi", silent: true }`.
#[test]
fn bang_double_is_detected_as_silent() {
    let mut v = fresh_view();
    type_str(&mut v, "!!echo hi");
    let outcome = press_enter(&mut v);
    match outcome {
        KeyOutcome::Bang { command, silent } => {
            assert_eq!(command, "echo hi");
            assert!(silent, "!! should have silent=true");
        }
        other => panic!("expected Bang, got {:?}", other),
    }
}

/// Leading whitespace is still recognised: `   !ls` → Bang.
#[test]
fn bang_with_leading_whitespace_is_recognised() {
    let mut v = fresh_view();
    // We must type spaces — the view's editor treats them as ordinary chars.
    type_str(&mut v, "   !ls");
    let outcome = press_enter(&mut v);
    match outcome {
        KeyOutcome::Bang { command, silent } => {
            assert_eq!(command, "ls");
            assert!(!silent);
        }
        other => panic!("expected Bang for '   !ls', got {:?}", other),
    }
}

/// After a bang submission the editor buffer must be empty and cursor at 0.
#[test]
fn editor_is_cleared_after_bang_submission() {
    let mut v = fresh_view();
    type_str(&mut v, "!date");
    let outcome = press_enter(&mut v);
    assert!(
        matches!(outcome, KeyOutcome::Bang { .. }),
        "expected Bang outcome"
    );
    assert!(
        v.editor.text.is_empty(),
        "editor.text should be empty after bang submit; got {:?}",
        v.editor.text
    );
    assert_eq!(v.editor.cursor, 0, "cursor should be 0 after bang submit");
}

/// `/help` is still routed as `SlashCommand("help", …)`, not as Bang.
#[test]
fn slash_command_is_not_intercepted_as_bang() {
    let mut v = fresh_view();
    type_str(&mut v, "/help");
    let outcome = press_enter(&mut v);
    match outcome {
        KeyOutcome::SlashCommand(name, _args) => {
            assert_eq!(name, "help");
        }
        other => panic!("expected SlashCommand for '/help', got {:?}", other),
    }
}

/// Plain text like `hello` is still routed as `Submit("hello")`, not Bang.
#[test]
fn plain_text_is_not_intercepted_as_bang() {
    let mut v = fresh_view();
    type_str(&mut v, "hello");
    let outcome = press_enter(&mut v);
    match outcome {
        KeyOutcome::Submit(text) => {
            assert_eq!(text, "hello");
        }
        other => panic!("expected Submit for 'hello', got {:?}", other),
    }
}

/// A single bare `!` (no command text) is still a Bang with an empty command string.
#[test]
fn bare_bang_has_empty_command() {
    let mut v = fresh_view();
    type_str(&mut v, "!");
    let outcome = press_enter(&mut v);
    match outcome {
        KeyOutcome::Bang { command, silent } => {
            assert_eq!(command, "");
            assert!(!silent);
        }
        other => panic!("expected Bang for '!', got {:?}", other),
    }
}

/// `!!` (bare double bang) is silent with empty command.
#[test]
fn bare_double_bang_has_empty_command_and_silent() {
    let mut v = fresh_view();
    type_str(&mut v, "!!");
    let outcome = press_enter(&mut v);
    match outcome {
        KeyOutcome::Bang { command, silent } => {
            assert_eq!(command, "");
            assert!(silent);
        }
        other => panic!("expected Bang for '!!', got {:?}", other),
    }
}
