You are working inside the pi-rs Cargo workspace at /home/user/Playground/pi-rs.

Goal: AWS Bedrock (Anthropic models) and Azure OpenAI providers.

These two are thin wrappers around existing wire formats:
- Bedrock streams the same Anthropic Messages JSON; differences:
  base URL is region-scoped, path is `/model/{id}/invoke-with-response-stream`,
  body has `anthropic_version: "bedrock-2023-05-31"` instead of
  `model`. For test purposes we accept a pre-signed bearer
  (`AuthMethod::ApiKey`) so wiremock doesn't need real SigV4.
- Azure OpenAI uses the OpenAI Chat Completions wire format,
  but the path is `/openai/deployments/{deployment}/chat/completions?api-version=...`,
  and the auth header is `api-key` (not `Authorization: Bearer`).

Step 1. Add `crates/pi-ai/src/provider/bedrock.rs`:
- `BedrockAnthropicProvider { config, auth, client, region }`
- new(): default region from `AWS_REGION` or "us-east-1"
- `with_region(s)`, `with_client(c)` builders
- `stream` builds the Anthropic body (delegate to
  `super::anthropic::content_blocks_to_anthropic`), POSTs to
  `{base}/model/{model.id}/invoke-with-response-stream`, parses
  Anthropic SSE events identically to AnthropicProvider.

Step 2. Add `crates/pi-ai/src/provider/azure.rs`:
- `AzureOpenAiProvider { config, auth, client, api_version }`
- new(): default api_version "2024-10-21"
- `with_api_version(s)`, `with_client(c)` builders
- `stream` reuses `super::openai::message_to_openai`, POSTs to
  `{base}/openai/deployments/{model.id}/chat/completions?api-version={version}`,
  parses chunks identically to OpenAiProvider.

Step 3. Update `crates/pi-ai/src/provider.rs`:
- `pub mod bedrock; pub mod azure;`
- `pub use bedrock::BedrockAnthropicProvider;`
- `pub use azure::AzureOpenAiProvider;`
- Add `Bedrock` and `Azure` variants to `ProviderKind`.

Step 4. Update `crates/pi-ai/src/registry.rs` to register both
providers (use the Anthropic Claude family for Bedrock, a generic
"deployment" for Azure with no models — users configure their own
deployment names).

Step 5. Update `crates/pi-agent-core/src/runtime.rs`'s
`build_provider` to dispatch on the new ProviderKinds.

Step 6. Tests:
- `crates/pi-ai/tests/bedrock_stream.rs`: wiremock server
  returning Anthropic SSE; assert text + finish.
- `crates/pi-ai/tests/azure_stream.rs`: wiremock server returning
  OpenAI-style SSE; assert text + finish.
- both: 5xx → AiError::Provider.

Step 7. Add ENV_KEYS entries for Bedrock (`AWS_BEDROCK_TOKEN`) and
Azure (`AZURE_OPENAI_API_KEY`).

Build clean: `cargo build --workspace`
Tests green: `cargo test -p pi-ai`

When done output: DONE.
