//! Tests for the `settings_ui` module.
//!
//! Checks that `fields` returns the expected field names + options, and that
//! `apply` correctly mutates settings or rejects invalid inputs.

use pi_agent_core::settings::{QueueMode, Settings, ThinkingSetting, Transport};
use pi_coding_agent::settings_ui::{apply, fields};

// ─── helpers ─────────────────────────────────────────────────────────────────

fn default_settings() -> Settings {
    Settings::default()
}

fn theme_list() -> Vec<String> {
    vec!["dark".into(), "light".into(), "solarized".into()]
}

// ─── fields() ────────────────────────────────────────────────────────────────

#[test]
fn fields_returns_expected_field_names() {
    let s = default_settings();
    let themes = theme_list();
    let fs = fields(&s, &themes);
    let names: Vec<&str> = fs.iter().map(|f| f.name).collect();
    assert!(
        names.contains(&"thinking"),
        "expected 'thinking' in fields, got {:?}",
        names
    );
    assert!(names.contains(&"steering_mode"));
    assert!(names.contains(&"follow_up_mode"));
    assert!(names.contains(&"transport"));
    assert!(names.contains(&"scoped_models"));
    assert!(names.contains(&"theme"));
}

#[test]
fn fields_thinking_options_are_correct() {
    let s = default_settings();
    let fs = fields(&s, &[]);
    let thinking = fs.iter().find(|f| f.name == "thinking").unwrap();
    assert_eq!(
        thinking.options,
        vec!["off", "low", "medium", "high"],
        "thinking options mismatch"
    );
}

#[test]
fn fields_steering_mode_options() {
    let s = default_settings();
    let fs = fields(&s, &[]);
    let f = fs.iter().find(|f| f.name == "steering_mode").unwrap();
    assert_eq!(f.options, vec!["one-at-a-time", "all"]);
}

#[test]
fn fields_follow_up_mode_options() {
    let s = default_settings();
    let fs = fields(&s, &[]);
    let f = fs.iter().find(|f| f.name == "follow_up_mode").unwrap();
    assert_eq!(f.options, vec!["one-at-a-time", "all"]);
}

#[test]
fn fields_transport_options() {
    let s = default_settings();
    let fs = fields(&s, &[]);
    let f = fs.iter().find(|f| f.name == "transport").unwrap();
    assert_eq!(f.options, vec!["sse", "websocket", "auto"]);
}

#[test]
fn fields_scoped_models_options() {
    let s = default_settings();
    let fs = fields(&s, &[]);
    let f = fs.iter().find(|f| f.name == "scoped_models").unwrap();
    assert_eq!(f.options, vec!["false", "true"]);
}

#[test]
fn fields_theme_options_come_from_argument() {
    let s = default_settings();
    let themes = theme_list();
    let fs = fields(&s, &themes);
    let f = fs.iter().find(|f| f.name == "theme").unwrap();
    assert_eq!(f.options, themes);
}

#[test]
fn fields_current_reflects_settings() {
    let mut s = default_settings();
    s.thinking = ThinkingSetting::Medium;
    s.scoped_models = true;
    s.theme = "light".into();
    let fs = fields(&s, &theme_list());

    let thinking = fs.iter().find(|f| f.name == "thinking").unwrap();
    assert_eq!(thinking.current, "medium");

    let scoped = fs.iter().find(|f| f.name == "scoped_models").unwrap();
    assert_eq!(scoped.current, "true");

    let theme = fs.iter().find(|f| f.name == "theme").unwrap();
    assert_eq!(theme.current, "light");
}

// ─── apply() — happy path ────────────────────────────────────────────────────

#[test]
fn apply_thinking_high_mutates_field() {
    let mut s = default_settings();
    apply(&mut s, "thinking", "high").expect("apply should succeed");
    assert_eq!(s.thinking, ThinkingSetting::High);
}

#[test]
fn apply_thinking_off() {
    let mut s = default_settings();
    s.thinking = ThinkingSetting::High;
    apply(&mut s, "thinking", "off").unwrap();
    assert_eq!(s.thinking, ThinkingSetting::Off);
}

#[test]
fn apply_thinking_low() {
    let mut s = default_settings();
    apply(&mut s, "thinking", "low").unwrap();
    assert_eq!(s.thinking, ThinkingSetting::Low);
}

#[test]
fn apply_thinking_medium() {
    let mut s = default_settings();
    apply(&mut s, "thinking", "medium").unwrap();
    assert_eq!(s.thinking, ThinkingSetting::Medium);
}

#[test]
fn apply_scoped_models_true_mutates_bool() {
    let mut s = default_settings();
    assert!(!s.scoped_models);
    apply(&mut s, "scoped_models", "true").expect("apply should succeed");
    assert!(s.scoped_models);
}

#[test]
fn apply_scoped_models_false() {
    let mut s = default_settings();
    s.scoped_models = true;
    apply(&mut s, "scoped_models", "false").unwrap();
    assert!(!s.scoped_models);
}

#[test]
fn apply_theme_dark_mutates_theme_name() {
    let mut s = default_settings();
    s.theme = "light".into();
    apply(&mut s, "theme", "dark").expect("apply should succeed");
    assert_eq!(s.theme, "dark");
}

#[test]
fn apply_theme_arbitrary_string_is_accepted() {
    let mut s = default_settings();
    apply(&mut s, "theme", "solarized").unwrap();
    assert_eq!(s.theme, "solarized");
}

#[test]
fn apply_steering_mode_all() {
    let mut s = default_settings();
    apply(&mut s, "steering_mode", "all").unwrap();
    assert_eq!(s.steering_mode, QueueMode::All);
}

#[test]
fn apply_steering_mode_one_at_a_time() {
    let mut s = default_settings();
    s.steering_mode = QueueMode::All;
    apply(&mut s, "steering_mode", "one-at-a-time").unwrap();
    assert_eq!(s.steering_mode, QueueMode::OneAtATime);
}

#[test]
fn apply_follow_up_mode_all() {
    let mut s = default_settings();
    apply(&mut s, "follow_up_mode", "all").unwrap();
    assert_eq!(s.follow_up_mode, QueueMode::All);
}

#[test]
fn apply_transport_sse() {
    let mut s = default_settings();
    apply(&mut s, "transport", "sse").unwrap();
    assert_eq!(s.transport, Transport::Sse);
}

#[test]
fn apply_transport_websocket() {
    let mut s = default_settings();
    apply(&mut s, "transport", "websocket").unwrap();
    assert_eq!(s.transport, Transport::Websocket);
}

#[test]
fn apply_transport_auto() {
    let mut s = default_settings();
    s.transport = Transport::Sse;
    apply(&mut s, "transport", "auto").unwrap();
    assert_eq!(s.transport, Transport::Auto);
}

// ─── apply() — error cases ───────────────────────────────────────────────────

#[test]
fn apply_unknown_field_returns_err() {
    let mut s = default_settings();
    let result = apply(&mut s, "unknown", "x");
    assert!(result.is_err(), "expected Err for unknown field");
    let msg = result.unwrap_err();
    assert!(
        msg.contains("unknown"),
        "error message should mention 'unknown', got: {msg}"
    );
}

#[test]
fn apply_thinking_extreme_returns_err() {
    let mut s = default_settings();
    let result = apply(&mut s, "thinking", "extreme");
    assert!(result.is_err(), "expected Err for invalid thinking value");
    // The settings must be unchanged.
    assert_eq!(s.thinking, ThinkingSetting::Off);
}

#[test]
fn apply_transport_invalid_value_returns_err() {
    let mut s = default_settings();
    let result = apply(&mut s, "transport", "grpc");
    assert!(result.is_err());
}

#[test]
fn apply_scoped_models_invalid_value_returns_err() {
    let mut s = default_settings();
    let result = apply(&mut s, "scoped_models", "yes");
    assert!(result.is_err());
    assert!(!s.scoped_models, "settings should be unchanged on error");
}

#[test]
fn apply_steering_mode_invalid_value_returns_err() {
    let mut s = default_settings();
    let result = apply(&mut s, "steering_mode", "round-robin");
    assert!(result.is_err());
}
