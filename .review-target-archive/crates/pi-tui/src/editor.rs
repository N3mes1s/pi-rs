/// Editor primitives: a multi-line input buffer with cursor, used by the
/// interactive mode. Kept simple — no syntax highlighting, just enough to
/// support `@filename` completion, `!command` execution and Shift+Enter
/// multiline as upstream pi describes.
#[derive(Debug, Clone, Default)]
pub struct Editor {
    pub text: String,
    pub cursor: usize,
}

#[derive(Debug, Clone)]
pub enum EditorEvent {
    /// User pressed Enter (Submit). String is the buffer at that point.
    Submit(String),
    /// User pressed Alt+Enter (queue follow-up).
    QueueFollowUp(String),
    /// Buffer changed.
    Changed,
    /// User asked for `@filename` completion at cursor.
    AtCompletion { prefix: String },
    /// User asked for `!command` execution.
    BangCommand { command: String, silent: bool },
    /// User pressed Escape (cancel/abort).
    Cancel,
    /// User pressed Ctrl+C twice — quit.
    Quit,
}

impl Editor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, ch: char) {
        let mut bytes = [0u8; 4];
        let s = ch.encode_utf8(&mut bytes);
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    pub fn insert_str(&mut self, s: &str) {
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let mut new = self.cursor - 1;
        while new > 0 && !self.text.is_char_boundary(new) {
            new -= 1;
        }
        self.text.replace_range(new..self.cursor, "");
        self.cursor = new;
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    pub fn submit(&mut self) -> EditorEvent {
        let buf = std::mem::take(&mut self.text);
        self.cursor = 0;
        EditorEvent::Submit(buf)
    }

    /// Detect special leading commands. Returns Some(event) if recognised.
    pub fn special_command(&self) -> Option<EditorEvent> {
        let trimmed = self.text.trim_start();
        if let Some(rest) = trimmed.strip_prefix("!!") {
            return Some(EditorEvent::BangCommand {
                command: rest.to_string(),
                silent: true,
            });
        }
        if let Some(rest) = trimmed.strip_prefix('!') {
            return Some(EditorEvent::BangCommand {
                command: rest.to_string(),
                silent: false,
            });
        }
        None
    }
}
