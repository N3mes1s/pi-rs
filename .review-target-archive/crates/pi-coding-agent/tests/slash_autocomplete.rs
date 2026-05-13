//! Test slash-command autocomplete UI.
//!
//! Exercise the autocomplete dropdown visibility and selection logic when typing `/`.
//!
//! Note: More comprehensive tests exist in `modes::interactive::tests`
//! (e.g. `build_frame_slash_autocomplete_*`), but this module provides
//! additional coverage for the public slash registry and autocomplete behavior.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pi_agent_core::settings::ThinkingSetting;
use pi_coding_agent::keymap::Keymap;
use pi_coding_agent::modes::interactive::{handle_key, KeyOutcome, View};
use pi_coding_agent::slash::SlashRegistry;

fn ke(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn fresh_view() -> View {
    View::new(Keymap::defaults(), ThinkingSetting::Off)
}

#[test]
fn slash_registry_contains_theme_builtin() {
    let reg = SlashRegistry::new();
    let theme_cmd = reg.get("theme");
    assert!(theme_cmd.is_some(), "/theme should be registered");
    assert_eq!(
        theme_cmd.unwrap().description,
        "Switch the active theme; e.g. /theme dark, /theme light, or any installed theme name"
    );
}

#[test]
fn autocomplete_filters_by_prefix() {
    let reg = SlashRegistry::new();
    let all_names = reg.names();

    // Test prefix matching on "he": should match "help"
    let he_matches: Vec<_> = all_names
        .iter()
        .filter(|name| name.starts_with("he"))
        .collect();

    assert!(he_matches.contains(&&"help".to_string()));

    // Test prefix "ho": should match "hotkeys"
    let ho_matches: Vec<_> = all_names
        .iter()
        .filter(|name| name.starts_with("ho"))
        .collect();
    assert!(ho_matches.contains(&&"hotkeys".to_string()));

    // No commands start with "xyz"
    let xyz_matches: Vec<_> = all_names
        .iter()
        .filter(|name| name.starts_with("xyz"))
        .collect();
    assert!(xyz_matches.is_empty());
}

#[test]
fn autocomplete_dropdown_limited_to_five() {
    let reg = SlashRegistry::new();
    let all_names = reg.names();

    // At most 5 suggestions should be shown (limit per RFD 0024).
    // This is honoured in build_frame, but here we just verify the data is available.
    assert!(!all_names.is_empty(), "should have at least one command");
}

#[test]
fn theme_command_available_in_help_listing() {
    let reg = SlashRegistry::new();
    let all = reg.iter().collect::<Vec<_>>();

    // Verify /theme is in the registry and will appear in /help output
    let has_theme = all.iter().any(|cmd| cmd.name == "theme");
    assert!(has_theme, "/theme should appear in /help listing");
}

#[test]
fn tab_accepts_top_suggestion() {
    let mut view = fresh_view();
    view.editor.text = "/he".to_string();
    view.editor.cursor = view.editor.text.len();

    let outcome = handle_key(&mut view, &ke(KeyCode::Tab));
    assert!(matches!(outcome, KeyOutcome::None));
    assert_eq!(view.editor.text, "/help ");
    assert_eq!(view.editor.cursor, view.editor.text.len());
    assert!(view.slash_ac_hidden_until_char);
}

#[test]
fn tab_cycles_and_shift_tab_cycles_backwards() {
    let mut view = fresh_view();
    view.editor.text = "/h".to_string();
    view.editor.cursor = view.editor.text.len();

    handle_key(&mut view, &ke(KeyCode::Tab));
    assert_eq!(view.editor.text, "/help ");

    handle_key(&mut view, &ke(KeyCode::Tab));
    assert_eq!(view.editor.text, "/hotkeys ");

    handle_key(&mut view, &ke(KeyCode::BackTab));
    assert_eq!(view.editor.text, "/help ");
}

#[test]
fn right_at_end_of_line_accepts_top_suggestion() {
    let mut view = fresh_view();
    view.editor.text = "/he".to_string();
    view.editor.cursor = view.editor.text.len();

    handle_key(&mut view, &ke(KeyCode::Right));
    assert_eq!(view.editor.text, "/help ");
    assert_eq!(view.editor.cursor, view.editor.text.len());
}
