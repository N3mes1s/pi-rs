use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pi_coding_agent::keymap::{chord_from_event, Action, ChordCode, Keymap};
use std::collections::BTreeMap;

#[test]
fn chord_from_event_lowercases_char_codes() {
    let ev = KeyEvent::new(KeyCode::Char('L'), KeyModifiers::CONTROL);
    let chord = chord_from_event(&ev);
    assert_eq!(chord.code, ChordCode::Char('l'));
}

#[test]
fn defaults_lookup_returns_bound_action() {
    let km = Keymap::defaults();
    let ev = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(km.lookup(&ev), Some(Action::Submit));

    let ev = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL);
    assert_eq!(km.lookup(&ev), Some(Action::OpenModel));

    let ev = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(km.lookup(&ev), Some(Action::Cancel));
}

#[test]
fn merge_overrides_replaces_action_binding_without_duplicating() {
    let mut km = Keymap::defaults();
    let count_open_model_before = km
        .bindings
        .iter()
        .filter(|(_, a)| **a == Action::OpenModel)
        .count();
    assert_eq!(count_open_model_before, 1);

    let mut overrides = BTreeMap::new();
    overrides.insert("open_model".to_string(), "Ctrl+M".to_string());
    km.merge_overrides(&overrides);

    // Ctrl+M now points to OpenModel.
    let ev = KeyEvent::new(KeyCode::Char('m'), KeyModifiers::CONTROL);
    assert_eq!(km.lookup(&ev), Some(Action::OpenModel));

    // Old binding (Ctrl+L) is gone — should not still resolve to OpenModel.
    let old = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL);
    assert_ne!(km.lookup(&old), Some(Action::OpenModel));

    // Only one entry maps to OpenModel — no duplicates.
    let count_open_model_after = km
        .bindings
        .iter()
        .filter(|(_, a)| **a == Action::OpenModel)
        .count();
    assert_eq!(count_open_model_after, 1);
}
