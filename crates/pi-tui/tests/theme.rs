use pi_tui::theme::{ColorSpec, NamedColor, Theme, ThemeRegistry};

#[test]
fn registry_new_has_dark_and_light() {
    let reg = ThemeRegistry::new();
    let names = reg.names();
    assert!(names.contains(&"dark".to_string()));
    assert!(names.contains(&"light".to_string()));
    let dark = reg.get("dark").expect("dark theme");
    assert_eq!(dark.name, "dark");
    let light = reg.get("light").expect("light theme");
    assert_eq!(light.name, "light");
}

#[test]
fn install_and_get_round_trip() {
    let mut reg = ThemeRegistry::new();
    let mine = Theme {
        name: "mine".into(),
        fg: ColorSpec::Rgb { r: 1, g: 2, b: 3 },
        bg: ColorSpec::Named(NamedColor::Reset),
        muted: ColorSpec::Named(NamedColor::DarkGrey),
        accent: ColorSpec::Named(NamedColor::Cyan),
        user: ColorSpec::Named(NamedColor::Blue),
        assistant: ColorSpec::Named(NamedColor::Green),
        thinking: ColorSpec::Named(NamedColor::DarkGrey),
        tool: ColorSpec::Named(NamedColor::Yellow),
        error: ColorSpec::Named(NamedColor::Red),
    };
    reg.install(mine);
    let got = reg.get("mine").unwrap();
    match got.fg {
        ColorSpec::Rgb { r, g, b } => assert_eq!((r, g, b), (1, 2, 3)),
        _ => panic!("expected Rgb"),
    }
    assert!(reg.names().contains(&"mine".to_string()));
}

#[test]
fn colorspec_round_trips_through_serde_named() {
    let c = ColorSpec::Named(NamedColor::Cyan);
    let s = serde_json::to_string(&c).unwrap();
    // serde tags an untagged enum naturally; named is just `"cyan"`.
    assert_eq!(s, "\"cyan\"");
    let back: ColorSpec = serde_json::from_str(&s).unwrap();
    match back {
        ColorSpec::Named(NamedColor::Cyan) => {}
        _ => panic!("round trip changed variant"),
    }
}

#[test]
fn colorspec_round_trips_through_serde_rgb() {
    let c = ColorSpec::Rgb { r: 10, g: 20, b: 30 };
    let s = serde_json::to_string(&c).unwrap();
    let back: ColorSpec = serde_json::from_str(&s).unwrap();
    match back {
        ColorSpec::Rgb { r, g, b } => assert_eq!((r, g, b), (10, 20, 30)),
        _ => panic!(),
    }
}

#[test]
fn colorspec_to_crossterm_named_and_rgb() {
    use crossterm::style::Color;
    assert!(matches!(
        ColorSpec::Named(NamedColor::Red).to_crossterm(),
        Color::Red
    ));
    assert!(matches!(
        ColorSpec::Rgb { r: 1, g: 2, b: 3 }.to_crossterm(),
        Color::Rgb { r: 1, g: 2, b: 3 }
    ));
}
