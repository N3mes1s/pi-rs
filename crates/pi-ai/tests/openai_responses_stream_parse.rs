// TODO(rfd-0019): re-enable after responses-core merges.
//
// This file pins the SSE-parser behavior for the OpenAI Responses API.
// It depends on `pi_ai::provider::openai_responses_stream::parse_sse`
// (or equivalent) which lands with claude/responses-core. Until then
// the tests are gated behind the `rfd_0019_responses` cfg so the crate
// still `cargo check --tests -p pi-ai` cleanly.

#![cfg(rfd_0019_responses)]
#![allow(dead_code)]

use pi_ai::message::FinishReason;
use pi_ai::provider::openai_responses_stream::parse_sse_stream;
use pi_ai::stream::{StreamEvent, StreamEventKind};

const TEXT_ONLY: &str = include_str!("data/openai_responses/text_only.sse");
const REASONING_THEN_TEXT: &str = include_str!("data/openai_responses/reasoning_then_text.sse");
const TOOL_CALL: &str = include_str!("data/openai_responses/tool_call.sse");
const MULTI_TOOL_ROUND: &str = include_str!("data/openai_responses/multi_tool_round.sse");
const ERROR_SSE: &str = include_str!("data/openai_responses/error.sse");

fn parse(body: &str) -> Vec<StreamEvent> {
    parse_sse_stream(body).expect("parse_sse_stream ok")
}

#[test]
fn text_only_emits_only_text_deltas_and_usage() {
    let events = parse(TEXT_ONLY);
    let texts: Vec<&str> = events
        .iter()
        .filter_map(|e| match &e.kind {
            StreamEventKind::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(texts.join(""), "Hello, world!");

    // No reasoning, no tool calls.
    assert!(!events
        .iter()
        .any(|e| matches!(e.kind, StreamEventKind::ThinkingDelta { .. })));
    assert!(!events
        .iter()
        .any(|e| matches!(e.kind, StreamEventKind::ToolCallStart { .. })));

    // Usage was emitted.
    let usage = events
        .iter()
        .find_map(|e| match &e.kind {
            StreamEventKind::Usage { usage } => Some(usage.clone()),
            _ => None,
        })
        .expect("usage event present");
    assert_eq!(usage.input_tokens, 12);
    assert_eq!(usage.output_tokens, 5);
    assert_eq!(usage.cache_read_tokens, 4);

    // Stop reason.
    let finish = events
        .iter()
        .find_map(|e| match &e.kind {
            StreamEventKind::Finish { reason } => Some(*reason),
            _ => None,
        })
        .expect("finish event present");
    assert!(matches!(finish, FinishReason::Stop));
}

#[test]
fn reasoning_then_text_emits_thinking_before_text() {
    let events = parse(REASONING_THEN_TEXT);

    let mut saw_thinking = false;
    let mut saw_text_after_thinking = false;
    for e in &events {
        match &e.kind {
            StreamEventKind::ThinkingDelta { .. } => saw_thinking = true,
            StreamEventKind::TextDelta { .. } if saw_thinking => saw_text_after_thinking = true,
            _ => {}
        }
    }
    assert!(saw_thinking, "expected at least one ThinkingDelta event");
    assert!(
        saw_text_after_thinking,
        "expected a TextDelta after the ThinkingDelta(s)"
    );

    let thinking: String = events
        .iter()
        .filter_map(|e| match &e.kind {
            StreamEventKind::ThinkingDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(thinking, "Thinking about the answer.");
}

#[test]
fn tool_call_assembles_arguments_json() {
    let events = parse(TOOL_CALL);

    let starts: Vec<_> = events
        .iter()
        .filter(|e| matches!(e.kind, StreamEventKind::ToolCallStart { .. }))
        .collect();
    assert_eq!(starts.len(), 1, "expected one ToolCallStart");

    let complete = events
        .iter()
        .find_map(|e| match &e.kind {
            StreamEventKind::ToolCallComplete { id, name, input } => {
                Some((id.clone(), name.clone(), input.clone()))
            }
            _ => None,
        })
        .expect("ToolCallComplete present");
    assert_eq!(complete.0, "call_abc");
    assert_eq!(complete.1, "echo_tool");
    assert_eq!(complete.2, serde_json::json!({"message":"hi"}));
}

#[test]
fn multi_tool_round_emits_two_tool_calls_in_order() {
    let events = parse(MULTI_TOOL_ROUND);

    let calls: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.kind {
            StreamEventKind::ToolCallComplete { id, name, input } => {
                Some((id.clone(), name.clone(), input.clone()))
            }
            _ => None,
        })
        .collect();

    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].0, "call_aaa");
    assert_eq!(calls[0].2, serde_json::json!({"message":"first"}));
    assert_eq!(calls[1].0, "call_bbb");
    assert_eq!(calls[1].2, serde_json::json!({"message":"second"}));
}

#[test]
fn error_event_yields_error_kind() {
    let events = parse(ERROR_SSE);
    let err = events
        .iter()
        .find_map(|e| match &e.kind {
            StreamEventKind::Error { message } => Some(message.clone()),
            _ => None,
        })
        .expect("Error event present");
    assert!(
        err.contains("upstream timeout"),
        "error message should propagate from response.failed: got {err:?}"
    );
}
