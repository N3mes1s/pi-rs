use pi_ai::auth::{AuthMethod, AuthStorage};
use serde_json::json;

#[test]
fn open_set_round_trip_persists_to_disk_and_reloads() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auth.json");

    let storage = AuthStorage::open(path.clone()).unwrap();
    storage.set(
        "anthropic",
        AuthMethod::ApiKey {
            value: "sk-ant-1".into(),
        },
    );
    storage.set(
        "claude",
        AuthMethod::OAuth {
            access_token: "at".into(),
            refresh_token: Some("rt".into()),
            expires_at: Some(123),
        },
    );

    // Reload from disk
    let reloaded = AuthStorage::open(path.clone()).unwrap();
    let got = reloaded.get("anthropic").unwrap();
    match got {
        AuthMethod::ApiKey { value } => assert_eq!(value, "sk-ant-1"),
        other => panic!("expected ApiKey, got {:?}", other),
    }

    let got = reloaded.get("claude").unwrap();
    match got {
        AuthMethod::OAuth {
            access_token,
            refresh_token,
            expires_at,
        } => {
            assert_eq!(access_token, "at");
            assert_eq!(refresh_token.as_deref(), Some("rt"));
            assert_eq!(expires_at, Some(123));
        }
        other => panic!("expected OAuth, got {:?}", other),
    }

    // Sanity: file is on disk and looks like our shape.
    let raw = std::fs::read_to_string(&path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(parsed["providers"]["anthropic"]["kind"], json!("api_key"));
    assert_eq!(parsed["providers"]["claude"]["kind"], json!("o_auth"));
}

#[test]
fn provider_names_and_remove() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auth.json");
    let storage = AuthStorage::open(path).unwrap();

    storage.set(
        "openai",
        AuthMethod::ApiKey {
            value: "sk-1".into(),
        },
    );
    storage.set(
        "fireworks",
        AuthMethod::ApiKey {
            value: "fw-1".into(),
        },
    );

    let mut names = storage.provider_names();
    names.sort();
    assert_eq!(names, vec!["fireworks".to_string(), "openai".to_string()]);

    storage.remove("openai");
    let names = storage.provider_names();
    assert_eq!(names, vec!["fireworks".to_string()]);
    assert!(storage.get("openai").is_none());
}

#[test]
fn from_env_picks_up_known_keys() {
    // Use a unique value so we don't accidentally match host environment.
    // Set ANTHROPIC_API_KEY for this test only and clear the others.
    // SAFETY: tests in a single binary share env; this test names are
    // distinct so they won't race within the same module by default.
    std::env::set_var("ANTHROPIC_API_KEY", "test-anthropic-key");
    std::env::remove_var("OPENAI_API_KEY");
    std::env::remove_var("FIREWORKS_API_KEY");

    let storage = AuthStorage::from_env();
    match storage.get("anthropic") {
        Some(AuthMethod::ApiKey { value }) => assert_eq!(value, "test-anthropic-key"),
        other => panic!("expected ApiKey for anthropic, got {:?}", other),
    }
    assert!(storage.get("openai").is_none());
    assert!(storage.get("fireworks").is_none());

    std::env::remove_var("ANTHROPIC_API_KEY");
}

#[test]
fn in_memory_does_not_write_to_disk() {
    let storage = AuthStorage::in_memory();
    storage.set(
        "anthropic",
        AuthMethod::None,
    );
    assert!(matches!(storage.get("anthropic"), Some(AuthMethod::None)));
}

#[test]
fn env_keys_constant_is_complete() {
    // Sanity check the public ENV_KEYS table.
    let names: Vec<&str> = AuthStorage::ENV_KEYS.iter().map(|(p, _)| *p).collect();
    assert!(names.contains(&"anthropic"));
    assert!(names.contains(&"openai"));
    assert!(names.contains(&"fireworks"));
}
