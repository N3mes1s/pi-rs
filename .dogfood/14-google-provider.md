You are working inside the pi-rs Cargo workspace at /home/user/Playground/pi-rs.

Goal: native Google Generative AI provider (Gemini API).

Background:
- Existing providers live in `crates/pi-ai/src/provider/`:
  `anthropic.rs` and `openai.rs`. Both implement the
  `pi_ai::provider::Provider` trait.
- Wire format: POST to
  `{base_url}/v1beta/models/{model}:streamGenerateContent?alt=sse&key={api_key}`
  with body:
      {
        "contents": [
          {"role": "user", "parts": [{"text": "..."}]},
          {"role": "model", "parts": [...]}
        ],
        "systemInstruction": {"parts": [{"text": "..."}]},
        "generationConfig": {"maxOutputTokens": N, "temperature": T},
        "tools": [{"functionDeclarations": [...]}]
      }
  SSE events have JSON bodies with `candidates[0].content.parts[]`
  containing either `{"text": "..."}` or `{"functionCall": {"name":
  "...", "args": {...}}}`. `usageMetadata` carries token counts.
  `finishReason` lives on the candidate.

Step 1. Add `crates/pi-ai/src/provider/google.rs`:
- struct `GoogleProvider { config, auth, client }`, mirroring
  `AnthropicProvider`.
- implement the `Provider` trait via SSE streaming. Roles: User →
  "user", Tool → "user", Assistant → "model", System → folded into
  `systemInstruction`.
- map `ContentBlock::Text/Thinking/ToolUse/ToolResult/Attachment`
  to Gemini parts. `Thinking` wraps text in
  `<thinking>...</thinking>` since Gemini doesn't have a native
  reasoning trace concept.
- map finishReason: STOP → Stop, MAX_TOKENS → Length,
  SAFETY/RECITATION → Refusal, TOOL_USE/TOOL_CALL → ToolUse, _ → Other.
- `with_client(client)` for tests.

Step 2. In `crates/pi-ai/src/provider.rs` re-export `GoogleProvider`.
In `crates/pi-ai/src/registry.rs` add:
    ProviderConfig {
      name: "google",
      kind: ProviderKind::Google,  // new variant
      base_url: "https://generativelanguage.googleapis.com",
      auth_header: "x-goog-api-key",
      auth_format: "{token}",
      models: vec![
        m("google", "gemini-2.5-pro", Some("gemini-pro"), 1_000_000, 8_192, true, true, 1.25, 5.0),
        m("google", "gemini-2.5-flash", Some("gemini"), 1_000_000, 8_192, false, true, 0.075, 0.30),
      ],
    }
Add `ProviderKind::Google` variant. Update the `match` in
`pi-agent-core/src/runtime.rs::build_provider` to construct
`Box::new(GoogleProvider::new(...))`.

Step 3. Add `pi-ai::ENV_KEYS` entry: `("google", "GOOGLE_API_KEY")`.

Step 4. Tests in `crates/pi-ai/tests/google_stream.rs`:
- wiremock server returning a Gemini-style SSE stream with
  text-only response — assert text is captured.
- wiremock server returning a stream with a `functionCall` part —
  assert one ToolCall is emitted with the right name + input.
- 5xx response — assert AiError::Provider.
- response with finishReason=SAFETY — assert FinishReason::Refusal.

Build clean: `cargo build --workspace`
Tests green: `cargo test -p pi-ai --test google_stream`

When done output: DONE.
