You are working inside the pi-rs Cargo workspace at /home/user/Playground/pi-rs.

The current per-module coverage report (from `bash scripts/coverage.sh`) shows
two large gaps remaining:

1. `crates/pi-agent-core/src/runtime.rs` — 0% lines (411 missed). This
   is the central agent loop. We need to drive it with a mock
   `pi_ai::Provider` that returns canned stream events, and assert
   on the events emitted by `AgentSession`. After this, the file
   should be >85% lines.

2. `crates/pi-ai/src/provider/openai.rs` — 53% lines. The streaming
   branches not yet covered are `tool_calls` accumulation and the
   `[DONE]` tail flush. Drive these via a wiremock SSE server.

3. `crates/pi-ai/src/provider/anthropic.rs` — 73% lines. Cover the
   `Error` mid-stream case (a malformed `data:` line) and the
   `MessageStart` event emission.

Add tests in `crates/<crate>/tests/<module>_extra2.rs` (do NOT
overwrite existing files). Specifically:

A. `crates/pi-agent-core/tests/runtime_loop.rs`
   Implement a `MockProvider` struct in the test that:
   - holds a `Mutex<Vec<Vec<StreamEvent>>>` of canned responses
     (one Vec<StreamEvent> per turn);
   - implements `pi_ai::Provider`'s `stream` method by returning
     `futures::stream::iter(events.into_iter().map(Ok))` boxed;
   - returns `ProviderConfig` with kind=Anthropic, base_url="mock";
   - on each call, pops the next canned vec from the front.

   Build a minimal `RuntimeConfig` in-memory:
       SessionManager::in_memory()
       AuthStorage::in_memory() + set("anthropic", ApiKey)
       ModelRegistry::new(...)  — register a "mock" provider with the
         exact name your MockProvider returns? ANSWER: install a
         ProviderConfig named "anthropic" with one model alias "mock";
         then have AgentSession use that. Since the runtime calls
         `build_provider` based on `ProviderKind`, you can NOT
         intercept that. Instead, attach a custom Provider directly
         by exposing a small back-door — IF NEEDED, add a method
         `RuntimeConfig::with_provider_factory(...)` to runtime.rs.
         If the runtime does not currently support pluggable
         providers, this PR may be the right time to add a
         `ProviderFactory` trait and have `build_provider` consult
         it. Add the trait + a default impl that wraps the existing
         match, and store an `Option<Arc<dyn ProviderFactory>>` on
         RuntimeConfig. Default behaviour stays the same.
       ToolRegistry::new() (empty so the assistant produces final
         answer immediately).
   Then send `prompt("hello")`, drain the events, and assert:
   - `UserMessage`, `AssistantStart`, `AssistantTextDelta`,
     `AssistantMessage`, and `TurnComplete` were emitted in order.
   - The session manager has a Meta + User + Assistant entry on disk.

   Add a SECOND test that supplies a tool call: the canned events
   include a `ToolCallComplete` and `Finish { ToolUse }`, and the
   ToolRegistry has a stub tool that returns ok. Drive it; assert
   `AssistantToolCall` and `ToolResult` events are emitted, then a
   second turn happens that ends with `TurnComplete`.

   Add a THIRD test that triggers `compact_with_llm` with a mock
   provider returning a fixed summary; assert the compactor
   prepends a `<context_recap>` user message.

   Add a FOURTH test that triggers `abort()` during a turn and
   asserts an `Aborted` event is observed.

B. `crates/pi-ai/tests/openai_tool_use_stream.rs`
   wiremock SSE responses with multiple chunks:
       data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"foo","arguments":"{\"a\":"}}]}}]}
       data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"1}"}}]}}]}
       data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}
       data: [DONE]
   Assert `.generate(...)` returns one ToolCall with id=call_1,
   name="foo", input={"a":1}.

   Also add a test for the `[DONE]`-tail flush: the streaming
   server omits the explicit `finish_reason` and only sends the
   delta + [DONE]. Assert that we still see a `Finish::Stop` event.

C. `crates/pi-ai/tests/anthropic_error_branch.rs`
   - One test where wiremock returns a 500 with body "boom" — the
     stream() call should return `AiError::Provider { status: 500,
     body: "boom" }`.
   - One test where the SSE stream contains a non-JSON line; assert
     the stream skips it and still completes when followed by a
     valid `message_stop`.

After writing, run `cargo test --workspace` and iterate until
green. Then run `bash scripts/coverage.sh 2>&1 | tail -50` and report
the new TOTAL line.

Output: DONE.
