//! Per RFD 0028 §D.4 (JSONL stdout parser) + §D.5 (spend
//! attribution). Pure functions over `AgentEvent` slices —
//! no I/O. The subprocess module (`subprocess.rs`) drives the
//! line reader; this module just parses + extracts.
//!
//! Forward-compat stance: a single malformed JSONL line MUST
//! NOT abort the cycle. `parse_event_line` returns `None` and
//! the caller is expected to log the bad line + keep reading.
//! This handles the case where a future Commit B revision adds
//! a new `AgentEventKind` variant that this halo doesn't know
//! (serde_json::from_str fails — we drop the line, cycle
//! continues).

use thiserror::Error;

use pi_sdk::cost::{estimate_cost_usd, CostRegistry};
use pi_sdk::{AgentEvent, AgentEventKind};

/// Parse a single line of JSONL stdout into an `AgentEvent`.
/// Returns `None` if the line is empty / whitespace, or if it
/// fails to deserialize (unknown variant, malformed JSON, etc.).
/// On parse failure the caller SHOULD log the bad line — don't
/// silently swallow it, but don't abort the cycle either.
pub fn parse_event_line(line: &str) -> Option<AgentEvent> {
    if line.trim().is_empty() {
        return None;
    }
    match serde_json::from_str::<AgentEvent>(line) {
        Ok(evt) => Some(evt),
        Err(e) => {
            tracing::warn!(
                line = %trim_to_log(line, 200),
                error = %e,
                "compiled-agent JSONL parse failed; skipping line"
            );
            None
        }
    }
}

fn trim_to_log(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_owned()
    } else {
        format!("{}…", &s[..max])
    }
}

/// Errors `cycle_spend` can return.
#[derive(Debug, Error)]
pub enum CycleSpendError {
    #[error("compiled agent emitted Usage event before any SessionStarted; cannot determine model_id for cost lookup")]
    NoSessionStarted,
}

/// Per RFD 0028 §D.5: precise spend attribution from a compiled
/// agent's `AgentEvent` stream.
///
/// Walks the event slice for the first `SessionStarted` to extract
/// `model_id`, then sums `Usage` events × `pi_sdk::cost::estimate_cost_usd`.
/// Receiving a `Usage` event before any `SessionStarted` is a hard
/// cycle abort (`CycleSpendError::NoSessionStarted`) — pi-sdk
/// guarantees `SessionStarted` precedes any Usage event in the
/// pump, so violating that invariant means something is wrong with
/// the agent's wire format.
pub fn cycle_spend(
    events: &[AgentEvent],
    pricing: &CostRegistry,
) -> Result<f64, CycleSpendError> {
    let model_id = events
        .iter()
        .find_map(|e| match &e.kind {
            AgentEventKind::SessionStarted { model, .. } => Some(model.as_str()),
            _ => None,
        })
        .ok_or(CycleSpendError::NoSessionStarted)?;

    let total: f64 = events
        .iter()
        .filter_map(|e| match &e.kind {
            AgentEventKind::Usage { usage } => Some(usage),
            _ => None,
        })
        .map(|usage| estimate_cost_usd(usage, model_id, pricing))
        .sum();

    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_sdk::cost::Pricing;
    use pi_sdk::Usage;

    fn ev(kind: AgentEventKind) -> AgentEvent {
        AgentEvent {
            session_id: "s1".into(),
            entry_id: "e1".into(),
            timestamp: 0,
            kind,
        }
    }

    fn session_started_anthropic() -> AgentEvent {
        ev(AgentEventKind::SessionStarted {
            id: "s1".into(),
            cwd: "/work".into(),
            model: "claude-haiku-4-5-20251001".into(),
            provider: "anthropic".into(),
        })
    }

    fn usage(input: u64, output: u64) -> AgentEvent {
        ev(AgentEventKind::Usage {
            usage: Usage {
                input_tokens: input,
                output_tokens: output,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                reasoning_tokens: 0,
                cost_usd: 0.0,
            },
        })
    }

    #[test]
    fn parse_event_line_skips_blank() {
        assert!(parse_event_line("").is_none());
        assert!(parse_event_line("   \t  ").is_none());
    }

    #[test]
    fn parse_event_line_skips_garbage() {
        assert!(parse_event_line("not json").is_none());
        assert!(parse_event_line("{partial").is_none());
    }

    #[test]
    fn parse_event_line_skips_unknown_variant() {
        // Forward-compat: a v2-introduced variant fails to deserialize
        // (serde rejects unknown variants on `#[serde(tag = "type")]`
        // enums by default). MUST be a quiet skip, not a panic.
        let line = r#"{"session_id":"s","entry_id":"e","timestamp":0,"kind":{"type":"future_variant","x":1}}"#;
        assert!(parse_event_line(line).is_none());
    }

    #[test]
    fn parse_event_line_round_trips_known_variants() {
        let evt = usage(1000, 500);
        let line = serde_json::to_string(&evt).unwrap();
        let parsed = parse_event_line(&line).expect("known variant must parse");
        assert_eq!(parsed.session_id, "s1");
        match parsed.kind {
            AgentEventKind::Usage { usage } => {
                assert_eq!(usage.input_tokens, 1000);
                assert_eq!(usage.output_tokens, 500);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn cycle_spend_sums_usage_events_with_explicit_pricing() {
        let mut reg = CostRegistry::empty();
        reg.override_for(
            "claude-haiku-4-5-20251001",
            Pricing::flat(1.0, 5.0), // $1/MTok input, $5/MTok output
        );
        let events = vec![
            session_started_anthropic(),
            usage(1_000_000, 500_000), // $1.00 input + $2.50 output = $3.50
            usage(0, 100_000),         // $0.50 output
        ];
        let total = cycle_spend(&events, &reg).expect("spend ok");
        // 3.50 + 0.50 = 4.00 within float epsilon.
        assert!((total - 4.0).abs() < 1e-9, "expected ~4.00, got {total}");
    }

    #[test]
    fn cycle_spend_no_session_started_is_hard_error() {
        let events = vec![usage(1000, 500)];
        let err = cycle_spend(&events, &CostRegistry::empty()).expect_err("no session started");
        assert!(matches!(err, CycleSpendError::NoSessionStarted), "{err:?}");
    }

    #[test]
    fn cycle_spend_zero_usage_events_returns_zero() {
        let events = vec![session_started_anthropic()];
        let total = cycle_spend(&events, &CostRegistry::empty()).expect("ok");
        assert_eq!(total, 0.0);
    }

    #[test]
    fn cycle_spend_unknown_model_uses_default_zero_pricing() {
        // No override registered for this model; `estimate_cost_usd`
        // falls through to a 0/0 Pricing per pi-sdk's contract.
        let events = vec![
            session_started_anthropic(),
            usage(1_000_000, 500_000),
        ];
        let total = cycle_spend(&events, &CostRegistry::empty()).expect("ok");
        assert_eq!(total, 0.0, "unknown model → zero spend per pi-sdk default");
    }
}
