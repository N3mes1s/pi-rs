//! Customisable keybindings, loaded from `~/.pi/agent/keybindings.json`.
//!
//! The schema is `{ "<action>": "<chord>" }`, e.g.
//! `{"submit": "Enter", "queue_followup": "Alt+Enter", "model": "Ctrl+L"}`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Submit,
    QueueFollowup,
    Cancel,
    Quit,
    NewLine,
    DeletePrev,
    DeleteWordPrev,
    KillLine,
    OpenModel,
    OpenSettings,
    OpenTree,
    OpenResume,
    CycleModel,
    CycleModelBack,
    ToggleThinking,
    ToggleToolOutput,
    ToggleThinkingOutput,
    PrevHistory,
    NextHistory,
    EditExternal,
}

#[derive(Debug, Clone, Default)]
pub struct Keymap {
    pub bindings: BTreeMap<Chord, Action>,
    pub extension_bindings: BTreeMap<Chord, (usize, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct Chord {
    pub modifiers: u8,
    pub code: ChordCode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub enum ChordCode {
    Char(char),
    Enter,
    Escape,
    Backspace,
    Tab,
    BackTab,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Delete,
    Insert,
    F(u8),
}

const MOD_SHIFT: u8 = 1;
const MOD_CTRL: u8 = 2;
const MOD_ALT: u8 = 4;

impl Keymap {
    pub fn defaults() -> Self {
        let mut m = Self::default();
        m.bind("Enter", Action::Submit);
        m.bind("Alt+Enter", Action::QueueFollowup);
        m.bind("Shift+Enter", Action::NewLine);
        m.bind("Escape", Action::Cancel);
        m.bind("Ctrl+C", Action::Quit);
        m.bind("Ctrl+D", Action::Quit);
        m.bind("Backspace", Action::DeletePrev);
        m.bind("Alt+Backspace", Action::DeleteWordPrev);
        m.bind("Ctrl+K", Action::KillLine);
        m.bind("Ctrl+L", Action::OpenModel);
        m.bind("Ctrl+S", Action::OpenSettings);
        m.bind("Ctrl+T", Action::OpenTree);
        m.bind("Ctrl+R", Action::OpenResume);
        m.bind("Ctrl+P", Action::CycleModel);
        m.bind("Shift+Ctrl+P", Action::CycleModelBack);
        m.bind("Shift+Tab", Action::ToggleThinking);
        m.bind("Ctrl+O", Action::ToggleToolOutput);
        m.bind("Up", Action::PrevHistory);
        m.bind("Down", Action::NextHistory);
        m.bind("Ctrl+G", Action::EditExternal);
        m
    }

    pub fn bind(&mut self, chord: &str, action: Action) {
        if let Some(c) = parse_chord(chord) {
            self.bindings.insert(c, action);
        }
    }

    pub fn lookup(&self, ev: &KeyEvent) -> Option<Action> {
        let c = chord_from_event(ev);
        self.bindings.get(&c).copied()
    }

    pub fn load_overrides(path: &Path) -> std::io::Result<BTreeMap<String, String>> {
        let txt = std::fs::read_to_string(path)?;
        let v: BTreeMap<String, String> = serde_json::from_str(&txt).unwrap_or_default();
        Ok(v)
    }

    pub fn merge_overrides(&mut self, overrides: &BTreeMap<String, String>) {
        for (action_name, chord) in overrides {
            if let Ok(action) = serde_json::from_value::<Action>(serde_json::Value::String(action_name.clone())) {
                if let Some(c) = parse_chord(chord) {
                    // remove any prior binding to this action.
                    self.bindings.retain(|_, a| *a != action);
                    self.bindings.insert(c, action);
                }
            }
        }
    }

    /// Register an extension keybinding. Returns `true` if the chord parsed
    /// successfully, `false` (and does nothing) if it did not.
    pub fn bind_extension(&mut self, chord: &str, ext_idx: usize, command_name: String) -> bool {
        if let Some(c) = parse_chord(chord) {
            self.extension_bindings.insert(c, (ext_idx, command_name));
            true
        } else {
            false
        }
    }

    /// Look up an extension binding for the given key event.
    pub fn lookup_extension(&self, ev: &KeyEvent) -> Option<(usize, String)> {
        let c = chord_from_event(ev);
        self.extension_bindings.get(&c).cloned()
    }
}

pub fn parse_chord(s: &str) -> Option<Chord> {
    let mut modifiers: u8 = 0;
    let parts: Vec<&str> = s.split('+').map(|p| p.trim()).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return None;
    }
    for p in &parts[..parts.len() - 1] {
        match p.to_ascii_lowercase().as_str() {
            "shift" => modifiers |= MOD_SHIFT,
            "ctrl" | "control" => modifiers |= MOD_CTRL,
            "alt" | "meta" | "option" => modifiers |= MOD_ALT,
            _ => return None,
        }
    }
    let key = parts.last().copied()?;
    let code = match key.to_ascii_lowercase().as_str() {
        "enter" | "return" => ChordCode::Enter,
        "escape" | "esc" => ChordCode::Escape,
        "backspace" | "bs" => ChordCode::Backspace,
        "tab" => ChordCode::Tab,
        "backtab" | "shift+tab" => ChordCode::BackTab,
        "up" => ChordCode::Up,
        "down" => ChordCode::Down,
        "left" => ChordCode::Left,
        "right" => ChordCode::Right,
        "home" => ChordCode::Home,
        "end" => ChordCode::End,
        "pgup" | "pageup" => ChordCode::PageUp,
        "pgdn" | "pagedown" => ChordCode::PageDown,
        "delete" | "del" => ChordCode::Delete,
        "insert" | "ins" => ChordCode::Insert,
        s if s.starts_with('f') => {
            let n: u8 = s[1..].parse().ok()?;
            ChordCode::F(n)
        }
        s if s.chars().count() == 1 => ChordCode::Char(s.chars().next().unwrap()),
        _ => return None,
    };
    Some(Chord { modifiers, code })
}

pub fn chord_from_event(ev: &KeyEvent) -> Chord {
    let mut modifiers: u8 = 0;
    if ev.modifiers.contains(KeyModifiers::SHIFT) {
        modifiers |= MOD_SHIFT;
    }
    if ev.modifiers.contains(KeyModifiers::CONTROL) {
        modifiers |= MOD_CTRL;
    }
    if ev.modifiers.contains(KeyModifiers::ALT) {
        modifiers |= MOD_ALT;
    }
    let code = match ev.code {
        KeyCode::Char(c) => {
            // If shift is the only modifier and the char is uppercase, keep
            // the lowercase form to make matching predictable.
            let lower = c.to_ascii_lowercase();
            ChordCode::Char(lower)
        }
        KeyCode::Enter => ChordCode::Enter,
        KeyCode::Esc => ChordCode::Escape,
        KeyCode::Backspace => ChordCode::Backspace,
        KeyCode::Tab => ChordCode::Tab,
        KeyCode::BackTab => ChordCode::BackTab,
        KeyCode::Up => ChordCode::Up,
        KeyCode::Down => ChordCode::Down,
        KeyCode::Left => ChordCode::Left,
        KeyCode::Right => ChordCode::Right,
        KeyCode::Home => ChordCode::Home,
        KeyCode::End => ChordCode::End,
        KeyCode::PageUp => ChordCode::PageUp,
        KeyCode::PageDown => ChordCode::PageDown,
        KeyCode::Delete => ChordCode::Delete,
        KeyCode::Insert => ChordCode::Insert,
        KeyCode::F(n) => ChordCode::F(n),
        _ => ChordCode::Char(' '),
    };
    Chord { modifiers, code }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_enter() {
        let c = parse_chord("Enter").expect("Enter should parse");
        assert_eq!(c.modifiers, 0);
        assert_eq!(c.code, ChordCode::Enter);
    }

    #[test]
    fn parses_ctrl_l() {
        let c = parse_chord("Ctrl+L").expect("Ctrl+L should parse");
        assert_eq!(c.modifiers, MOD_CTRL);
        assert_eq!(c.code, ChordCode::Char('l'));
    }

    #[test]
    fn parses_shift_ctrl_p() {
        let c = parse_chord("Shift+Ctrl+P").expect("Shift+Ctrl+P should parse");
        assert_eq!(c.modifiers & MOD_SHIFT, MOD_SHIFT);
        assert_eq!(c.modifiers & MOD_CTRL, MOD_CTRL);
        assert_eq!(c.code, ChordCode::Char('p'));
    }

    #[test]
    fn parses_function_key() {
        let c = parse_chord("F5").expect("F5 should parse");
        assert_eq!(c.modifiers, 0);
        assert_eq!(c.code, ChordCode::F(5));
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(parse_chord("Ctrl+Bogus").is_none());
    }
}
