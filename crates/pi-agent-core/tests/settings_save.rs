//! Coverage for `Settings::save` — the persistence path added alongside
//! `/scoped-models`. Round-trips every field including `scoped_models`,
//! creates parent directories on demand, and surfaces an error on an
//! unwritable target.

use pi_agent_core::settings::{QueueMode, Settings, ThinkingSetting, Transport};
use std::path::PathBuf;

#[test]
fn save_then_load_round_trips_all_fields() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");

    let mut s = Settings::default();
    s.provider = "openai".into();
    s.model = "gpt-4o".into();
    s.thinking = ThinkingSetting::High;
    s.steering_mode = QueueMode::All;
    s.follow_up_mode = QueueMode::All;
    s.transport = Transport::Websocket;
    s.theme = "light".into();
    s.compact_threshold = 0.42;
    s.tools = vec!["read".into(), "write".into()];
    s.no_builtin_tools = true;
    s.no_tools = false;
    s.session_dir = Some(PathBuf::from("/tmp/sessions"));
    s.scoped_models = true;

    s.save(&path).expect("save ok");
    assert!(path.is_file(), "save must create the file");

    let loaded = Settings::load(&path);
    assert_eq!(loaded.provider, "openai");
    assert_eq!(loaded.model, "gpt-4o");
    assert_eq!(loaded.thinking, ThinkingSetting::High);
    assert_eq!(loaded.steering_mode, QueueMode::All);
    assert_eq!(loaded.follow_up_mode, QueueMode::All);
    assert_eq!(loaded.transport, Transport::Websocket);
    assert_eq!(loaded.theme, "light");
    assert!((loaded.compact_threshold - 0.42).abs() < f32::EPSILON);
    assert_eq!(loaded.tools, vec!["read".to_string(), "write".to_string()]);
    assert!(loaded.no_builtin_tools);
    assert!(!loaded.no_tools);
    assert_eq!(loaded.session_dir, Some(PathBuf::from("/tmp/sessions")));
    assert!(loaded.scoped_models);
}

#[test]
fn save_creates_missing_parent_directories() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("a").join("b").join("c").join("settings.json");
    assert!(!nested.parent().unwrap().exists());

    Settings::default().save(&nested).expect("save ok");
    assert!(nested.is_file());
    assert!(nested.parent().unwrap().is_dir());
}

#[test]
fn save_returns_err_on_unwritable_path() {
    // `/proc/self/cannot-create-here/settings.json` lives below a
    // pseudo-fs node that does not accept new directories — both
    // `create_dir_all` and `write` will fail. The exact errno varies
    // but the call must surface an Err and not panic.
    let bad = std::path::PathBuf::from("/proc/1/cant/be/written/settings.json");
    let r = Settings::default().save(&bad);
    assert!(r.is_err(), "expected error for unwritable path, got {r:?}");
}
