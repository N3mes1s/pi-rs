use pi_tui::editor::{Editor, EditorEvent};

#[test]
fn insert_appends_at_cursor() {
    let mut e = Editor::new();
    e.insert('h');
    e.insert('i');
    assert_eq!(e.text, "hi");
    assert_eq!(e.cursor, 2);
}

#[test]
fn insert_str_advances_cursor_by_byte_len() {
    let mut e = Editor::new();
    e.insert_str("héllo"); // é is 2 bytes
    assert_eq!(e.text, "héllo");
    assert_eq!(e.cursor, e.text.len());
}

#[test]
fn backspace_at_zero_is_noop() {
    let mut e = Editor::new();
    e.backspace();
    assert_eq!(e.text, "");
    assert_eq!(e.cursor, 0);
}

#[test]
fn backspace_handles_multibyte_chars() {
    let mut e = Editor::new();
    e.insert_str("aé");
    e.backspace();
    // The whole multi-byte é must disappear.
    assert_eq!(e.text, "a");
    assert_eq!(e.cursor, 1);
}

#[test]
fn clear_resets_text_and_cursor() {
    let mut e = Editor::new();
    e.insert_str("anything");
    e.clear();
    assert_eq!(e.text, "");
    assert_eq!(e.cursor, 0);
}

#[test]
fn submit_drains_buffer() {
    let mut e = Editor::new();
    e.insert_str("hello");
    let ev = e.submit();
    match ev {
        EditorEvent::Submit(s) => assert_eq!(s, "hello"),
        _ => panic!("expected Submit"),
    }
    assert_eq!(e.text, "");
    assert_eq!(e.cursor, 0);
}

#[test]
fn special_command_recognises_bang_and_double_bang() {
    let mut e = Editor::new();
    e.insert_str("!ls -la");
    match e.special_command().expect("should detect !") {
        EditorEvent::BangCommand { command, silent } => {
            assert_eq!(command, "ls -la");
            assert!(!silent);
        }
        _ => panic!("expected BangCommand"),
    }

    e.clear();
    e.insert_str("!!quiet");
    match e.special_command().expect("should detect !!") {
        EditorEvent::BangCommand { command, silent } => {
            assert_eq!(command, "quiet");
            assert!(silent, "double-bang should be silent");
        }
        _ => panic!("expected BangCommand"),
    }

    e.clear();
    e.insert_str("not a bang");
    assert!(e.special_command().is_none());
}
