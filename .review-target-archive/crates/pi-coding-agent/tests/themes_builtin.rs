//! Built-in themes copied from oh-my-pi (catppuccin-{mocha,latte},
//! dracula, nord, gruvbox-{dark,light}, tokyo-night, poimandres) ship
//! inside the binary via `include_str!`.

use pi_coding_agent::themes::{install_builtins, load_themes, BUILTIN_THEMES};
use pi_tui::ThemeRegistry;

const EXPECTED: &[&str] = &[
    "catppuccin-mocha",
    "catppuccin-latte",
    "dracula",
    "nord",
    "gruvbox-dark",
    "gruvbox-light",
    "tokyo-night",
    "poimandres",
];

#[test]
fn builtin_catalogue_lists_every_expected_theme() {
    let names: Vec<&str> = BUILTIN_THEMES.iter().map(|(n, _)| *n).collect();
    for want in EXPECTED {
        assert!(names.contains(want), "missing built-in theme {want}");
    }
}

#[test]
fn install_builtins_registers_every_theme_under_its_canonical_name() {
    let mut reg = ThemeRegistry::new();
    install_builtins(&mut reg);
    let names = reg.names();
    for want in EXPECTED {
        assert!(
            names.contains(&want.to_string()),
            "registry missing {want}; got {names:?}",
        );
    }
}

#[test]
fn load_themes_includes_builtins_alongside_dark_and_light() {
    let reg = load_themes(&[]);
    let names = reg.names();
    assert!(names.contains(&"dark".to_string()));
    assert!(names.contains(&"light".to_string()));
    assert!(names.contains(&"dracula".to_string()));
    assert!(names.contains(&"catppuccin-mocha".to_string()));
}

#[test]
fn user_theme_with_same_name_overrides_builtin() {
    use pi_tui::{ColorSpec, NamedColor, Theme};
    let dir = tempfile::tempdir().unwrap();
    let custom = Theme {
        name: "dracula".into(),
        fg: ColorSpec::Named(NamedColor::Red),
        bg: ColorSpec::Named(NamedColor::Reset),
        muted: ColorSpec::Named(NamedColor::DarkGrey),
        accent: ColorSpec::Named(NamedColor::Yellow),
        user: ColorSpec::Named(NamedColor::Cyan),
        assistant: ColorSpec::Named(NamedColor::Green),
        thinking: ColorSpec::Named(NamedColor::DarkGrey),
        tool: ColorSpec::Named(NamedColor::Blue),
        error: ColorSpec::Named(NamedColor::Red),
    };
    let json = serde_json::to_string(&custom).unwrap();
    std::fs::write(dir.path().join("dracula.json"), json).unwrap();

    let reg = load_themes(&[dir.path().to_path_buf()]);
    let got = reg.get("dracula").expect("dracula registered");
    assert_eq!(got.fg, ColorSpec::Named(NamedColor::Red));
}

#[test]
fn every_builtin_json_blob_parses_as_a_theme() {
    for (label, json) in BUILTIN_THEMES {
        let parsed: Result<pi_tui::Theme, _> = serde_json::from_str(json);
        assert!(
            parsed.is_ok(),
            "{label} failed to parse: {:?}",
            parsed.err()
        );
    }
}
