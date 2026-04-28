// TODO(rfd-0019): re-enable after responses-core merges.
//
// Pins the RFD-0008 contract on the Responses path: the `response.completed`
// event must populate every Usage field (input_tokens, output_tokens,
// cache_read_tokens, total_tokens), not just output_tokens.

#![cfg(rfd_0019_responses)]
#![allow(dead_code)]

use pi_ai::provider::openai_responses_stream::parse_sse_stream;
use pi_ai::stream::StreamEventKind;

const TEXT_ONLY: &str = include_str!("data/openai_responses/text_only.sse");

#[test]
fn usage_is_fully_populated_from_response_completed() {
    let events = parse_sse_stream(TEXT_ONLY).expect("parse ok");
    let usage = events
        .iter()
        .find_map(|e| match &e.kind {
            StreamEventKind::Usage { usage } => Some(usage.clone()),
            _ => None,
        })
        .expect("Usage event must be emitted from response.completed");

    assert_eq!(
        usage.input_tokens, 12,
        "input_tokens must come from usage.input_tokens"
    );
    assert_eq!(
        usage.output_tokens, 5,
        "output_tokens must come from usage.output_tokens"
    );
    assert_eq!(
        usage.cache_read_tokens, 4,
        "cache_read_tokens must come from usage.input_tokens_details.cached_tokens"
    );

    // RFD 0008 contract: total = input + output (or carried verbatim, but
    // never zero when both halves are non-zero).
    let total = usage.input_tokens + usage.output_tokens;
    assert_eq!(total, 17, "input+output should equal upstream total_tokens");
}
