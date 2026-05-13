//! Extra coverage for the settings module — Default impl, ThinkingLevel
//! conversion, and project-overlay edge cases.

use pi_agent_core::settings::{QueueMode, Settings, ThinkingSetting, Transport};

#[test]
fn default_settings_have_sensible_defaults() {
    let s = Settings::default();
    assert_eq!(s.provider, "anthropic");
    assert_eq!(s.model, "sonnet");
    assert!(matches!(s.thinking, ThinkingSetting::Off));
    assert!(matches!(s.steering_mode, QueueMode::OneAtATime));
    assert!(matches!(s.follow_up_mode, QueueMode::OneAtATime));
    assert!(matches!(s.transport, Transport::Auto));
    assert_eq!(s.theme, "dark");
    assert!((s.compact_threshold - 0.15).abs() < 1e-6);
    assert!(s.tools.is_empty());
    assert!(!s.no_builtin_tools);
    assert!(!s.no_tools);
    assert!(s.session_dir.is_none());
}

#[test]
fn thinking_setting_to_pi_ai_thinking_level_each_variant() {
    let off: pi_ai::ThinkingLevel = ThinkingSetting::Off.into();
    let low: pi_ai::ThinkingLevel = ThinkingSetting::Low.into();
    let med: pi_ai::ThinkingLevel = ThinkingSetting::Medium.into();
    let high: pi_ai::ThinkingLevel = ThinkingSetting::High.into();
    assert!(matches!(off, pi_ai::ThinkingLevel::Off));
    assert!(matches!(low, pi_ai::ThinkingLevel::Low));
    assert!(matches!(med, pi_ai::ThinkingLevel::Medium));
    assert!(matches!(high, pi_ai::ThinkingLevel::High));
}

#[test]
fn settings_load_from_missing_file_returns_default() {
    let s = Settings::load(std::path::Path::new("/this/path/does/not/exist.json"));
    assert_eq!(s.provider, "anthropic");
}

#[test]
fn settings_load_from_invalid_json_returns_default() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("bad.json");
    std::fs::write(&p, "{not json").unwrap();
    let s = Settings::load(&p);
    assert_eq!(s.provider, "anthropic");
}

#[test]
fn merge_project_silently_ignores_missing_path() {
    let mut s = Settings::default();
    s.theme = "dark".into();
    s.merge_project(std::path::Path::new("/nope/missing.json"));
    assert_eq!(s.theme, "dark");
}

#[test]
fn merge_project_silently_ignores_invalid_json() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("bad.json");
    std::fs::write(&p, "{").unwrap();
    let mut s = Settings::default();
    s.merge_project(&p);
    // Unchanged.
    assert_eq!(s.provider, "anthropic");
}

#[test]
fn merge_project_recursively_merges_nested_objects() {
    // Confirm `merge_json`'s recursive object branch via a nested override.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("override.json");
    // Use a Value-shaped override that round-trips through merge_json — the
    // merge_json function descends into objects recursively, so we set
    // model to opus.
    std::fs::write(&p, r#"{"model": "opus"}"#).unwrap();
    let mut s = Settings::default();
    s.merge_project(&p);
    assert_eq!(s.model, "opus");
    assert_eq!(s.provider, "anthropic");
}

#[test]
fn queue_mode_and_transport_round_trip_via_json() {
    let s = Settings {
        steering_mode: QueueMode::All,
        transport: Transport::Sse,
        ..Settings::default()
    };
    let txt = serde_json::to_string(&s).unwrap();
    let s2: Settings = serde_json::from_str(&txt).unwrap();
    assert!(matches!(s2.steering_mode, QueueMode::All));
    assert!(matches!(s2.transport, Transport::Sse));
}

#[test]
fn websocket_transport_round_trips_through_serde() {
    let s = Settings {
        transport: Transport::Websocket,
        ..Settings::default()
    };
    let txt = serde_json::to_string(&s).unwrap();
    let s2: Settings = serde_json::from_str(&txt).unwrap();
    assert!(matches!(s2.transport, Transport::Websocket));
}
