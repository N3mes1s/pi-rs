use pi_coding_agent::picker::{PickItem, Picker};

fn items() -> Vec<PickItem<&'static str>> {
    vec![
        PickItem { label: "alpha".into(), value: "alpha" },
        PickItem { label: "beta".into(), value: "beta" },
        PickItem { label: "gamma".into(), value: "gamma" },
        PickItem { label: "delta".into(), value: "delta" },
    ]
}

#[test]
fn empty_query_returns_items_in_original_order() {
    let p = Picker::new(items());
    let r = p.ranked();
    let labels: Vec<&str> = r.iter().map(|(_, i)| i.label.as_str()).collect();
    assert_eq!(labels, vec!["alpha", "beta", "gamma", "delta"]);
}

#[test]
fn fuzzy_query_ranks_matches() {
    let mut p = Picker::new(items());
    p.push_query('a');
    let r = p.ranked();
    assert!(!r.is_empty(), "should have at least one match for 'a'");
    // Every result must contain the letter `a` somewhere.
    for (_, item) in &r {
        assert!(item.label.contains('a'));
    }
    // First-ranked must have a non-zero score.
    assert!(r[0].0 > 0);
}

#[test]
fn move_up_and_move_down_wrap_around() {
    let mut p = Picker::new(items());
    assert_eq!(p.selected, 0);
    p.move_up();
    // wraps to the last visible item (4 items, limit 20)
    assert_eq!(p.selected, 3);
    p.move_down();
    assert_eq!(p.selected, 0);
    p.move_down();
    assert_eq!(p.selected, 1);
}

#[test]
fn selected_value_returns_current() {
    let p = Picker::new(items());
    let v = p.selected_value().unwrap();
    assert_eq!(v, "alpha");
}

#[test]
fn pop_query_resets_selection() {
    let mut p = Picker::new(items());
    p.push_query('b');
    p.move_down();
    p.pop_query();
    assert_eq!(p.query, "");
    assert_eq!(p.selected, 0);
}
