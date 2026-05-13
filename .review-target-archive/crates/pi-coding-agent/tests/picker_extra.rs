//! Extra coverage for the Picker — empty picker returns `None` from
//! `selected_value`, `with_limit(3)` truncates a 5-item list, and pop_query
//! on empty query is a no-op.

use pi_coding_agent::picker::{PickItem, Picker};

#[test]
fn selected_value_on_empty_picker_returns_none() {
    let p: Picker<&'static str> = Picker::new(Vec::new());
    assert!(p.selected_value().is_none());
    assert!(p.ranked().is_empty());
}

#[test]
fn with_limit_caps_ranked_results() {
    let items: Vec<PickItem<i32>> = (0..5)
        .map(|i| PickItem {
            label: format!("item-{i}"),
            value: i,
        })
        .collect();
    let p = Picker::new(items).with_limit(3);
    assert_eq!(p.ranked().len(), 3);
    let labels: Vec<&str> = p.ranked().iter().map(|(_, i)| i.label.as_str()).collect();
    assert_eq!(labels, vec!["item-0", "item-1", "item-2"]);
}

#[test]
fn move_down_on_empty_picker_does_not_panic_and_keeps_selected_zero() {
    let mut p: Picker<i32> = Picker::new(Vec::new());
    p.move_down();
    p.move_up();
    assert_eq!(p.selected, 0);
}

#[test]
fn pop_query_on_empty_query_is_a_noop() {
    let mut p: Picker<i32> = Picker::new(vec![PickItem {
        label: "x".into(),
        value: 1,
    }]);
    p.pop_query();
    assert_eq!(p.query, "");
    assert_eq!(p.selected, 0);
}

#[test]
fn fuzzy_matches_then_clear_query_restores_full_listing() {
    let items: Vec<PickItem<&'static str>> = ["apple", "banana", "cherry"]
        .iter()
        .map(|s| PickItem {
            label: (*s).into(),
            value: *s,
        })
        .collect();
    let mut p = Picker::new(items);
    p.push_query('a');
    let with_a = p.ranked().len();
    assert!(with_a >= 2);
    p.pop_query();
    assert_eq!(p.ranked().len(), 3);
}
