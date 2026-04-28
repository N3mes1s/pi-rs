# RFD 0019 — OpenAI Responses API support

- **Status:** Discussion
- **Author:** pi-rs maintainers
- **Created:** 2026-04-28
- **Implemented:** &lt;pending&gt;

## Summary

Pi-rs talks to OpenAI exclusively through the legacy **Chat
Completions** endpoint (`/v1/chat/completions`,
`crates/pi-ai/src/provider/openai.rs:135`). OpenAI has shipped a
second, newer surface — the **Responses API**
(`/v1/responses`) — and the entire gpt-5 family plus the o-series
reasoning models route through it in the upstream
[pi-mono](https://github.com/badlogic/pi-mono) implementation we're
porting from. The most visible symptom of this gap surfaced during
the merge campaign of 2026-04-28: the bundled `code-reviewer`
subagent (model `gpt-5.4`) returned **HTTP 400 — "Unsupported
parameter: 'max_tokens' is not supported with this model. Use
'max_completion_tokens' instead"** on first invocation. The "right"
fix isn't a `max_tokens` → `max_completion_tokens` rename; it's that
gpt-5.x is a Responses-native model and Chat Completions support for
it will continue to degrade.

This RFD scopes the work to add a parallel `openai-responses`
provider path so pi-rs can dispatch reasoning-class OpenAI models
through the API OpenAI itself recommends.

## Background

### What pi-mono does

[pi-mono `packages/ai/src/providers/`](https://github.com/badlogic/pi-mono/tree/main/packages/ai/src/providers)
splits OpenAI into multiple sibling modules, each registered as a
distinct API provider:

```
openai-completions.ts        ← /v1/chat/completions (legacy)
openai-responses.ts          ← /v1/responses        (current)
openai-responses-shared.ts   ← message + stream conversion
openai-codex-responses.ts    ← Codex variant of Responses
azure-openai-responses.ts    ← Azure variant
```

The model registry tags every model with an `api:` field that picks
the dispatcher:

| pi-mono model id | api               |
| ---------------- | ----------------- |
| `gpt-5`          | `openai-responses` |
| `gpt-5.4`        | `openai-responses` |
| `gpt-5-mini`     | `openai-responses` |
| `o3`, `o4-mini`  | `azure-openai-responses` |
| `gpt-4o`         | `openai-completions` |

### What pi-rs has today

```rust
// crates/pi-ai/src/provider/openai.rs:135
let url = format!("{}/chat/completions", self.config.base_url);
```

Single endpoint. The streaming parser handles the Chat Completions
SSE shape (`choices[0].delta.{content,tool_calls}`,
`finish_reason`). The model registry has no `api_kind` field — every
OpenAI model goes to `chat/completions` regardless. There is no
reference to `/v1/responses`, `responses.create`, `output_text`,
`response.output_item.added`, or any other Responses-API token
anywhere under `crates/pi-ai/src/`.

### Wire-shape diff (Chat Completions vs Responses)

| Aspect              | Chat Completions                                                 | Responses                                                                  |
| ------------------- | ---------------------------------------------------------------- | -------------------------------------------------------------------------- |
| Path                | `POST /v1/chat/completions`                                      | `POST /v1/responses`                                                       |
| Top-level body keys | `model, messages, max_completion_tokens, temperature, stream, tools, tool_choice, stream_options` | `model, input, stream, prompt_cache_key, prompt_cache_retention, store, max_output_tokens, temperature, service_tier, tools, reasoning, include` |
| User content type   | `{role:"user", content:[{type:"text",…},{type:"image_url",…}]}` | `{role:"user", content:[{type:"input_text",…},{type:"input_image",…}]}` |
| Assistant content   | `{role:"assistant", content:"…", tool_calls:[…]}`               | `{type:"message", content:[{type:"output_text",…}]}` + sibling `{type:"function_call",…}` items |
| Tool wire format    | `{type:"function", function:{name, description, parameters}}`   | `{type:"function", name, description, parameters, strict}` (flat)          |
| Tool result         | `{role:"tool", tool_call_id, content}`                          | `{type:"function_call_output", call_id, output}`                           |
| Reasoning           | `reasoning_effort: "low"\|"medium"\|"high"` (header-ish; partial) | `reasoning: {effort: "low"\|"medium"\|"high"\|"none", summary: "auto"}` + `include: ["reasoning.encrypted_content"]` |
| Stop reason         | `finish_reason: "stop"\|"length"\|"tool_calls"\|"content_filter"` | `response.status: "completed"\|"incomplete"\|"failed"\|"cancelled"`        |

### Streaming events (Responses)

The Chat Completions SSE pattern is "one delta type, peel `.choices[0].delta`."
Responses fans out into 14 distinct events that pi-rs would need to
route on:

```
response.created
response.output_item.added             // item.type ∈ {reasoning, message, function_call}
response.reasoning_summary_part.added
response.reasoning_summary_text.delta
response.reasoning_summary_part.done
response.content_part.added
response.output_text.delta
response.refusal.delta
response.function_call_arguments.delta
response.function_call_arguments.done
response.output_item.done              // item.type as above; closes the block
response.completed                     // carries response.usage.{input_tokens, output_tokens, total_tokens}
response.failed
error
```

Stop-reason mapping per pi-mono `mapStopReason`:

| Responses status | pi internal |
| ---------------- | ----------- |
| `completed`      | `Stop`      |
| `incomplete`     | `Length`    |
| `failed`         | `Error`     |
| `cancelled`      | `Error`     |

## Proposal

### 1. `ApiKind` field on `ModelMeta`

`crates/pi-ai/src/registry.rs` — add an enum and per-model field:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiKind {
    /// `POST /v1/chat/completions` — legacy default, what every OpenAI
    /// model in pi-rs goes through today.
    ChatCompletions,
    /// `POST /v1/responses` — required for gpt-5.x and o-series.
    Responses,
}

pub struct ModelMeta {
    // …existing fields…
    pub api_kind: ApiKind,
}
```

Default to `ApiKind::ChatCompletions`. Bump these to `Responses`:

| Provider | Model       | Reason                           |
| -------- | ----------- | -------------------------------- |
| openai   | `gpt-5`     | Reasoning-class                  |
| openai   | `gpt-5.4`   | Reasoning-class (this campaign)  |
| openai   | `gpt-5-mini`| Reasoning-class                  |
| openai   | `gpt-5-nano`| Reasoning-class                  |
| openai   | `o3`        | Reasoning-class                  |
| openai   | `o3-pro`    | Reasoning-class                  |
| openai   | `o4-mini`   | Reasoning-class                  |
| openai   | `gpt-4o`    | Stay on Chat Completions for now |
| openai   | `gpt-4o-mini`| Stay on Chat Completions for now |

### 2. New module `crates/pi-ai/src/provider/openai_responses.rs`

Mirrors the existing `openai.rs` but targets `/v1/responses`. Same
public `Provider` impl; the divergence is in:

```rust
fn build_request_body(req: &ChatRequest) -> serde_json::Value {
    json!({
        "model": req.model,
        "input": messages_to_responses_input(&req.messages),
        "stream": true,
        "store": false,
        "prompt_cache_key": req.session_id,                    // optional
        "prompt_cache_retention": "short",
        "max_output_tokens": req.max_tokens,                   // renamed
        "temperature": req.temperature,
        "tools": req.tools.iter().map(tool_to_responses_tool).collect::<Vec<_>>(),
        "reasoning": effort_block(req.thinking),               // {effort, summary}
        "include": ["reasoning.encrypted_content"],            // when reasoning is on
    })
}
```

### 3. Helpers in `openai_responses.rs`

```rust
/// Convert pi-rs `Message` → Responses `input` items.
fn messages_to_responses_input(msgs: &[Message]) -> Vec<Value> { … }
//   Message::User text          ⇒ {role:"user",      content:[{type:"input_text",  text}]}
//   Message::User image         ⇒ {role:"user",      content:[{type:"input_image", image_url}]}
//   Message::System             ⇒ {role:"system",    content:[{type:"input_text",  text}]}
//   Message::Assistant text     ⇒ {type:"message",   content:[{type:"output_text", text}]}
//   Message::Assistant tool-use ⇒ {type:"function_call", call_id, name, arguments}
//   Message::ToolResult         ⇒ {type:"function_call_output", call_id, output}

/// Flatten the Chat-Completions {type:"function", function:{…}} tool wrapper
/// into the Responses {type:"function", name, description, parameters, strict} shape.
fn tool_to_responses_tool(t: &ToolDef) -> Value { … }

/// Translate pi-rs's existing `Thinking::{Off,Low,Medium,High,Adaptive}`
/// into `reasoning: {effort, summary}`. `Adaptive` maps to
/// `effort:"high", summary:"auto"` for now (Responses has no native
/// Anthropic-style adaptive; revisit if OpenAI ships one).
fn effort_block(t: Thinking) -> Option<Value> { … }
```

### 4. SSE event router

`crates/pi-ai/src/provider/openai_responses_stream.rs` — a 14-arm
match over `event["type"]` that pushes into pi-rs's existing
`AssistantEvent` channel. Sketch:

```rust
match event["type"].as_str() {
    Some("response.created")                          => /* no-op */ ,
    Some("response.output_item.added")                => start_block(&event),     // reasoning|message|function_call
    Some("response.reasoning_summary_text.delta")     => emit_thinking_delta(&event),
    Some("response.output_text.delta")                => emit_text_delta(&event),
    Some("response.function_call_arguments.delta")    => emit_tool_args_delta(&event),
    Some("response.function_call_arguments.done")     => emit_tool_args_done(&event),
    Some("response.output_item.done")                 => end_block(&event),
    Some("response.completed")                        => finalize_with_usage(&event),
    Some("response.failed") | Some("error")           => yield_error(&event),
    _                                                 => /* ignore unknown */ ,
}
```

`finalize_with_usage` populates `Usage.{input_tokens, output_tokens,
total_tokens, cache_read_tokens}` — same contract as RFD 0008 / 0015.

### 5. Dispatch

`OpenAiProvider::stream` becomes a 6-line dispatcher:

```rust
pub async fn stream(&self, req: ChatRequest) -> Result<EventStream> {
    let meta = registry::lookup(&self.config.name, &req.model)?;
    match meta.api_kind {
        ApiKind::ChatCompletions => self.stream_chat_completions(req).await,
        ApiKind::Responses       => self.stream_responses(req).await,
    }
}
```

### 6. Pricing / cache discount

The existing differential cache pricing path (RFD 0010) uses
`Usage.cache_read_tokens` / `cache_creation_tokens`. Responses
returns `usage.input_tokens.cached_tokens` (sub-field) — wire it
through `finalize_with_usage` so the cost math stays identical.

## Test plan

1. **`tests/openai_responses_request_shape.rs`** — given a fixed
   `ChatRequest` with one user message + one tool def, assert the
   serialized JSON body equals a golden file (input items, tool
   shape, reasoning block).
2. **`tests/openai_responses_stream_parse.rs`** — feed a captured
   SSE log (committed under `tests/data/openai_responses/*.sse`)
   covering: text-only, reasoning + text, tool call, multi-tool
   round, error. Assert the resulting `Vec<AssistantEvent>` matches
   golden.
3. **`tests/openai_responses_usage_population.rs`** — assert
   `Usage` is fully populated from `response.completed` (RFD 0008
   contract — same test pattern as the openai-compat fix).
4. **`tests/openai_dispatch_router.rs`** — `gpt-5.4` routes to
   Responses, `gpt-4o` routes to Chat Completions; assert via a
   mock HTTP recorder that exactly one POST hits the right path.
5. **End-to-end manual** — `pi --provider openai --model gpt-5.4
   -p "What's 2+2?"` returns a clean answer via Responses and
   stats records the right cost. Re-run the same prompt under
   `gpt-4o` and confirm Chat Completions still works.

## Out of scope

- **Responses *stateful* mode** — `previous_response_id` / `store:
  true` for server-side conversation state. Pi-rs is currently
  stateless on the wire; making it stateful is a separate RFD
  with its own storage + invalidation story. Default `store:
  false`.
- **`text.verbosity` knob** — Responses lets the caller set
  output verbosity. Pi-rs has no UX for this yet; punt.
- **Encrypted reasoning persistence** — `include:
  ["reasoning.encrypted_content"]` returns blobs we'd need to
  thread back into the next request to keep the chain-of-thought
  cache hot. Worth a follow-up; v1 just discards.
- **Codex / Azure variants** — pi-mono has separate
  `openai-codex-responses.ts` and `azure-openai-responses.ts`.
  We can add `ApiKind::ResponsesCodex` and an Azure dispatcher
  later without revisiting this RFD.
- **Tightening Chat Completions for o-series** — once Responses
  ships, gpt-5.x and o-series stop using Chat Completions
  entirely; the `max_completion_tokens` rename remains in place
  as a fallback for any caller still on the old path.

## Open questions

- **Should `gpt-4o` migrate to Responses too?** Pi-mono keeps it
  on Chat Completions, presumably for parity with non-reasoning
  callers. Lean: leave `gpt-4o` on Chat Completions in v1; revisit
  after we have Responses-path coverage in production.
- **Do we want a `--api responses|completions` override flag for
  experimentation?** Useful for A/B-ing during the migration.
  Lean: yes, behind `--unsafe-api-override` (warn loudly).
- **How do we handle adaptive thinking?** Responses has fixed
  effort levels; pi-rs's `Thinking::Adaptive` (RFD 0003) is an
  Opus-side concept. Lean: map `Adaptive → high` for OpenAI
  Responses with a comment in `effort_block`.

## Implementation plan

This RFD is large enough to dogfood through pi-rs itself once the
2026-04-28 merge campaign clears (8 of 9 branches landed; monitor
RFD 0017 needs a rebase for the `rfd/README.md` index conflict).
Suggested split into worktreed Opus 4.7 sessions:

1. `claude/responses-registry`  — `ApiKind` enum + per-model field
   + dispatch stub. Tiny; doable solo.
2. `claude/responses-core`      — request body + message conversion
   + tool conversion + stream parser. Bulk of the work.
3. `claude/responses-tests`     — golden SSE fixtures + parse tests
   + usage population test.
4. `claude/responses-evolve`    — once #1–3 land, real-world
   exercise via the AGENTS.md evolve loop and judge subagent on
   `gpt-5.4`, then file follow-ups.

References:
* pi-mono Responses module: <https://github.com/badlogic/pi-mono/blob/main/packages/ai/src/providers/openai-responses.ts>
* pi-mono shared converter: <https://github.com/badlogic/pi-mono/blob/main/packages/ai/src/providers/openai-responses-shared.ts>
* OpenAI Responses API docs: <https://platform.openai.com/docs/api-reference/responses>
