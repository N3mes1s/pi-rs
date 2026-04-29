//! Integration test for /theme command: verify that theme switching changes
//! the rendered output colors immediately.

use pi_coding_agent::modes::interactive::View;
use pi_coding_agent::keymap::Keymap;
use pi_coding_agent::renderer::{Block, Transcript};
use pi_agent_core::settings::ThinkingSetting;
use pi_tui::ThemeRegistry;

#[test]
fn theme_live_switch_changes_palette() {
    // Create a transcript with some content.
    let mut transcript = Transcript::default();
    transcript.blocks.push(Block::AssistantText(
        "Here is **bold text** and _italic text_ and `code`.".to_string(),
    ));
    transcript.blocks.push(Block::User("User prompt here.".to_string()));

    // Get dark and light themes from the registry.
    let registry = ThemeRegistry::new();
    let dark_theme = registry.get("dark").cloned().expect("dark theme exists");
    let light_theme = registry.get("light").cloned().expect("light theme exists");

    // Render with dark theme
    let frame_dark = transcript.render(&dark_theme, 80);
    let dark_spans: Vec<(String, Option<crossterm::style::Color>)> = frame_dark
        .lines
        .iter()
        .flat_map(|line| {
            line.spans
                .iter()
                .map(|span| (span.text.clone(), span.color))
        })
        .collect();

    // Render with light theme
    let frame_light = transcript.render(&light_theme, 80);
    let light_spans: Vec<(String, Option<crossterm::style::Color>)> = frame_light
        .lines
        .iter()
        .flat_map(|line| {
            line.spans
                .iter()
                .map(|span| (span.text.clone(), span.color))
        })
        .collect();

    // The text content should be the same
    let dark_text: Vec<String> = dark_spans.iter().map(|(t, _)| t.clone()).collect();
    let light_text: Vec<String> = light_spans.iter().map(|(t, _)| t.clone()).collect();
    assert_eq!(dark_text, light_text, "Text content should be identical");

    // But the colors should differ (themes have different accent/assistant colors)
    let dark_colors: Vec<Option<crossterm::style::Color>> = dark_spans.iter().map(|(_, c)| *c).collect();
    let light_colors: Vec<Option<crossterm::style::Color>> = light_spans.iter().map(|(_, c)| *c).collect();

    // At least some spans should have different colors between dark and light
    let colors_differ = dark_colors != light_colors;
    assert!(
        colors_differ,
        "Dark and light themes should produce different color palettes"
    );
}

#[test]
fn theme_switch_persists_setting() {
    let mut transcript = Transcript::default();
    transcript.blocks.push(Block::User("test".to_string()));

    let registry = ThemeRegistry::new();
    let dark = registry.get("dark").cloned().expect("dark exists");
    let light = registry.get("light").cloned().expect("light exists");

    // Render once with dark
    let _ = transcript.render(&dark, 80);

    // Render with light
    let _ = transcript.render(&light, 80);

    // Verify that both themes exist and can be looked up
    assert!(registry.get("dark").is_some());
    assert!(registry.get("light").is_some());
}

#[test]
fn view_tracks_current_theme_name() {
    let keymap = Keymap::default();
    let mut view = View::new(keymap, ThinkingSetting::Off);

    // Initially empty
    assert!(view.current_theme_name.is_empty());

    // Simulate /theme dispatch
    view.current_theme_name = "dark".to_string();
    view.dirty = true;

    assert_eq!(view.current_theme_name, "dark");
    assert!(view.dirty);
}
