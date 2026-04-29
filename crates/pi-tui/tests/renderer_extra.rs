//! Extra coverage for `pi-tui::renderer::DiffRenderer`.
//!
//! These tests use a small `CountingWriter` so we can inspect the bytes
//! written by each render call without violating the borrow checker.

use crossterm::style::Color;
use pi_tui::renderer::{DiffRenderer, Frame, Line, Span};
use std::io::Write;
use std::sync::{Arc, Mutex};

/// Writer that appends to a shared `Vec<u8>` so tests can inspect it
/// after the renderer drops its mutable borrow.
#[derive(Clone, Default)]
struct SharedWriter {
    inner: Arc<Mutex<Vec<u8>>>,
}

impl Write for SharedWriter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.inner.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl SharedWriter {
    fn snapshot(&self) -> Vec<u8> {
        self.inner.lock().unwrap().clone()
    }
}

/// Serialises tests that flip the global `PI_NO_SYNC` env var.
fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static M: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    M.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

#[test]
fn line_plain_constructs_a_single_plain_span() {
    let l = Line::plain("hi");
    assert_eq!(l.spans.len(), 1);
    assert_eq!(l.spans[0].text, "hi");
    assert!(l.spans[0].color.is_none());
}

#[test]
fn render_emits_sgr_escape_sequences_for_coloured_spans() {
    let _g = env_lock();
    std::env::set_var("PI_NO_SYNC", "1");
    let w = SharedWriter::default();
    {
        let mut r = DiffRenderer::new(w.clone());
        let frame = Frame {
            lines: vec![Line {
                spans: vec![Span::plain("plain "), Span::coloured("red", Color::Red)],
            }],
            cursor_at: None,
        };
        r.render(&frame).unwrap();
    }
    let s = String::from_utf8_lossy(&w.snapshot()).into_owned();
    assert!(s.contains("plain"));
    assert!(s.contains("red"));
    assert!(
        s.contains("\x1b["),
        "expected SGR escape sequence, got: {s:?}"
    );
}

#[test]
fn second_render_of_unchanged_frame_writes_fewer_bytes() {
    let _g = env_lock();
    std::env::set_var("PI_NO_SYNC", "1");
    let frame = Frame {
        lines: vec![
            Line {
                spans: vec![Span::coloured("alpha", Color::Cyan)],
            },
            Line {
                spans: vec![Span::coloured("beta", Color::Magenta)],
            },
        ],
            cursor_at: None,
        };
    let w = SharedWriter::default();
    let mut r = DiffRenderer::new(w.clone());
    r.render(&frame).unwrap();
    let after_first = w.snapshot().len();
    r.render(&frame).unwrap();
    let after_second_total = w.snapshot().len();
    let second_render_bytes = after_second_total - after_first;
    assert!(
        second_render_bytes < after_first,
        "second render wrote {} bytes (first wrote {}), expected fewer",
        second_render_bytes,
        after_first
    );
    // The second render must NOT re-emit the visible label text.
    let second_part = &w.snapshot()[after_first..];
    let s2 = String::from_utf8_lossy(second_part);
    assert!(
        !s2.contains("alpha") && !s2.contains("beta"),
        "second render should skip unchanged lines: {s2:?}"
    );
}

#[test]
fn shrinking_frame_clears_leftover_lines_from_previous_frame() {
    let _g = env_lock();
    std::env::set_var("PI_NO_SYNC", "1");
    let w = SharedWriter::default();
    let mut r = DiffRenderer::new(w.clone());
    let big = Frame {
        lines: vec![Line::plain("one"), Line::plain("two"), Line::plain("three")],
            cursor_at: None,
        };
    let small = Frame {
        lines: vec![Line::plain("one"), Line::plain("two")],
            cursor_at: None,
        };
    r.render(&big).unwrap();
    let mid = w.snapshot().len();
    r.render(&small).unwrap();
    assert!(w.snapshot().len() > mid, "leftover-clear should write");
}

#[test]
fn render_with_sync_writes_dec2026_begin_end_markers() {
    let _g = env_lock();
    std::env::remove_var("PI_NO_SYNC");
    let w = SharedWriter::default();
    {
        let mut r = DiffRenderer::new(w.clone());
        r.render(&Frame {
            lines: vec![Line::plain("synced")],
            cursor_at: None,
        })
        .unwrap();
    }
    let s = String::from_utf8_lossy(&w.snapshot()).into_owned();
    assert!(
        s.contains("\x1b[?2026h"),
        "expected sync-begin marker, got: {s:?}"
    );
    assert!(
        s.contains("\x1b[?2026l"),
        "expected sync-end marker, got: {s:?}"
    );
    std::env::set_var("PI_NO_SYNC", "1");
}

#[test]
fn resize_changes_width_and_render_still_succeeds() {
    let _g = env_lock();
    std::env::set_var("PI_NO_SYNC", "1");
    let w = SharedWriter::default();
    let mut r = DiffRenderer::new(w);
    r.resize(40);
    r.render(&Frame {
        lines: vec![Line::plain("xx")],
            cursor_at: None,
        })
    .unwrap();
    r.resize(120);
}
