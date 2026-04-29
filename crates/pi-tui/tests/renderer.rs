use pi_tui::renderer::{DiffRenderer, Frame, Line, Span};

#[test]
fn empty_frame_emits_no_visible_text() {
    // We render into a Vec<u8> writer so no real TTY is required.
    std::env::set_var("PI_NO_SYNC", "1");
    let buf: Vec<u8> = Vec::new();
    let mut r = DiffRenderer::new(buf);
    r.render(&Frame::default()).unwrap();
    // The renderer is allowed to emit cursor/clear control sequences but
    // it must not write any printable text content.
    // We re-render to extract the bytes by rendering once more and checking
    // we don't crash, then drop. Capture by rebuilding fresh:
    let mut buf2: Vec<u8> = Vec::new();
    let mut r2 = DiffRenderer::new(&mut buf2);
    r2.render(&Frame::default()).unwrap();
    let text: String = String::from_utf8_lossy(&buf2)
        .chars()
        .filter(|c| !c.is_control() && *c != '\u{1b}' && *c != '[')
        .collect();
    // Strip remaining "?2026hl" style chars left over from CSI sequences.
    let trimmed: String = text
        .chars()
        .filter(|c| !"?0123456789hlABCDEFGHJKMS".contains(*c))
        .collect();
    assert!(
        trimmed.trim().is_empty(),
        "expected no printable content, got: {trimmed:?}"
    );
}

#[test]
fn render_writes_hello() {
    std::env::set_var("PI_NO_SYNC", "1");
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut r = DiffRenderer::new(&mut buf);
        let frame = Frame {
            lines: vec![Line {
                spans: vec![Span::plain("hello")],
            }],
            cursor_at: None,
        };
        r.render(&frame).unwrap();
    }
    let s = String::from_utf8_lossy(&buf);
    assert!(
        s.contains("hello"),
        "expected output to contain 'hello', got: {s:?}"
    );
}

#[test]
fn resize_updates_width_without_panicking() {
    let mut buf: Vec<u8> = Vec::new();
    let mut r = DiffRenderer::new(&mut buf);
    r.resize(40);
    r.resize(120);
    // No panic ⇒ pass.
}

#[test]
fn render_then_render_diff_only_redraws_changed_lines() {
    std::env::set_var("PI_NO_SYNC", "1");
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut r = DiffRenderer::new(&mut buf);
        let f1 = Frame {
            lines: vec![
                Line {
                    spans: vec![Span::plain("line-a")],
                },
                Line {
                    spans: vec![Span::plain("line-b")],
                },
            ],
            cursor_at: None,
        };
        r.render(&f1).unwrap();
        let after_first = buf.len();

        // Render the exact same frame: no visible-line writes should happen,
        // although the renderer is allowed to emit a few control sequences.
        let mut buf2: Vec<u8> = Vec::new();
        let mut r2 = DiffRenderer::new(&mut buf2);
        r2.render(&f1).unwrap();
        r2.render(&f1).unwrap();
        let s2 = String::from_utf8_lossy(&buf2);
        // line-a/line-b appears at most twice (only on first render).
        let count_a = s2.matches("line-a").count();
        assert!(count_a <= 1, "duplicate redraw detected: {count_a}");
        let _ = after_first;
    }
}
