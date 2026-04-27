You are working inside the pi-rs Cargo workspace at /home/user/Playground/pi-rs.

Goal: register the rest of the OpenAI-compatible providers in
`pi_ai::registry`. No new code is needed — they all use
`OpenAiCompatProvider`.

Add `ProviderConfig` entries for each of these in
`crates/pi-ai/src/registry.rs::default_providers()`:

| name | base_url | sample models |
|---|---|---|
| cerebras | https://api.cerebras.ai/v1 | llama3.1-70b |
| groq | https://api.groq.com/openai/v1 | llama-3.3-70b-versatile |
| xai | https://api.x.ai/v1 | grok-2-latest |
| openrouter | https://openrouter.ai/api/v1 | (empty list — users pick) |
| deepseek | https://api.deepseek.com | deepseek-chat, deepseek-reasoner |
| mistral | https://api.mistral.ai/v1 | mistral-large-latest |
| zai | https://api.z.ai/api/paas/v4 | glm-4.6 |
| huggingface | https://api-inference.huggingface.co/v1 | (empty) |
| ollama | http://localhost:11434/v1 | (empty) |
| kimi | https://api.moonshot.cn/v1 | moonshot-v1-128k |
| minimax | https://api.minimax.chat/v1 | (empty) |

All use auth_header `Authorization`, auth_format `Bearer {token}`,
kind `ProviderKind::OpenAiCompat`. Use the `m()` helper for models
with reasonable context_window/max_tokens/cost values; if you don't
know exact values, use `131_072 / 8_192 / 0.5 / 1.5` as defaults.

Add ENV_KEYS entries:
- `("cerebras", "CEREBRAS_API_KEY")`
- `("groq", "GROQ_API_KEY")`
- `("xai", "XAI_API_KEY")`
- `("openrouter", "OPENROUTER_API_KEY")`
- `("deepseek", "DEEPSEEK_API_KEY")`
- `("mistral", "MISTRAL_API_KEY")`
- `("zai", "ZAI_API_KEY")`
- `("huggingface", "HF_TOKEN")`
- `("ollama", "OLLAMA_API_KEY")` (Ollama doesn't need auth, but
  having the env handle is fine)
- `("kimi", "MOONSHOT_API_KEY")`
- `("minimax", "MINIMAX_API_KEY")`

Tests in `crates/pi-ai/tests/registry_compat.rs`:
- assert each new provider name is in
  `ModelRegistry::new(...).providers().map(|c| c.name).collect()`.
- assert resolving aliases like "deepseek-chat", "grok-2-latest"
  works.
- env-loading: with `XAI_API_KEY=foo`,
  `AuthStorage::from_env().get("xai")` returns
  `AuthMethod::ApiKey { value: "foo" }`.

Build clean: `cargo build --workspace`
Tests green: `cargo test -p pi-ai --test registry_compat`

When done output: DONE.
