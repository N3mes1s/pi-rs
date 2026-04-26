//! Generic fuzzy picker used by `/resume`, `/model`, `/tree`, `/fork`, `/clone`.
//!
//! The picker is decoupled from any IO so its filtering and selection
//! algorithm can be unit-tested deterministically. The TUI side owns the
//! input/output loop and feeds the picker keystrokes via [`Picker::on_key`].

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::Matcher;
use std::cmp::Ordering;

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
