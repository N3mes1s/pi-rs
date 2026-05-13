use crossterm::style::Color;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Theme {
    pub name: String,
    pub fg: ColorSpec,
    pub bg: ColorSpec,
    pub muted: ColorSpec,
    pub accent: ColorSpec,
    pub user: ColorSpec,
    pub assistant: ColorSpec,
    pub thinking: ColorSpec,
    pub tool: ColorSpec,
    pub error: ColorSpec,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ColorSpec {
    Named(NamedColor),
    Rgb { r: u8, g: u8, b: u8 },
}

impl ColorSpec {
    pub fn to_crossterm(self) -> Color {
        match self {
            ColorSpec::Named(NamedColor::Reset) => Color::Reset,
            ColorSpec::Named(NamedColor::Black) => Color::Black,
            ColorSpec::Named(NamedColor::DarkGrey) => Color::DarkGrey,
            ColorSpec::Named(NamedColor::Red) => Color::Red,
            ColorSpec::Named(NamedColor::Green) => Color::Green,
            ColorSpec::Named(NamedColor::Yellow) => Color::Yellow,
            ColorSpec::Named(NamedColor::Blue) => Color::Blue,
            ColorSpec::Named(NamedColor::Magenta) => Color::Magenta,
            ColorSpec::Named(NamedColor::Cyan) => Color::Cyan,
            ColorSpec::Named(NamedColor::White) => Color::White,
            ColorSpec::Rgb { r, g, b } => Color::Rgb { r, g, b },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NamedColor {
    Reset,
    Black,
    DarkGrey,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
}

#[derive(Debug, Default, Clone)]
pub struct ThemeRegistry {
    themes: HashMap<String, Theme>,
}

impl ThemeRegistry {
    pub fn new() -> Self {
        let mut me = Self::default();
        me.themes.insert("dark".into(), dark_theme());
        me.themes.insert("light".into(), light_theme());
        me
    }

    pub fn get(&self, name: &str) -> Option<&Theme> {
        self.themes.get(name)
    }

    pub fn install(&mut self, theme: Theme) {
        self.themes.insert(theme.name.clone(), theme);
    }

    pub fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.themes.keys().cloned().collect();
        v.sort();
        v
    }
}

fn dark_theme() -> Theme {
    Theme {
        name: "dark".into(),
        fg: ColorSpec::Named(NamedColor::White),
        bg: ColorSpec::Named(NamedColor::Reset),
        muted: ColorSpec::Named(NamedColor::DarkGrey),
        accent: ColorSpec::Named(NamedColor::Cyan),
        user: ColorSpec::Named(NamedColor::Cyan),
        assistant: ColorSpec::Named(NamedColor::Green),
        thinking: ColorSpec::Named(NamedColor::DarkGrey),
        tool: ColorSpec::Named(NamedColor::Yellow),
        error: ColorSpec::Named(NamedColor::Red),
    }
}

fn light_theme() -> Theme {
    Theme {
        name: "light".into(),
        fg: ColorSpec::Named(NamedColor::Black),
        bg: ColorSpec::Named(NamedColor::Reset),
        muted: ColorSpec::Named(NamedColor::DarkGrey),
        accent: ColorSpec::Named(NamedColor::Blue),
        user: ColorSpec::Named(NamedColor::Blue),
        assistant: ColorSpec::Named(NamedColor::Green),
        thinking: ColorSpec::Named(NamedColor::DarkGrey),
        tool: ColorSpec::Named(NamedColor::Magenta),
        error: ColorSpec::Named(NamedColor::Red),
    }
}
