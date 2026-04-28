//! Extra coverage for keymap branches not hit by `keymap.rs` / `keymap_more.rs`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pi_coding_agent::keymap::{chord_from_event, parse_chord, ChordCode, Keymap};
use std::collections::BTreeMap;

#[test]
fn parse_chord_empty_input_returns_none() {
    assert!(parse_chord("").is_none());
    assert!(parse_chord("   ").is_none());
    assert!(parse_chord("+").is_none());
}

#[test]
fn parse_chord_unknown_modifier_returns_none() {
    assert!(parse_chord("Hyper+A").is_none());
}

#[test]
fn parse_chord_unknown_key_returns_none() {
    assert!(parse_chord("Ctrl+SuperKey").is_none());
}

#[test]
fn parse_chord_named_keys_delete_insert_pgup_pgdn_home_end() {
    for (s, expected) in [
        ("Delete", ChordCode::Delete),
        ("Del", ChordCode::Delete),
        ("Insert", ChordCode::Insert),
        ("Ins", ChordCode::Insert),
        ("PgUp", ChordCode::PageUp),
        ("PageUp", ChordCode::PageUp),
        ("PgDn", ChordCode::PageDown),
        ("PageDown", ChordCode::PageDown),
        ("Home", ChordCode::Home),
        ("End", ChordCode::End),
    ] {
        let c = parse_chord(s).expect(s);
        assert_eq!(c.code, expected, "for {s}");
    }
}

#[test]
fn keymap_bind_silently_drops_unparseable_chord() {
    let mut km = Keymap::default();
    let before = km.bindings.len();
    km.bind("NotARealChord", pi_coding_agent::keymap::Action::Submit);
    assert_eq!(
        km.bindings.len(),
        before,
        "unparseable chord must not be inserted"
    );
}

#[test]
fn load_overrides_on_missing_path_returns_err() {
    let p = std::path::Path::new("/this/file/does/not/exist/keys.json");
    let r = Keymap::load_overrides(p);
    assert!(r.is_err());
}

#[test]
fn load_overrides_on_invalid_json_returns_empty_map() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("bad.json");
    std::fs::write(&p, "not-json").unwrap();
    let r = Keymap::load_overrides(&p).expect("file exists, returns ok");
    assert!(r.is_empty(), "invalid JSON should yield an empty map");
}

#[test]
fn load_overrides_on_valid_json_returns_parsed_map() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("keys.json");
    std::fs::write(&p, r#"{"submit": "Ctrl+Y"}"#).unwrap();
    let r = Keymap::load_overrides(&p).unwrap();
    assert_eq!(r.get("submit").map(String::as_str), Some("Ctrl+Y"));
}

#[test]
fn merge_overrides_silently_drops_unknown_action_names() {
    let mut km = Keymap::defaults();
    let before = km.bindings.len();
    let mut m = BTreeMap::new();
    m.insert("not_a_real_action".to_string(), "Ctrl+X".to_string());
    km.merge_overrides(&m);
    assert_eq!(km.bindings.len(), before);
}

#[test]
fn merge_overrides_silently_drops_unparseable_chord() {
    let mut km = Keymap::defaults();
    let before = km.bindings.len();
    let mut m = BTreeMap::new();
    m.insert("submit".to_string(), "totally_bogus".to_string());
    km.merge_overrides(&m);
    // The parse_chord branch fails, so we never replace the binding.
    assert_eq!(km.bindings.len(), before);
}

#[test]
fn chord_from_event_for_home_end_pgup_pgdn_delete_insert() {
    for (kc, expected) in [
        (KeyCode::Home, ChordCode::Home),
        (KeyCode::End, ChordCode::End),
        (KeyCode::PageUp, ChordCode::PageUp),
        (KeyCode::PageDown, ChordCode::PageDown),
        (KeyCode::Delete, ChordCode::Delete),
        (KeyCode::Insert, ChordCode::Insert),
    ] {
        let c = chord_from_event(&KeyEvent::new(kc, KeyModifiers::NONE));
        assert_eq!(c.code, expected, "for keycode {:?}", kc);
    }
}

#[test]
fn chord_from_event_unhandled_keycode_falls_back_to_space_char() {
    let c = chord_from_event(&KeyEvent::new(KeyCode::Null, KeyModifiers::NONE));
    assert_eq!(c.code, ChordCode::Char(' '));
}
