//! Extra coverage for the themes module — `read_theme` on a missing file,
//! invalid JSON skipped, and `HotThemes::new` over a tempdir.

use pi_coding_agent::themes::{load_themes, read_theme, HotThemes};
use std::path::PathBuf;

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
fn read_theme_on_missing_path_returns_none() {
    let p = PathBuf::from("/this/file/should/not/exist/at/all.json");
    assert!(read_theme(&p).is_none());
}

#[test]
fn read_theme_on_garbage_returns_none() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("bad.json");
    std::fs::write(&p, "{not json}").unwrap();
    assert!(read_theme(&p).is_none());
}

#[test]
fn load_themes_with_only_invalid_files_keeps_defaults_only() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.json"), "{").unwrap();
    std::fs::write(dir.path().join("b.json"), "[]").unwrap();
    let reg = load_themes(&[dir.path().to_path_buf()]);
    assert!(reg.names().contains(&"dark".to_string()));
    assert!(reg.names().contains(&"light".to_string()));
}

#[test]
fn load_themes_skips_dirs_that_do_not_exist() {
    let reg = load_themes(&[PathBuf::from("/this/dir/never/exists")]);
    // Defaults are still present.
    assert!(reg.names().contains(&"dark".to_string()));
}

#[test]
fn hot_themes_constructed_over_a_missing_directory_still_returns_defaults() {
    let dirs = vec![PathBuf::from("/this/never/exists")];
    let hot = HotThemes::new(dirs);
    let snap = hot.snapshot();
    assert!(snap.names().contains(&"dark".to_string()));
    assert!(snap.names().contains(&"light".to_string()));
}

#[test]
fn hot_themes_picks_up_existing_files_at_construction_time() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("warm.json"), theme_json("warm")).unwrap();
    let hot = HotThemes::new(vec![dir.path().to_path_buf()]);
    let snap = hot.snapshot();
    assert!(snap.names().contains(&"warm".to_string()));
}
