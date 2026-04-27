//! Generic fuzzy picker used by `/resume`, `/model`, `/tree`, `/fork`, `/clone`.
//!
//! The picker is decoupled from any IO so its filtering and selection
//! algorithm can be unit-tested deterministically. The TUI side owns the
//! input/output loop and feeds the picker keystrokes via [`Picker::on_key`].

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::Matcher;
use std::cmp::Ordering;

// ─── label formatters ────────────────────────────────────────────────────────

/// Format a [`pi_agent_core::SessionMeta`] into a human-readable picker label.
///
/// Format: `"<short_id>  <provider>/<model>  <YYYY-MM-DD HH:MM>  <title>"`
///
/// - `short_id` — first 8 characters of `meta.id`
/// - timestamp — `meta.updated_at` (milliseconds since Unix epoch) converted
///   to UTC and formatted as `%Y-%m-%d %H:%M`
/// - title — `meta.title` if `Some` and non-empty, otherwise `"(no title)"`
pub fn format_session_label(meta: &pi_agent_core::SessionMeta) -> String {
    use chrono::{TimeZone, Utc};

    let short_id: String = meta.id.chars().take(8).collect();

    let ts_secs = meta.updated_at / 1_000;
    let ts_nanos = ((meta.updated_at % 1_000) * 1_000_000) as u32;
    let dt = Utc
        .timestamp_opt(ts_secs, ts_nanos)
        .single()
        .unwrap_or_else(Utc::now);
    let timestamp = dt.format("%Y-%m-%d %H:%M").to_string();

    let title = meta
        .title
        .as_deref()
        .filter(|t| !t.is_empty())
        .unwrap_or("(no title)");

    format!(
        "{}  {}/{}  {}  {}",
        short_id, meta.provider, meta.model, timestamp, title
    )
}

/// Format a [`pi_agent_core::SessionEntry`] into a human-readable picker label.
///
/// Format: `"<kind>  <short_text>"` where `short_text` is up to 60 characters
/// of the entry's primary text with newlines replaced by spaces.
///
/// Kind strings:
/// - `User` → `"user"`
/// - `Assistant` → `"assistant"`
/// - `ToolCall { call }` → `"tool_call: <name>"`
/// - `ToolResult` → `"tool_result"`
/// - `Compaction` → `"compaction"`
/// - `Meta` → `"meta"`
/// - `SystemPrompt` → `"system"`
/// - `Usage` → `"usage"` (no text)
pub fn format_tree_entry(entry: &pi_agent_core::SessionEntry) -> String {
    use pi_agent_core::SessionEntryKind;

    let (kind_str, raw_text): (&str, String) = match &entry.kind {
        SessionEntryKind::User { message } => ("user", message.text()),
        SessionEntryKind::Assistant { message } => ("assistant", message.text()),
        SessionEntryKind::ToolCall { call } => {
            // Return immediately so we can own the kind string.
            let short = short_text(&call.name, 60);
            return format!("tool_call: {}  {}", call.name, short);
        }
        SessionEntryKind::ToolResult { result } => {
            ("tool_result", result.model_output.clone())
        }
        SessionEntryKind::Compaction { summary, .. } => ("compaction", summary.clone()),
        SessionEntryKind::Meta { .. } => ("meta", String::new()),
        SessionEntryKind::SystemPrompt { text } => ("system", text.clone()),
        SessionEntryKind::Usage { .. } => ("usage", String::new()),
    };

    let snippet = short_text(&raw_text, 60);
    if snippet.is_empty() {
        kind_str.to_string()
    } else {
        format!("{}  {}", kind_str, snippet)
    }
}

/// Truncate `text` to at most `max_chars` Unicode scalar values, replacing
/// newlines with spaces first.
fn short_text(text: &str, max_chars: usize) -> String {
    let flat: String = text.replace('\n', " ").replace('\r', " ");
    flat.chars().take(max_chars).collect()
}

#[derive(Debug, Clone)]
pub struct PickItem<T> {
    pub label: String,
    pub value: T,
}

#[derive(Debug)]
pub struct Picker<T> {
    items: Vec<PickItem<T>>,
    pub query: String,
    pub selected: usize,
    pub limit: usize,
}

impl<T: Clone> Picker<T> {
    pub fn new(items: Vec<PickItem<T>>) -> Self {
        Self {
            items,
            query: String::new(),
            selected: 0,
            limit: 20,
        }
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /// Returns the total number of candidate items (unfiltered).
    pub fn items_len(&self) -> usize {
        self.items.len()
    }

    pub fn ranked(&self) -> Vec<(i64, &PickItem<T>)> {
        if self.query.is_empty() {
            return self
                .items
                .iter()
                .map(|i| (0, i))
                .take(self.limit)
                .collect();
        }
        let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
        let pat = Pattern::parse(&self.query, CaseMatching::Smart, Normalization::Smart);
        let mut scored: Vec<(i64, &PickItem<T>)> = self
            .items
            .iter()
            .filter_map(|i| {
                let s = pat.score(
                    nucleo_matcher::Utf32Str::Ascii(i.label.as_bytes()),
                    &mut matcher,
                );
                s.map(|s| (s as i64, i))
            })
            .collect();
        scored.sort_by(|a, b| match b.0.cmp(&a.0) {
            Ordering::Equal => a.1.label.cmp(&b.1.label),
            other => other,
        });
        scored.truncate(self.limit);
        scored
    }

    pub fn selected_value(&self) -> Option<T> {
        self.ranked()
            .get(self.selected)
            .map(|(_, item)| item.value.clone())
    }

    pub fn move_down(&mut self) {
        let len = self.ranked().len();
        if len > 0 {
            self.selected = (self.selected + 1) % len;
        }
    }

    pub fn move_up(&mut self) {
        let len = self.ranked().len();
        if len > 0 {
            self.selected = (self.selected + len - 1) % len;
        }
    }

    pub fn push_query(&mut self, c: char) {
        self.query.push(c);
        self.selected = 0;
    }

    pub fn pop_query(&mut self) {
        self.query.pop();
        self.selected = 0;
    }
}
