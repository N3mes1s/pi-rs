You are working inside the pi-rs Cargo workspace at /home/user/Playground/pi-rs.

Task: add a thorough unit/integration test suite for the `pi-ai` crate.
Coverage targets: every public function in `auth`, `oauth`, `registry`,
`message`, `tool`, and the message-conversion helpers
`pi_ai::provider::anthropic::content_blocks_to_anthropic` and
`pi_ai::provider::openai::message_to_openai` should be exercised.

Place the tests in `crates/pi-ai/tests/<module>.rs` files (one per
target module). Use serde_json::json! for value comparisons. Do NOT
make any real network calls — for the providers, use the existing
`wiremock` dev-dependency to spin up a mock SSE server and pass its
URL via `ProviderConfig.base_url`.

Specifically:

1. `tests/auth.rs` — round-trip `AuthStorage::open` → `set` → reload
   from the same path. Cover `from_env`, `provider_names`, `remove`,
   and the `AuthMethod::OAuth` variant.

2. `tests/oauth.rs` — verify `Pkce::from_bytes` is deterministic and
   `Pkce::new()` produces a 43-byte URL-safe verifier. Verify
   `build_authorize_url` includes every required query parameter and
   percent-encodes them. `is_expired` must be true once `expires_at`
   is in the past, false otherwise. Mock the token endpoint with
   wiremock and assert `exchange_code` returns the parsed
   `TokenResponse`. Use `OAuthEndpoints::anthropic` for the input.

3. `tests/registry.rs` — `ModelRegistry::new` must register
   anthropic, openai and fireworks. `resolve` must work with
   `provider/model`, with bare alias, with full id, and return None
   for unknowns. `install` must add a new provider that `get` can
   then retrieve.

4. `tests/message.rs` — assert `Message::user_text/assistant_text/
   system_text` produce the expected role+content. `Usage::default()`
   is all zeros. `text()` concatenates only the Text blocks.

5. `tests/anthropic_conversion.rs` — call
   `pi_ai::provider::anthropic::content_blocks_to_anthropic` with each
   variant of `ContentBlock` (Text, Thinking, ToolUse, ToolResult,
   Attachment Image, Attachment File) and assert the produced JSON has
   the right shape (use serde_json::json! to compare).

6. `tests/openai_conversion.rs` — call
   `pi_ai::provider::openai::message_to_openai` with: an assistant
   message containing only text; an assistant message containing a
   ToolUse; a user message containing a ToolResult; and a user message
   with a Thinking block. Assert the resulting `Vec<Value>` is correct.

7. `tests/anthropic_stream.rs` — start a `wiremock` server returning a
   text/event-stream body that mimics the Anthropic SSE wire format
   (message_start, content_block_delta with text_delta, message_delta
   with stop_reason=end_turn, message_stop). Build an
   `AnthropicProvider` with that base_url, call `.generate(...)`, and
   assert the response message text matches. Use a dummy ApiKey for
   auth.

8. `tests/openai_stream.rs` — do the same against an OpenAI-style
   stream: choose chunks with `choices[0].delta.content` and a final
   `choices[0].finish_reason` plus a `usage` payload.

After writing them, run `cargo test -p pi-ai` and iterate until all
tests pass. Do not change any non-test source files unless a failing
test reveals a real bug — in which case, fix the bug and explain in a
single comment in the failing test what the bug was.

When done, output: DONE.
