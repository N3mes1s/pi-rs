//! TALE-EP `<budget>N</budget>` parser is telemetry-only on the `hard`
//! route (RFD 0020 v1.1 §3) — never enforced at dispatch time. These
//! tests pin its parsing semantics so future runtime changes can rely
//! on a stable contract.

use pi_agent_core::parse_tale_ep_budget;

#[test]
fn budget_tag_with_pure_number_parses() {
    assert_eq!(parse_tale_ep_budget("<budget>1000</budget>"), Some(1000));
}

#[test]
fn budget_tag_inside_freeform_prompt() {
    let prompt = "prove this loop terminates <budget>500</budget> rest of the request";
    assert_eq!(parse_tale_ep_budget(prompt), Some(500));
}

#[test]
fn budget_tag_tolerates_inner_whitespace() {
    assert_eq!(
        parse_tale_ep_budget("<budget>  4096  </budget>"),
        Some(4096)
    );
}

#[test]
fn first_valid_budget_wins_when_multiple_present() {
    let prompt = "<budget>200</budget> ... <budget>9999</budget>";
    assert_eq!(parse_tale_ep_budget(prompt), Some(200));
}

#[test]
fn malformed_first_tag_is_skipped_in_favour_of_later_valid_tag() {
    let prompt = "<budget>not-a-number</budget> then <budget>42</budget>";
    assert_eq!(parse_tale_ep_budget(prompt), Some(42));
}

#[test]
fn no_budget_tag_returns_none() {
    assert_eq!(parse_tale_ep_budget("rename foo to bar"), None);
}

#[test]
fn unclosed_tag_returns_none() {
    assert_eq!(parse_tale_ep_budget("<budget>123 oops"), None);
}
