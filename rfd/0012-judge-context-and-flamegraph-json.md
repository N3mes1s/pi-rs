# RFD 0012 — Trajectory judge context-awareness + agent-readable flamegraph

- **Status:** Discussion
- **Author:** pi-rs maintainers
- **Created:** 2026-04-28
- **Implemented:** &lt;pending&gt;

## Summary

The trajectory judge (`crates/pi-coding-agent/src/native/trajectory/
judge.rs`) marks any session that didn't make tool calls as a likely
"fabrication" and scores it `success: false`. That's wrong when the
agent answered a question whose answer was in the system prompt
(AGENTS.md, CLAUDE.md, or any `discover_context_files` result). RFD
0011 surfaced this immediately: a one-question dogfood that pi
answered correctly from AGENTS.md scored `0.10` — judge said "no
tool calls, response appears fabricated."

Two root causes:

1. **The runtime never writes `SessionEntryKind::ContextLoad`
   entries.** `pi_agent_core::session::SessionEntryKind` defines
   the variant. The flamegraph + picker render it. But the
   *runtime* doesn't emit one when `discover_context_files`
   produces a non-empty list — `RuntimeConfig::context_files` goes
   straight into the system prompt, and no JSONL line records that
   it happened.
2. **The judge's user message never tells the judge what was in
   the system prompt.** `build_user_message` includes
   `<user_request>`, `<assistant_final_reply>`, `<features>`,
   `<action_digest>` — and stops. The judge sees zero evidence the
   agent ever had access to anything but the literal user prompt.

This RFD fixes both. Plus, since the user asked for it and the
evolve loop needs it: add a JSON output mode to `pi --flamegraph`
so the daemon (and any future agent) can ingest trajectory shape
without parsing HTML.

## Background

Live evidence from RFD 0011's smoke run:

```
$ PI_CODING_AGENT_DIR=/tmp/pi-self/agent pi -p "What does the AGENTS.md \
    in this repo say about the (0.5, 1.5) placeholder? One sentence."
The `(0.5, 1.5)` placeholder pricing pair in `crates/pi-ai/data/
pricing.json` is a forgotten audit … pick real prices from the
provider's public list page instead.

# session JSONL last line:
{ "kind": "outcome", "success": false, "score": 0.1,
  "source": "llm_judge",
  "notes": "{\"reason\":\"The agent provided a detailed answer about
   (0.5, 1.5) pricing placeholders, but made zero tool calls and took
   no actions to actually read AGENTS.md or any file in the repo. The
   response appears to be a fabrication or hallucination …\"}" }
```

The agent literally read AGENTS.md (via the system prompt) and
quoted it correctly. The judge marked it failure. With
`pi --evolve dry-run` consuming these scores, every honest
context-aware response will weigh against AGENTS.md instead of
for it. The whole evolution loop runs in the wrong direction.

References to existing pieces:

* `pi_agent_core::context::discover_context_files` returns a
  `Vec<ContextFile { path, content }>`.
* `pi_coding_agent::startup::assemble` builds
  `RuntimeConfig.context_files` from the discovered list and feeds
  it into the system prompt — but doesn't tell the session.
* `SessionEntryKind::ContextLoad { source, bytes, tokens }` already
  exists, complete with serde + tests
  (`crates/pi-agent-core/tests/trajectory_variants.rs:18-27`,
  `session_clone.rs:79`).
* The flamegraph (`flamegraph.rs:212`) renders ContextLoad rows as
  greenish bars. They just never appear in real sessions today.

## Proposal

### Part A — Emit ContextLoad entries from the runtime

`pi_agent_core::Runtime::prompt` (or
`AgentSessionRuntime::create_session`) walks
`self.cfg.context_files` once at session start (or first
`prompt`). For each entry, append:

```rust
self.cfg.session_manager.append(
    &self.id,
    SessionEntryKind::ContextLoad {
        source: ctx.path.display().to_string(),
        bytes:  ctx.content.len() as u64,
        tokens: estimate_tokens(&ctx.content),  // rough char/4 estimate
    },
);
```

Done **once per session** (not once per turn) — a `OnceCell` or a
"recorded" flag on the session inner state guards re-emission. The
write happens before the first `User` entry so the trajectory
ordering reads naturally for downstream consumers.

`estimate_tokens` is a small free helper:

```rust
fn estimate_tokens(s: &str) -> Option<u64> {
    // Rough approximation: 1 token ≈ 4 bytes for English text.
    // Off by ~20 % vs. real tokenizers; good enough for the judge.
    Some((s.len() as u64).div_ceil(4))
}
```

### Part B — Judge reads ContextLoad + system prompt size

`build_user_message` grows a fifth block:

```rust
fn build_user_message(branch: &[SessionEntry], features: &TrajectoryFeatures) -> String {
    let user_request   = first_user_text(branch).unwrap_or_else(|| "(no user message)".into());
    let final_reply    = last_assistant_text(branch).unwrap_or_else(|| "(no assistant reply)".into());
    let context_loaded = collect_context_loads(branch);          // NEW
    let features_json  = serde_json::to_string_pretty(features).unwrap_or_default();
    let digest         = action_digest(branch);

    format!(
        "<user_request>\n{}\n</user_request>\n\n\
         <context_loaded>\n{}\n</context_loaded>\n\n\
         <assistant_final_reply>\n{}\n</assistant_final_reply>\n\n\
         <features>\n{}\n</features>\n\n\
         <action_digest>\n{}\n</action_digest>",
        truncate(&user_request, 4000),
        context_loaded,
        truncate(&final_reply, 4000),
        features_json,
        digest,
    )
}

fn collect_context_loads(branch: &[SessionEntry]) -> String {
    let entries: Vec<String> = branch.iter().filter_map(|e| match &e.kind {
        SessionEntryKind::ContextLoad { source, bytes, tokens } => {
            Some(format!(
                "- {} ({} bytes, ~{} tokens)",
                source,
                bytes,
                tokens.unwrap_or(0),
            ))
        }
        _ => None,
    }).collect();
    if entries.is_empty() {
        "(no context files were loaded into the system prompt)".into()
    } else {
        entries.join("\n")
    }
}
```

The system-prompt block (`SYSTEM_PROMPT`) gets one new rule:

> A correct answer that quotes content from a `<context_loaded>`
> file is **not** a fabrication. Tool calls are evidence of work,
> not a prerequisite for success. If the agent's reply is grounded
> in a context file, score it on whether it answered the user's
> question — not on whether it re-fetched the file.

### Part C — `pi --flamegraph` JSON output

```rust
// crates/pi-coding-agent/src/cli.rs
#[arg(long = "flamegraph", value_name = "SESSION_OR_PATH")]
pub flamegraph: Option<String>,

/// Output format for `--flamegraph`: `html` (default) or `json`.
#[arg(long = "flamegraph-format", value_name = "FORMAT",
      value_parser = clap::builder::PossibleValuesParser::new(["html", "json"]))]
pub flamegraph_format: Option<String>,
```

The JSON shape is a flat array, agent-readable, no presentational
artefacts:

```jsonc
{
  "session_id":  "aefc9545-…",
  "estimated_tokens": 7046,
  "turns": [
    {
      "index":  1,
      "blocks": [
        { "kind": "meta",           "tokens": 0,   "label": "session start" }
      ]
    },
    {
      "index":  2,
      "blocks": [
        { "kind": "context_load",   "tokens": 1046, "label": "/home/user/pi-rs/AGENTS.md" },
        { "kind": "user",           "tokens": 22,   "label": "What does the AGENTS.md..." },
        { "kind": "assistant_text", "tokens": 47,   "label": "The (0.5, 1.5) placeholder..." },
        { "kind": "outcome",        "tokens": 0,    "label": "loss (0.10)",
          "outcome": { "success": false, "score": 0.10 } }
      ]
    }
  ]
}
```

Agents consume this directly: load it, walk `turns`, identify the
fattest assistant turns, cross-reference outcomes. The HTML view
becomes the same data with a `<style>` and `<svg>`-equivalent
layer on top — consider keeping them as two render functions over a
shared `Trajectory` struct.

### Wiring

```rust
// flamegraph.rs
pub fn render(branch: &[SessionEntry], format: Format) -> String {
    let trajectory = build(branch);              // NEW: shared model
    match format {
        Format::Html => render_html(&trajectory),
        Format::Json => serde_json::to_string_pretty(&trajectory).unwrap(),
    }
}
```

`bin/pi.rs` dispatches on `cli.flamegraph_format`.

## Test plan

1. **`tests/trajectory_recorder.rs`** — extend the existing test to
   assert the runtime appends exactly one `ContextLoad` entry per
   `RuntimeConfig.context_files` element, and that the entry
   appears in the JSONL *before* the first `User` entry.
2. **`tests/trajectory_judge.rs`** — extend the existing fake-judge
   test fixture to feed a branch with a non-empty `ContextLoad`
   list. Assert the user message passed to the (stub) judge model
   contains the literal `<context_loaded>` block with the source
   path. New regression test: a branch where the assistant
   answered without tool calls but referenced a context file's
   contents should score success when the judge stub returns a
   verdict template that respects the new rule.
3. **`tests/trajectory_flamegraph.rs`** — extend with a
   `--flamegraph-format json` round-trip:
   - serialise → deserialise into a small private struct.
   - assert `turns[].blocks[].kind` covers `meta`, `user`,
     `assistant_text`, `tool_call`, `tool_result`, `outcome`,
     `context_load` for the right fixture.
4. **End-to-end (gated on `ANTHROPIC_API_KEY`)**: re-run RFD
   0011's smoke
   ```
   pi -p "What does AGENTS.md say about (0.5, 1.5)?"
   ```
   expect the closing `Outcome` line to have `success: true,
   score >= 0.7` (was `false, 0.10` pre-fix).

## Out of scope

- **Tokenizer-accurate token counts in `ContextLoad.tokens`.** The
  rough char/4 estimate is enough for the judge's ranking.
  Plumbing a real tokenizer is RFD 0014.
- **Streaming flamegraph updates** — v1 is offline render against
  a closed session. Live updates while a session runs is a future
  TUI nicety.
- **Cross-session AGENTS.md drift detection** — comparing
  `bytes`/`tokens` fields across sessions to detect "AGENTS.md
  grew unboundedly" is a great signal; not in this RFD.
- **Skill-level context_load entries.** The skills loader does its
  own thing today; if a skill SKILL.md gets pulled into the prompt
  it should also emit ContextLoad. Reasonable; punt to a follow-up.

## Open questions

- **Should the judge's prompt rule against tool-call-as-success
  also tell it that a system_prompt > 5000 tokens is itself
  evidence the agent had context?** Lean yes — add it as a
  fall-through rule so the judge can still grade fairly even if no
  ContextLoad entries are present (e.g. when AGENTS.md is loaded
  via a non-`discover_context_files` code path).
- **Should `pi --flamegraph --format json` also include the per-
  turn `cost_usd` from the new pricing-aware `Usage` event?** Yes;
  trivially. Adds one `cost_usd` field to each turn block. Decided.
- **Where should `pi --flamegraph-format` live as a config
  default?** Punt to a follow-up. CLI flag for now.
