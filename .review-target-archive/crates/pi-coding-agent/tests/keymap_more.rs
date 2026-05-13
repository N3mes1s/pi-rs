//! Additional coverage for keymap parsing and event-to-chord mapping.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pi_coding_agent::keymap::{chord_from_event, parse_chord, Action, ChordCode, Keymap};

#[test]
fn defaults_includes_submit_and_quit_bindings() {
    let km = Keymap::defaults();
    let actions: Vec<Action> = km.bindings.values().copied().collect();
    assert!(
        actions.contains(&Action::Submit),
        "defaults should bind Submit"
    );
    assert!(actions.contains(&Action::Quit), "defaults should bind Quit");
}

#[test]
fn chord_from_event_maps_tab_and_backtab() {
    let tab = chord_from_event(&KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(tab.code, ChordCode::Tab);
    let back = chord_from_event(&KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE));
    assert_eq!(back.code, ChordCode::BackTab);
}

#[test]
fn chord_from_event_maps_function_keys() {
    for n in 1u8..=12 {
        let c = chord_from_event(&KeyEvent::new(KeyCode::F(n), KeyModifiers::NONE));
        assert_eq!(c.code, ChordCode::F(n));
    }
}

#[test]
fn chord_from_event_maps_arrow_keys() {
    let cases = [
        (KeyCode::Up, ChordCode::Up),
        (KeyCode::Down, ChordCode::Down),
        (KeyCode::Left, ChordCode::Left),
        (KeyCode::Right, ChordCode::Right),
    ];
    for (kc, expected) in cases {
        let c = chord_from_event(&KeyEvent::new(kc, KeyModifiers::NONE));
        assert_eq!(c.code, expected, "for keycode {:?}", kc);
    }
}

#[test]
fn parse_chord_alt_shift_backspace_combines_modifiers() {
    let c = parse_chord("Alt+Shift+Backspace").expect("parses");
    assert_eq!(c.code, ChordCode::Backspace);
    // Shift = 1, Ctrl = 2, Alt = 4 in the implementation. Shift+Alt = 5.
    assert_eq!(c.modifiers & 1, 1, "shift bit set");
    assert_eq!(c.modifiers & 4, 4, "alt bit set");
    assert_eq!(c.modifiers & 2, 0, "ctrl bit not set");
}
