use pi_coding_agent::themes::{load_themes, HotThemes};

fn theme_json(name: &str) -> String {
    serde_json::json!({
        "name": name,
        "fg": "white",
        "bg": "reset",
        "muted": "darkgrey",
        "accent": "cyan",
        "user": "cyan",
        "assistant": "green",
        "thinking": "darkgrey",
        "tool": "yellow",
        "error": "red"
    })
    .to_string()
}

#[test]
fn load_themes_picks_up_json_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("custom.json"), theme_json("custom")).unwrap();
    std::fs::write(dir.path().join("other.json"), theme_json("other")).unwrap();
    // non-json should be ignored.
    std::fs::write(dir.path().join("readme.txt"), "hello").unwrap();

    let reg = load_themes(&[dir.path().to_path_buf()]);
    let names = reg.names();
    assert!(names.contains(&"custom".to_string()));
    assert!(names.contains(&"other".to_string()));
    // Defaults are still in.
    assert!(names.contains(&"dark".to_string()));
    assert!(names.contains(&"light".to_string()));
}

#[test]
fn hot_themes_can_be_constructed_in_tempdir_and_snapshot_loads_themes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("solar.json"), theme_json("solar")).unwrap();

    let hot = HotThemes::new(vec![dir.path().to_path_buf()]);
    let snap = hot.snapshot();
    assert!(snap.names().contains(&"solar".to_string()));
    assert!(snap.names().contains(&"dark".to_string()));
}

#[test]
fn load_themes_skips_invalid_json() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("broken.json"), "{not json}").unwrap();
    let reg = load_themes(&[dir.path().to_path_buf()]);
    // Defaults still present, broken theme gracefully ignored.
    assert!(reg.names().contains(&"dark".to_string()));
    assert!(!reg.names().contains(&"broken".to_string()));
}
