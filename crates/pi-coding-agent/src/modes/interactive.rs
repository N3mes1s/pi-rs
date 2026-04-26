//! Interactive mode.
//!
//! When stdin/stdout are both TTYs, this enters a raw-mode TUI built on top
//! of `pi_tui::DiffRenderer`, `pi_tui::Editor`, `pi_coding_agent::renderer::Transcript`,
//! and `pi_coding_agent::keymap::Keymap`. When either is not a TTY (pipes,
//! redirects, CI), it falls back to the simpler line-based REPL preserved in
//! [`run_line_based`].

use crossterm::cursor::{Hide, Show};
use crossterm::event::{Event as CtEvent, EventStream, KeyCode, KeyEvent, KeyModifiers};
use crossterm::style::{Color, ResetColor, SetForegroundColor};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{cursor, execute, queue, style::Print};
use futures::StreamExt;
use pi_agent_core::{settings::ThinkingSetting, AgentEvent, AgentEventKind};
use pi_tui::{DiffRenderer, Editor, Frame, Line, Span, Theme};
use std::io::{IsTerminal, Write};
use std::time::{Duration, Instant};

use crate::keymap::{chord_from_event, Action, Chord, ChordCode, Keymap};
use crate::modes::build_session;
use crate::picker::{PickItem, Picker};
use crate::renderer::Transcript;
use crate::slash::{self, SlashKind, SlashRegistry};
use crate::startup::Startup;

/// Entry point. Picks raw-TUI or line-based depending on TTY state.
pub async fn run(startup: Startup) -> anyhow::Result<()> {
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        run_tui(startup).await
    } else {
        run_line_based(startup).await
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Raw-mode TUI
// ─────────────────────────────────────────────────────────────────────────────

/// RAII guard: restores terminal state on drop. Always fires, even on panic.
struct RawGuard;

impl RawGuard {
    fn enter() -> std::io::Result<Self> {
        enable_raw_mode()?;
        let mut out = std::io::stdout();
        execute!(out, EnterAlternateScreen, Hide)?;
        Ok(RawGuard)
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        let mut out = std::io::stdout();
        let _ = execute!(out, Show, LeaveAlternateScreen, ResetColor);
        let _ = disable_raw_mode();
    }
}

/// Bottom-half input mode.
#[allow(dead_code)]
#[derive(Debug)]
enum InputMode {
    Editor,
    Picker(PickerOverlay),
}

/// What happens after a picker resolves.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
enum PickerKind {
    Resume,
    Model,
    Tree,
    Fork,
    Clone,
}

#[derive(Debug)]
pub(crate) struct PickerOverlay {
    kind: PickerKind,
    picker: Picker<String>,
    title: String,
}

/// Pure view-state container — no I/O, no terminal — so it can be unit-tested
/// without a real TTY. Holds the transcript, keymap, optional picker, and
/// editor history. The TUI loop owns one of these and mutates it in response
/// to events; on each tick it asks for a render.
pub(crate) struct View {
    pub transcript: Transcript,
    pub keymap: Keymap,
    pub picker: Option<PickerOverlay>,
    pub editor: Editor,
    pub history: Vec<String>,
    pub history_idx: Option<usize>,
    pub queued_count: usize,
    pub thinking: ThinkingSetting,
    /// Time of the previous unconfirmed Quit action (Ctrl+C). If a second
    /// Quit lands within ~1s, we exit; otherwise we just clear.
    pub last_quit: Option<Instant>,
    pub turn_in_progress: bool,
    pub dirty: bool,
}

impl View {
    pub fn new(keymap: Keymap, thinking: ThinkingSetting) -> Self {
        Self {
            transcript: Transcript::default(),
            keymap,
            picker: None,
            editor: Editor::new(),
            history: Vec::new(),
            history_idx: None,
            queued_count: 0,
            thinking,
            last_quit: None,
            turn_in_progress: false,
            dirty: true,
        }
    }
}

/// Outcome of `handle_key` — tells the outer loop what (if anything) to do
/// in addition to local state mutations.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum KeyOutcome {
    None,
    Submit(String),
    Queue(String),
    SlashCommand(String, String),
    Quit,
    Abort,
    /// Cycle to next model in registry.
    CycleModel,
    /// Cycle thinking level.
    CycleThinking,
    /// Spawn external editor.
    EditExternal,
}

/// Pure key handler — no I/O. Returns what the outer loop must drive.
/// Mutates `view` (editor buffer, picker query, history index, transcript
/// collapse flags, quit-confirm timer).
pub(crate) fn handle_key(view: &mut View, ev: &KeyEvent) -> KeyOutcome {
    view.dirty = true;

    // If a picker is open, route everything through it first.
    if let Some(overlay) = view.picker.as_mut() {
        match ev.code {
            KeyCode::Esc => {
                view.picker = None;
                return KeyOutcome::None;
            }
            KeyCode::Enter => {
                let chosen = overlay.picker.selected_value();
                let kind = overlay.kind;
                view.picker = None;
                if let Some(value) = chosen {
                    return picker_outcome(kind, value);
                }
                return KeyOutcome::None;
            }
            KeyCode::Up => {
                overlay.picker.move_up();
                return KeyOutcome::None;
            }
            KeyCode::Down => {
                overlay.picker.move_down();
                return KeyOutcome::None;
            }
            KeyCode::Backspace => {
                overlay.picker.pop_query();
                return KeyOutcome::None;
            }
            KeyCode::Char(c) if !ev.modifiers.contains(KeyModifiers::CONTROL) => {
                overlay.picker.push_query(c);
                return KeyOutcome::None;
            }
            _ => return KeyOutcome::None,
        }
    }

    // ─── editor mode ───
    let chord = chord_from_event(ev);

    // Quit-confirm: Ctrl+C.
    if ev.code == KeyCode::Char('c') && ev.modifiers.contains(KeyModifiers::CONTROL) {
        let now = Instant::now();
        if let Some(prev) = view.last_quit {
            if now.duration_since(prev) < Duration::from_secs(1) {
                return KeyOutcome::Quit;
            }
        }
        view.last_quit = Some(now);
        view.editor.clear();
        return KeyOutcome::None;
    }

    // Ctrl+D: quit when buffer is empty.
    if ev.code == KeyCode::Char('d') && ev.modifiers.contains(KeyModifiers::CONTROL) {
        if view.editor.text.is_empty() {
            return KeyOutcome::Quit;
        }
        return KeyOutcome::None;
    }

    // Look up an action via the keymap.
    if let Some(action) = view.keymap.lookup(ev) {
        match action {
            Action::Submit => {
                let buf = std::mem::take(&mut view.editor.text);
                view.editor.cursor = 0;
                view.history_idx = None;
                if buf.trim().is_empty() {
                    return KeyOutcome::None;
                }
                view.history.push(buf.clone());
                view.last_quit = None;
                if let Some((name, args)) = slash::parse(&buf) {
                    return KeyOutcome::SlashCommand(name, args);
                }
                return KeyOutcome::Submit(buf);
            }
            Action::QueueFollowup => {
                let buf = std::mem::take(&mut view.editor.text);
                view.editor.cursor = 0;
                if buf.trim().is_empty() {
                    return KeyOutcome::None;
                }
                view.queued_count += 1;
                return KeyOutcome::Queue(buf);
            }
            Action::NewLine => {
                view.editor.insert('\n');
                return KeyOutcome::None;
            }
            Action::Cancel => {
                view.last_quit = None;
                if view.turn_in_progress {
                    return KeyOutcome::Abort;
                }
                return KeyOutcome::None;
            }
            Action::Quit => {
                // Ctrl+D path is already handled above. If a different
                // Quit binding fires, treat it as direct-quit.
                return KeyOutcome::Quit;
            }
            Action::DeletePrev => {
                view.editor.backspace();
                return KeyOutcome::None;
            }
            Action::DeleteWordPrev => {
                while view.editor.cursor > 0
                    && view
                        .editor
                        .text
                        .as_bytes()
                        .get(view.editor.cursor - 1)
                        .map(|b| (*b as char).is_whitespace())
                        .unwrap_or(false)
                {
                    view.editor.backspace();
                }
                while view.editor.cursor > 0
                    && view
                        .editor
                        .text
                        .as_bytes()
                        .get(view.editor.cursor - 1)
                        .map(|b| !(*b as char).is_whitespace())
                        .unwrap_or(false)
                {
                    view.editor.backspace();
                }
                return KeyOutcome::None;
            }
            Action::KillLine => {
                let cur = view.editor.cursor;
                let nl = view.editor.text[cur..]
                    .find('\n')
                    .map(|i| cur + i)
                    .unwrap_or(view.editor.text.len());
                view.editor.text.replace_range(cur..nl, "");
                return KeyOutcome::None;
            }
            Action::PrevHistory => {
                history_prev(view);
                return KeyOutcome::None;
            }
            Action::NextHistory => {
                history_next(view);
                return KeyOutcome::None;
            }
            Action::CycleModel | Action::OpenModel => {
                return KeyOutcome::CycleModel;
            }
            Action::CycleModelBack => {
                return KeyOutcome::CycleModel;
            }
            Action::ToggleThinking => {
                return KeyOutcome::CycleThinking;
            }
            Action::ToggleToolOutput => {
                view.transcript.tool_collapsed = !view.transcript.tool_collapsed;
                return KeyOutcome::None;
            }
            Action::ToggleThinkingOutput => {
                view.transcript.thinking_collapsed = !view.transcript.thinking_collapsed;
                return KeyOutcome::None;
            }
            Action::EditExternal => {
                return KeyOutcome::EditExternal;
            }
            Action::OpenSettings | Action::OpenTree | Action::OpenResume => {
                // These are surfaced by /-commands; not bound to keys by
                // default. No-op fallthrough.
                return KeyOutcome::None;
            }
        }
    }

    // Bare Ctrl+T toggles thinking-collapse. Override default mapping at the
    // request layer, since Ctrl+T in defaults() goes to OpenTree. The dogfood
    // spec asks for Ctrl+T → ToggleThinking-output here.
    if ev.code == KeyCode::Char('t') && ev.modifiers.contains(KeyModifiers::CONTROL) {
        view.transcript.thinking_collapsed = !view.transcript.thinking_collapsed;
        return KeyOutcome::None;
    }
    // Ctrl+O collapses tool output (matches dogfood).
    if ev.code == KeyCode::Char('o') && ev.modifiers.contains(KeyModifiers::CONTROL) {
        view.transcript.tool_collapsed = !view.transcript.tool_collapsed;
        return KeyOutcome::None;
    }

    // No mapping — fall back to raw editing.
    match (ev.code, ev.modifiers) {
        (KeyCode::Char(c), m) if !m.contains(KeyModifiers::CONTROL) => {
            view.editor.insert(c);
            view.last_quit = None;
        }
        (KeyCode::Backspace, _) => view.editor.backspace(),
        (KeyCode::Delete, _) => {
            if view.editor.cursor < view.editor.text.len() {
                let mut end = view.editor.cursor + 1;
                while end < view.editor.text.len() && !view.editor.text.is_char_boundary(end) {
                    end += 1;
                }
                view.editor.text.replace_range(view.editor.cursor..end, "");
            }
        }
        (KeyCode::Left, _) => {
            if view.editor.cursor > 0 {
                let mut new = view.editor.cursor - 1;
                while new > 0 && !view.editor.text.is_char_boundary(new) {
                    new -= 1;
                }
                view.editor.cursor = new;
            }
        }
        (KeyCode::Right, _) => {
            if view.editor.cursor < view.editor.text.len() {
                let mut new = view.editor.cursor + 1;
                while new < view.editor.text.len() && !view.editor.text.is_char_boundary(new) {
                    new += 1;
                }
                view.editor.cursor = new;
            }
        }
        (KeyCode::Home, _) => {
            // Go to start of current visual line.
            let cur = view.editor.cursor;
            let line_start = view.editor.text[..cur].rfind('\n').map(|i| i + 1).unwrap_or(0);
            view.editor.cursor = line_start;
        }
        (KeyCode::End, _) => {
            let cur = view.editor.cursor;
            let nl = view.editor.text[cur..]
                .find('\n')
                .map(|i| cur + i)
                .unwrap_or(view.editor.text.len());
            view.editor.cursor = nl;
        }
        _ => {
            view.dirty = false;
            return KeyOutcome::None;
        }
    }
    let _ = chord; // currently unused outside keymap lookup
    KeyOutcome::None
}

fn history_prev(view: &mut View) {
    if view.history.is_empty() {
        return;
    }
    let new_idx = match view.history_idx {
        None => view.history.len() - 1,
        Some(0) => 0,
        Some(i) => i - 1,
    };
    view.history_idx = Some(new_idx);
    view.editor.text = view.history[new_idx].clone();
    view.editor.cursor = view.editor.text.len();
}

fn history_next(view: &mut View) {
    let Some(i) = view.history_idx else {
        return;
    };
    if i + 1 >= view.history.len() {
        view.history_idx = None;
        view.editor.text.clear();
        view.editor.cursor = 0;
    } else {
        view.history_idx = Some(i + 1);
        view.editor.text = view.history[i + 1].clone();
        view.editor.cursor = view.editor.text.len();
    }
}

fn picker_outcome(kind: PickerKind, value: String) -> KeyOutcome {
    match kind {
        PickerKind::Model => KeyOutcome::SlashCommand("model".into(), value),
        PickerKind::Resume => KeyOutcome::SlashCommand("__resume_pick".into(), value),
        PickerKind::Tree => KeyOutcome::SlashCommand("__tree_pick".into(), value),
        PickerKind::Fork => KeyOutcome::SlashCommand("fork".into(), value),
        PickerKind::Clone => KeyOutcome::SlashCommand("__clone_pick".into(), value),
    }
}

/// Prevents unused warning when no Chord match arms remain.
fn _chord_typecheck(c: Chord) -> Chord {
    c
}
fn _chord_code_typecheck(c: ChordCode) -> ChordCode {
    c
}

// ─── render ────────────────────────────────────────────────────────────────

fn build_frame(
    view: &View,
    theme: &Theme,
    cols: u16,
    rows: u16,
    model: &str,
    cwd: &std::path::Path,
) -> Frame {
    let mut frame = view.transcript.render(theme, cols);

    // Limit transcript to rows minus chrome (editor + footer + separator).
    let editor_lines = std::cmp::max(1, view.editor.text.lines().count()) as u16;
    let chrome = editor_lines + 2; // separator + footer
    let max_transcript = rows.saturating_sub(chrome) as usize;
    if frame.lines.len() > max_transcript {
        let drop = frame.lines.len() - max_transcript;
        frame.lines.drain(0..drop);
    }

    // Separator.
    frame.lines.push(Line {
        spans: vec![Span::coloured(
            "─".repeat(cols as usize),
            theme.muted.to_crossterm(),
        )],
    });

    // Picker overlay or editor pane.
    if let Some(overlay) = &view.picker {
        frame.lines.push(Line {
            spans: vec![Span::coloured(
                format!("{}: {}", overlay.title, overlay.picker.query),
                theme.accent.to_crossterm(),
            )],
        });
        for (i, (_score, item)) in overlay.picker.ranked().iter().enumerate() {
            let prefix = if i == overlay.picker.selected { "▸ " } else { "  " };
            frame.lines.push(Line {
                spans: vec![Span::coloured(
                    format!("{}{}", prefix, item.label),
                    if i == overlay.picker.selected {
                        theme.accent.to_crossterm()
                    } else {
                        theme.fg.to_crossterm()
                    },
                )],
            });
        }
    } else {
        let text_for_display = if view.editor.text.is_empty() {
            "type a message  (/help, /quit)".to_string()
        } else {
            view.editor.text.clone()
        };
        for (i, line) in text_for_display.lines().enumerate() {
            let prefix = if i == 0 { "› " } else { "  " };
            let color = if view.editor.text.is_empty() {
                theme.muted.to_crossterm()
            } else {
                theme.fg.to_crossterm()
            };
            frame.lines.push(Line {
                spans: vec![
                    Span::coloured(prefix.to_string(), theme.accent.to_crossterm()),
                    Span::coloured(line.to_string(), color),
                ],
            });
        }
        if text_for_display.is_empty() {
            frame.lines.push(Line {
                spans: vec![Span::coloured(
                    "› ".to_string(),
                    theme.accent.to_crossterm(),
                )],
            });
        }
    }

    // Footer.
    let mut footer = view.transcript.footer(theme, model, cwd);
    footer.spans.push(Span::coloured(
        format!("  queued:{}", view.queued_count),
        theme.muted.to_crossterm(),
    ));
    footer.spans.push(Span::coloured(
        format!("  thinking:{}", thinking_label(view.thinking)),
        theme.muted.to_crossterm(),
    ));
    if view.last_quit.is_some() {
        footer.spans.push(Span::coloured(
            "  press Ctrl+C again to quit".to_string(),
            theme.error.to_crossterm(),
        ));
    }
    frame.lines.push(footer);
    frame
}

fn thinking_label(t: ThinkingSetting) -> &'static str {
    match t {
        ThinkingSetting::Off => "off",
        ThinkingSetting::Low => "low",
        ThinkingSetting::Medium => "medium",
        ThinkingSetting::High => "high",
    }
}

fn cycle_thinking(t: ThinkingSetting) -> ThinkingSetting {
    match t {
        ThinkingSetting::Off => ThinkingSetting::Low,
        ThinkingSetting::Low => ThinkingSetting::Medium,
        ThinkingSetting::Medium => ThinkingSetting::High,
        ThinkingSetting::High => ThinkingSetting::Off,
    }
}

fn thinking_to_runtime(t: ThinkingSetting) -> pi_ai::ThinkingLevel {
    t.into()
}

// ─── main TUI loop ─────────────────────────────────────────────────────────

async fn run_tui(startup: Startup) -> anyhow::Result<()> {
    let mut slash = SlashRegistry::new();
    slash.register_templates(&startup.prompts);

    let (session, mut rx) = build_session(&startup)?;

    // Pick theme.
    let theme = startup
        .themes
        .get(&startup.settings.theme)
        .cloned()
        .or_else(|| startup.themes.get("dark").cloned())
        .unwrap_or_else(|| pi_tui::ThemeRegistry::new().get("dark").cloned().unwrap());

    let mut view = View::new(startup.keymap.clone(), startup.settings.thinking);
    let mut current_model = format!("{}/{}", startup.settings.provider, startup.settings.model);
    let cwd = startup.runtime_config.cwd.clone();

    let _guard = RawGuard::enter()?;
    let mut renderer = DiffRenderer::new(std::io::stdout());

    let (mut cols, mut rows) = crossterm::terminal::size().unwrap_or((80, 24));
    renderer.resize(cols);

    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    'outer: loop {
        tokio::select! {
            biased;
            maybe_ev = events.next() => {
                let Some(Ok(ct_ev)) = maybe_ev else {
                    if maybe_ev.is_none() { break 'outer; }
                    continue;
                };
                match ct_ev {
                    CtEvent::Key(ke) => {
                        match handle_key(&mut view, &ke) {
                            KeyOutcome::None => {}
                            KeyOutcome::Quit => break 'outer,
                            KeyOutcome::Submit(text) => {
                                view.turn_in_progress = true;
                                let s = session.clone();
                                tokio::spawn(async move { let _ = s.prompt(text).await; });
                            }
                            KeyOutcome::Queue(text) => {
                                session.enqueue(text).await;
                            }
                            KeyOutcome::Abort => {
                                session.abort().await;
                            }
                            KeyOutcome::CycleModel => {
                                current_model = next_model(&startup, &current_model);
                                let (p, m) = split_model(&current_model);
                                session.set_model(p, m).await;
                                view.transcript.model_label = current_model.clone();
                            }
                            KeyOutcome::CycleThinking => {
                                view.thinking = cycle_thinking(view.thinking);
                                session.set_thinking(thinking_to_runtime(view.thinking)).await;
                            }
                            KeyOutcome::EditExternal => {
                                // Temporarily leave raw/alt screen, run
                                // editor, return.
                                let mut out = std::io::stdout();
                                let _ = execute!(out, Show, LeaveAlternateScreen, ResetColor);
                                let _ = disable_raw_mode();
                                let edited = run_external_editor(&view.editor.text);
                                let _ = enable_raw_mode();
                                let _ = execute!(out, EnterAlternateScreen, Hide);
                                if let Some(t) = edited {
                                    view.editor.text = t;
                                    view.editor.cursor = view.editor.text.len();
                                }
                                renderer = DiffRenderer::new(std::io::stdout());
                                renderer.resize(cols);
                                view.dirty = true;
                            }
                            KeyOutcome::SlashCommand(name, args) => {
                                match handle_slash(&slash, &name, &args, &session, &startup, &mut view, &mut current_model).await {
                                    SlashOutcome::Quit => break 'outer,
                                    SlashOutcome::Continue => {}
                                    SlashOutcome::Submit(text) => {
                                        view.turn_in_progress = true;
                                        let s = session.clone();
                                        tokio::spawn(async move { let _ = s.prompt(text).await; });
                                    }
                                }
                            }
                        }
                    }
                    CtEvent::Resize(c, r) => {
                        cols = c; rows = r;
                        renderer.resize(cols);
                        view.dirty = true;
                    }
                    _ => {}
                }
            }
            maybe_ag = rx.recv() => {
                let Some(ev) = maybe_ag else { continue; };
                ingest_event(&mut view, &ev);
                view.dirty = true;
            }
            _ = tick.tick() => {
                if view.dirty {
                    let frame = build_frame(&view, &theme, cols, rows, &current_model, &cwd);
                    let _ = renderer.render(&frame);
                    view.dirty = false;
                }
            }
        }
    }
    Ok(())
}

fn ingest_event(view: &mut View, ev: &AgentEvent) {
    view.transcript.ingest(ev);
    if matches!(
        ev.kind,
        AgentEventKind::TurnComplete | AgentEventKind::Aborted
    ) {
        view.turn_in_progress = false;
    }
}

fn split_model(s: &str) -> (String, String) {
    s.split_once('/')
        .map(|(p, m)| (p.into(), m.into()))
        .unwrap_or_else(|| ("anthropic".into(), s.into()))
}

fn next_model(startup: &Startup, current: &str) -> String {
    let all: Vec<String> = startup
        .runtime_config
        .model_registry
        .providers()
        .flat_map(|p| p.models.iter().map(move |m| format!("{}/{}", p.name, m.id)))
        .collect();
    if all.is_empty() {
        return current.to_string();
    }
    let i = all.iter().position(|m| m == current).unwrap_or(0);
    all[(i + 1) % all.len()].clone()
}

fn run_external_editor(initial: &str) -> Option<String> {
    let editor = std::env::var("VISUAL")
        .ok()
        .or_else(|| std::env::var("EDITOR").ok())?;
    let mut path = std::env::temp_dir();
    path.push(format!("pi-edit-{}.txt", std::process::id()));
    std::fs::write(&path, initial).ok()?;
    let status = std::process::Command::new(&editor).arg(&path).status().ok()?;
    if !status.success() {
        let _ = std::fs::remove_file(&path);
        return None;
    }
    let content = std::fs::read_to_string(&path).ok();
    let _ = std::fs::remove_file(&path);
    content
}

// ─── slash commands ────────────────────────────────────────────────────────

enum SlashOutcome {
    Quit,
    Continue,
    Submit(String),
}

async fn handle_slash(
    slash: &SlashRegistry,
    name: &str,
    args: &str,
    session: &pi_agent_core::AgentSession,
    startup: &Startup,
    view: &mut View,
    current_model: &mut String,
) -> SlashOutcome {
    match name {
        "quit" | "exit" => SlashOutcome::Quit,
        "help" | "hotkeys" => {
            let mut body = String::from("commands:\n");
            for n in slash.names() {
                body.push_str(&format!("  /{n}\n"));
            }
            view.transcript
                .blocks
                .push(crate::renderer::Block::Note(body));
            SlashOutcome::Continue
        }
        "compact" => {
            let ins = if args.is_empty() {
                None
            } else {
                Some(args.to_string())
            };
            session.compact_with_llm(ins).await;
            SlashOutcome::Continue
        }
        "model" => {
            let target = args.trim();
            if target.is_empty() {
                let items: Vec<PickItem<String>> = startup
                    .runtime_config
                    .model_registry
                    .providers()
                    .flat_map(|p| {
                        p.models.iter().map(move |m| PickItem {
                            label: format!("{}/{}", p.name, m.id),
                            value: format!("{}/{}", p.name, m.id),
                        })
                    })
                    .collect();
                view.picker = Some(PickerOverlay {
                    kind: PickerKind::Model,
                    picker: Picker::new(items),
                    title: "model".into(),
                });
                SlashOutcome::Continue
            } else {
                let (p, m) = split_model(target);
                session.set_model(p.clone(), m.clone()).await;
                *current_model = format!("{}/{}", p, m);
                view.transcript
                    .blocks
                    .push(crate::renderer::Block::Note(format!(
                        "[model set to {}]",
                        current_model
                    )));
                SlashOutcome::Continue
            }
        }
        "tree" => {
            let mgr = startup.runtime_config.session_manager.clone();
            let items: Vec<PickItem<String>> = mgr
                .tree(session.id())
                .map(|t| {
                    t.entries
                        .iter()
                        .map(|e| PickItem {
                            label: format!(
                                "{}  {:?}",
                                &e.id.chars().take(8).collect::<String>(),
                                std::mem::discriminant(&e.kind)
                            ),
                            value: e.id.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            view.picker = Some(PickerOverlay {
                kind: PickerKind::Tree,
                picker: Picker::new(items),
                title: "tree".into(),
            });
            SlashOutcome::Continue
        }
        "resume" => {
            let items: Vec<PickItem<String>> = startup
                .runtime_config
                .session_manager
                .list()
                .into_iter()
                .map(|s| PickItem {
                    label: format!("{}  {}", s.id, s.path.display()),
                    value: s.id,
                })
                .collect();
            view.picker = Some(PickerOverlay {
                kind: PickerKind::Resume,
                picker: Picker::new(items),
                title: "resume".into(),
            });
            SlashOutcome::Continue
        }
        "fork" => {
            let target = args.trim();
            if target.is_empty() {
                let mgr = startup.runtime_config.session_manager.clone();
                let items: Vec<PickItem<String>> = mgr
                    .tree(session.id())
                    .map(|t| {
                        t.entries
                            .iter()
                            .map(|e| PickItem {
                                label: e.id.clone(),
                                value: e.id.clone(),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                view.picker = Some(PickerOverlay {
                    kind: PickerKind::Fork,
                    picker: Picker::new(items),
                    title: "fork from".into(),
                });
            } else {
                let _ = startup
                    .runtime_config
                    .session_manager
                    .fork(session.id(), target);
                view.transcript
                    .blocks
                    .push(crate::renderer::Block::Note(format!(
                        "[forked at {}]",
                        target
                    )));
            }
            SlashOutcome::Continue
        }
        "clone" => {
            let mgr = startup.runtime_config.session_manager.clone();
            match mgr.create(&startup.settings.provider, &startup.settings.model) {
                Ok(meta) => view
                    .transcript
                    .blocks
                    .push(crate::renderer::Block::Note(format!(
                        "[cloned → {}]",
                        meta.id
                    ))),
                Err(e) => view
                    .transcript
                    .blocks
                    .push(crate::renderer::Block::Error(format!("clone: {e}"))),
            }
            SlashOutcome::Continue
        }
        "share" => {
            if which::which("gh").is_err() {
                view.transcript.blocks.push(crate::renderer::Block::Error(
                    "/share requires the `gh` CLI; install it from https://cli.github.com".into(),
                ));
                return SlashOutcome::Continue;
            }
            let mut tmp = std::env::temp_dir();
            tmp.push(format!("pi-share-{}.md", std::process::id()));
            let messages = session.messages().await;
            let mut body = String::new();
            for m in messages {
                body.push_str(&format!("## {:?}\n\n{}\n\n", m.role, m.text()));
            }
            if std::fs::write(&tmp, &body).is_err() {
                view.transcript.blocks.push(crate::renderer::Block::Error(
                    "failed to write share tmpfile".into(),
                ));
                return SlashOutcome::Continue;
            }
            let out = std::process::Command::new("gh")
                .args(["gist", "create", "--public"])
                .arg(&tmp)
                .output();
            let _ = std::fs::remove_file(&tmp);
            match out {
                Ok(o) if o.status.success() => {
                    let url = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    view.transcript
                        .blocks
                        .push(crate::renderer::Block::Note(format!("[shared: {url}]")));
                }
                Ok(o) => {
                    view.transcript.blocks.push(crate::renderer::Block::Error(format!(
                        "gh: {}",
                        String::from_utf8_lossy(&o.stderr)
                    )));
                }
                Err(e) => {
                    view.transcript
                        .blocks
                        .push(crate::renderer::Block::Error(format!("gh: {e}")));
                }
            }
            SlashOutcome::Continue
        }
        "login" => {
            let ep = pi_ai::oauth::OAuthEndpoints::anthropic();
            let pkce = pi_ai::oauth::Pkce::new();
            let state = format!("pi-{}", std::process::id());
            let url = pi_ai::build_authorize_url(&ep, &pkce, &state);
            view.transcript
                .blocks
                .push(crate::renderer::Block::Note(format!(
                    "open in your browser:\n{url}"
                )));
            // listen on the loopback port specified in the redirect_uri.
            let listen_addr = "127.0.0.1:54545";
            match pi_ai::oauth::listen_for_callback(listen_addr, &state).await {
                Ok((code, _)) => {
                    let client = reqwest_client();
                    match pi_ai::exchange_code(&client, &ep, &pkce, &code).await {
                        Ok(tok) => {
                            startup
                                .runtime_config
                                .auth_storage
                                .set("anthropic", tok.into_auth_method());
                            view.transcript
                                .blocks
                                .push(crate::renderer::Block::Note("[login: ok]".into()));
                        }
                        Err(e) => view
                            .transcript
                            .blocks
                            .push(crate::renderer::Block::Error(format!("token: {e}"))),
                    }
                }
                Err(e) => view
                    .transcript
                    .blocks
                    .push(crate::renderer::Block::Error(format!("callback: {e}"))),
            }
            SlashOutcome::Continue
        }
        "settings" => {
            view.transcript
                .blocks
                .push(crate::renderer::Block::Note(format!(
                    "settings: {}",
                    crate::context::settings_paths().0.display()
                )));
            SlashOutcome::Continue
        }
        // Internal slash names produced by picker resolution.
        "__resume_pick" => {
            view.transcript
                .blocks
                .push(crate::renderer::Block::Note(format!("[resume {}]", args)));
            SlashOutcome::Continue
        }
        "__tree_pick" => {
            view.transcript
                .blocks
                .push(crate::renderer::Block::Note(format!("[tree {}]", args)));
            SlashOutcome::Continue
        }
        "__clone_pick" => SlashOutcome::Continue,
        other => {
            if let Some(cmd) = slash.get(other) {
                if let SlashKind::Template { body } = &cmd.kind {
                    return SlashOutcome::Submit(slash::render_template(body, args));
                }
            }
            view.transcript
                .blocks
                .push(crate::renderer::Block::Error(format!(
                    "unknown command: /{other}"
                )));
            SlashOutcome::Continue
        }
    }
}

fn reqwest_client() -> reqwest::Client {
    reqwest::Client::builder()
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

// ─────────────────────────────────────────────────────────────────────────────
// Line-based fallback (kept verbatim from the previous implementation)
// ─────────────────────────────────────────────────────────────────────────────

async fn run_line_based(startup: Startup) -> anyhow::Result<()> {
    let mut slash = SlashRegistry::new();
    slash.register_templates(&startup.prompts);

    let (session, mut rx) = build_session(&startup)?;

    print_header(&startup);

    let printer = tokio::spawn(async move {
        let mut current_line_open = false;
        while let Some(ev) = rx.recv().await {
            match ev.kind {
                AgentEventKind::AssistantTextDelta { text } => {
                    let mut out = std::io::stdout();
                    let _ = execute!(out, SetForegroundColor(Color::Green));
                    let _ = write!(out, "{}", text);
                    let _ = execute!(out, ResetColor);
                    let _ = out.flush();
                    current_line_open = true;
                }
                AgentEventKind::AssistantThinkingDelta { text } => {
                    let mut out = std::io::stdout();
                    let _ = execute!(out, SetForegroundColor(Color::DarkGrey));
                    let _ = write!(out, "{}", text);
                    let _ = execute!(out, ResetColor);
                    let _ = out.flush();
                    current_line_open = true;
                }
                AgentEventKind::AssistantToolCall { call } => {
                    if current_line_open {
                        println!();
                    }
                    let mut out = std::io::stdout();
                    let _ = execute!(out, SetForegroundColor(Color::Yellow));
                    let _ = writeln!(
                        out,
                        "→ {} {}",
                        call.name,
                        serde_json::to_string(&call.input).unwrap_or_default()
                    );
                    let _ = execute!(out, ResetColor);
                    current_line_open = false;
                }
                AgentEventKind::ToolResult { result } => {
                    let mut out = std::io::stdout();
                    let color = if result.is_error { Color::Red } else { Color::DarkGrey };
                    let _ = execute!(out, SetForegroundColor(color));
                    for line in result.model_output.lines().take(20) {
                        let _ = writeln!(out, "  {line}");
                    }
                    if result.model_output.lines().count() > 20 {
                        let _ = writeln!(out, "  …");
                    }
                    let _ = execute!(out, ResetColor);
                }
                AgentEventKind::Error { message } => {
                    let mut out = std::io::stdout();
                    let _ = execute!(out, SetForegroundColor(Color::Red));
                    let _ = writeln!(out, "[error] {}", message);
                    let _ = execute!(out, ResetColor);
                }
                AgentEventKind::Usage { usage } => {
                    let mut out = std::io::stdout();
                    let _ = execute!(out, SetForegroundColor(Color::DarkGrey));
                    let _ = writeln!(
                        out,
                        "[tokens: in={} out={}]",
                        usage.input_tokens, usage.output_tokens
                    );
                    let _ = execute!(out, ResetColor);
                }
                AgentEventKind::TurnComplete => {
                    if current_line_open {
                        println!();
                    }
                    let _ = current_line_open;
                    break;
                }
                AgentEventKind::Aborted => {
                    println!("\n[aborted]");
                    break;
                }
                _ => {}
            }
        }
    });

    use tokio::io::AsyncBufReadExt;
    let mut stdin = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    let mut handle = printer;
    loop {
        if handle.is_finished() {
            // idle.
        }
        print_input_prompt(&startup);
        let line = match stdin.next_line().await? {
            Some(l) => l,
            None => break,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some((name, args)) = slash::parse(trimmed) {
            match handle_slash_line(&slash, &name, &args, &session, &startup).await {
                LineSlashOutcome::Quit => break,
                LineSlashOutcome::Continue => continue,
                LineSlashOutcome::Submit(text) => {
                    handle.abort();
                    let _ = session.prompt(text).await;
                    handle = tokio::spawn(async move {});
                }
            }
            continue;
        }
        handle.abort();
        let _ = session.prompt(trimmed.to_string()).await;
        handle = tokio::spawn(async move {});
    }
    Ok(())
}

enum LineSlashOutcome {
    Quit,
    Continue,
    Submit(String),
}

async fn handle_slash_line(
    slash: &SlashRegistry,
    name: &str,
    args: &str,
    session: &pi_agent_core::AgentSession,
    startup: &Startup,
) -> LineSlashOutcome {
    match name {
        "quit" | "exit" => LineSlashOutcome::Quit,
        "help" | "hotkeys" => {
            for n in slash.names() {
                println!("/{n}");
            }
            LineSlashOutcome::Continue
        }
        "compact" => {
            let ins = if args.is_empty() {
                None
            } else {
                Some(args.to_string())
            };
            session.compact(ins).await;
            println!("[compacted]");
            LineSlashOutcome::Continue
        }
        "model" => {
            let target = args.trim();
            if target.is_empty() {
                for p in startup.runtime_config.model_registry.providers() {
                    for m in &p.models {
                        println!("{}/{}", p.name, m.id);
                    }
                }
            } else {
                let (provider, model) = target
                    .split_once('/')
                    .map(|(p, m)| (p.to_string(), m.to_string()))
                    .unwrap_or_else(|| ("anthropic".into(), target.to_string()));
                session.set_model(provider, model).await;
                println!("[model set to {}]", target);
            }
            LineSlashOutcome::Continue
        }
        other => {
            if let Some(cmd) = slash.get(other) {
                if let SlashKind::Template { body } = &cmd.kind {
                    return LineSlashOutcome::Submit(slash::render_template(body, args));
                }
            }
            println!("unknown slash command: /{other}");
            LineSlashOutcome::Continue
        }
    }
}

fn print_header(startup: &Startup) {
    let mut out = std::io::stdout();
    let _ = queue!(out, SetForegroundColor(Color::Cyan), Print("pi-rs "));
    let _ = queue!(
        out,
        ResetColor,
        Print(format!(
            "({}/{})\n",
            startup.settings.provider, startup.settings.model
        ))
    );
    let _ = queue!(
        out,
        SetForegroundColor(Color::DarkGrey),
        Print(format!("cwd: {}\n", startup.runtime_config.cwd.display()))
    );
    let _ = queue!(
        out,
        Print("type a message, /help for commands, /quit to exit\n\n")
    );
    let _ = queue!(out, ResetColor);
    let _ = out.flush();
}

fn print_input_prompt(_startup: &Startup) {
    let mut out = std::io::stdout();
    let _ = execute!(
        out,
        SetForegroundColor(Color::Cyan),
        Print("\nyou> "),
        ResetColor
    );
    let _ = out.flush();
}

// Suppress unused-import warning on platforms without a cursor.
#[allow(dead_code)]
fn _force_link() {
    let _ = cursor::Hide;
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn ke(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    fn fresh_view() -> View {
        View::new(Keymap::defaults(), ThinkingSetting::Off)
    }

    #[test]
    fn quit_then_cancel_resets_quit_timer() {
        let mut v = fresh_view();
        // First Ctrl+C = arm quit-confirm, clear editor, do not exit.
        let r = handle_key(&mut v, &ke(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(r, KeyOutcome::None);
        assert!(v.last_quit.is_some());
        // Escape (= Cancel) clears the timer.
        let r = handle_key(&mut v, &ke(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(r, KeyOutcome::None);
        assert!(v.last_quit.is_none());
        // Second Ctrl+C now does NOT immediately quit (timer was reset).
        let r = handle_key(&mut v, &ke(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(r, KeyOutcome::None);
        assert!(v.last_quit.is_some());
    }

    #[test]
    fn typing_inserts_then_submit_emits_buffer() {
        let mut v = fresh_view();
        for c in "hi".chars() {
            handle_key(&mut v, &ke(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(v.editor.text, "hi");
        assert_eq!(v.editor.cursor, 2);
        // Move cursor left, insert.
        handle_key(&mut v, &ke(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(v.editor.cursor, 1);
        handle_key(&mut v, &ke(KeyCode::Char('!'), KeyModifiers::NONE));
        assert_eq!(v.editor.text, "h!i");
        // Submit.
        let r = handle_key(&mut v, &ke(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(r, KeyOutcome::Submit("h!i".into()));
        assert_eq!(v.editor.text, "");
    }

    #[test]
    fn slash_command_routes_to_slash_outcome() {
        let mut v = fresh_view();
        for c in "/help".chars() {
            handle_key(&mut v, &ke(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let r = handle_key(&mut v, &ke(KeyCode::Enter, KeyModifiers::NONE));
        match r {
            KeyOutcome::SlashCommand(name, _) => assert_eq!(name, "help"),
            other => panic!("expected SlashCommand, got {:?}", other),
        }
    }

    #[test]
    fn picker_query_and_enter_returns_value() {
        let mut v = fresh_view();
        let items = vec![
            PickItem {
                label: "claude-haiku-4-5-20251001".into(),
                value: "anthropic/claude-haiku-4-5-20251001".into(),
            },
            PickItem {
                label: "claude-sonnet-4-5-20251001".into(),
                value: "anthropic/claude-sonnet-4-5-20251001".into(),
            },
            PickItem {
                label: "gpt-4o".into(),
                value: "openai/gpt-4o".into(),
            },
        ];
        v.picker = Some(PickerOverlay {
            kind: PickerKind::Model,
            picker: Picker::new(items),
            title: "model".into(),
        });

        // Type query "haiku" — should select that.
        for c in "haiku".chars() {
            handle_key(&mut v, &ke(KeyCode::Char(c), KeyModifiers::NONE));
        }
        // Enter resolves.
        let r = handle_key(&mut v, &ke(KeyCode::Enter, KeyModifiers::NONE));
        match r {
            KeyOutcome::SlashCommand(name, args) => {
                assert_eq!(name, "model");
                assert_eq!(args, "anthropic/claude-haiku-4-5-20251001");
            }
            other => panic!("expected SlashCommand, got {:?}", other),
        }
        assert!(v.picker.is_none());
    }

    #[test]
    fn shift_enter_inserts_newline_not_submit() {
        let mut v = fresh_view();
        for c in "ab".chars() {
            handle_key(&mut v, &ke(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let r = handle_key(&mut v, &ke(KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(r, KeyOutcome::None);
        assert_eq!(v.editor.text, "ab\n");
    }

    #[test]
    fn alt_enter_queues_followup() {
        let mut v = fresh_view();
        for c in "queued".chars() {
            handle_key(&mut v, &ke(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let r = handle_key(&mut v, &ke(KeyCode::Enter, KeyModifiers::ALT));
        assert_eq!(r, KeyOutcome::Queue("queued".into()));
        assert_eq!(v.queued_count, 1);
    }

    #[test]
    fn history_navigation() {
        let mut v = fresh_view();
        v.history.push("first".into());
        v.history.push("second".into());
        handle_key(&mut v, &ke(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(v.editor.text, "second");
        handle_key(&mut v, &ke(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(v.editor.text, "first");
        handle_key(&mut v, &ke(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(v.editor.text, "second");
        handle_key(&mut v, &ke(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(v.editor.text, "");
    }
}
