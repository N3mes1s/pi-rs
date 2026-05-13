//! Property tests for the picker.

use pi_coding_agent::picker::{PickItem, Picker};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn move_down_then_move_up_returns_to_zero_modulo(
        labels in proptest::collection::vec("[a-z]{1,8}", 1..16),
        n in 0usize..50,
    ) {
        let items: Vec<PickItem<usize>> = labels
            .into_iter()
            .enumerate()
            .map(|(i, l)| PickItem { label: l, value: i })
            .collect();
        let mut p = Picker::new(items);
        // empty query => ranked() returns up to `limit` items in the original order.
        let len = p.ranked().len();
        prop_assume!(len > 0);

        for _ in 0..n {
            p.move_down();
        }
        for _ in 0..n {
            p.move_up();
        }
        prop_assert_eq!(p.selected, 0);
    }
}
