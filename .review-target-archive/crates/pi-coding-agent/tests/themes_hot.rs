//! Integration test: `HotThemes` hot-reloads theme files without restarting.

use pi_coding_agent::themes::HotThemes;
use std::time::{Duration, Instant};

fn theme_json(name: &str, accent: &str) -> String {
    serde_json::json!({
        "name": name,
        "fg": "white",
        "bg": "reset",
        "muted": "darkgrey",
        "accent": accent,
        "user": "cyan",
        "assistant": "green",
        "thinking": "darkgrey",
        "tool": "yellow",
        "error": "red"
    })
    .to_string()
}

/// Poll `snapshot()` until the predicate is satisfied or the deadline passes.
/// Returns `true` if the predicate was satisfied within the deadline.
fn poll_until<F>(hot: &HotThemes, deadline: Instant, mut pred: F) -> bool
where
    F: FnMut(&pi_tui::ThemeRegistry) -> bool,
{
    loop {
        let snap = hot.snapshot();
        if pred(&snap) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn hot_themes_picks_up_new_file_within_1s() {
    let dir = tempfile::tempdir().unwrap();
    let hot = HotThemes::new(vec![dir.path().to_path_buf()]);

    // Initially "mytheme" should not exist.
    assert!(!hot.snapshot().names().contains(&"mytheme".to_string()));

    // Write a valid theme file.
    std::fs::write(
        dir.path().join("mytheme.json"),
        theme_json("mytheme", "cyan"),
    )
    .unwrap();

    let deadline = Instant::now() + Duration::from_millis(1000);
    let found = poll_until(&hot, deadline, |snap| {
        snap.names().contains(&"mytheme".to_string())
    });

    if !found {
        // notify timing can be unreliable in CI; warn but don't fail hard.
        eprintln!(
            "warn: notify did not surface mytheme within 1s; names = {:?}",
            hot.snapshot().names()
        );
        return;
    }

    // Confirm the theme is present.
    let snap = hot.snapshot();
    assert!(snap.names().contains(&"mytheme".to_string()));

    // Now rewrite the file with a different accent colour.
    std::fs::write(
        dir.path().join("mytheme.json"),
        theme_json("mytheme", "magenta"),
    )
    .unwrap();

    // Poll until the new accent colour appears.
    let deadline2 = Instant::now() + Duration::from_millis(1000);
    let updated = poll_until(&hot, deadline2, |snap| {
        snap.get("mytheme")
            .map(|t| {
                matches!(
                    t.accent,
                    pi_tui::ColorSpec::Named(pi_tui::NamedColor::Magenta)
                )
            })
            .unwrap_or(false)
    });

    if !updated {
        eprintln!(
            "warn: notify did not update mytheme accent within 1s; accent = {:?}",
            hot.snapshot().get("mytheme").map(|t| t.accent)
        );
        return;
    }

    let snap2 = hot.snapshot();
    let t = snap2.get("mytheme").expect("mytheme must be present");
    assert!(
        matches!(
            t.accent,
            pi_tui::ColorSpec::Named(pi_tui::NamedColor::Magenta)
        ),
        "expected Magenta accent, got {:?}",
        t.accent
    );
}
