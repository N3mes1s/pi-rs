# VALIDATION — RFD 0011–0013 end-to-end loop

Empirical exercise of: AGENTS.md auto-load (0011), context-aware judge +
JSON flamegraph (0012), auto-apply evolve daemon (0013) — using the
prebuilt `pi` binary at
`/home/user/pi-rs/target/x86_64-unknown-linux-musl/debug/pi`.

## Sessions captured

Five intentional `pi --provider anthropic --model claude-opus-4-7 -p …`
runs, each in a fresh session under
`/home/user/quartet/validate-agent/sessions/_home_user_quartet_validate/`.
Costs / scores are summed from each session's `usage` + `outcome` events.

| # | session prefix | prompt (abridged) | input tok | output tok | cost | judge |
|---|----------------|-------------------|----------:|-----------:|-----:|------:|
| 1 | `0171bab4` | say exactly: ping | 6 942 | 6 | $0.03486 | *no outcome event* |
| 2 | `cba8e0c7` | AGENTS.md (0.5,1.5) placeholder, one sentence | 6 966 | 75 | $0.03671 | success / 0.95 |
| 3 | `2e4ec812` | Rust re-export pattern, two lines | 6 968 | 36 | $0.03574 | success / 0.95 |
| 4 | `6cdff70e` | Summarize RFD 0008, one sentence | 40 297 | 441 | $0.21251 | success / 0.95 |
| 5 | `2c5e9f78` | claude-opus-4-7 cost-per-MTok, two numbers | 29 561 | 412 | $0.15810 | **fail / 0.15** |

5-session total: **input 90 734 / output 970 / cost $0.47792**.

Run #4 and #5 ballooned because the model hit `bash` first, the
auto-approver denied it (headless policy → `ASK`), then it fell back to
`find` / `grep` / `read`. Each tool round added a full prefix to the
context, so input tokens 5–6× the trivial cases.

## Stats totals (`artifacts/stats.json`)

`pi --stats sync` reported `scanned 6 file(s), inserted 17 row(s)` —
the 6th file is *this* validation session itself. Roll-up:

```
total_requests:       17
total_input_tokens:   135 226
total_output_tokens:  2 080
total_cache_read_tok: 0
total_cost:           $0.72813
error_count:          0
```

The 17 requests vs. 5 prompts gap is expected: the agentic sessions
generate one `usage` event per provider turn (run #4 = 5 turns, run #5
= 4 turns, the validation session itself = 10 turns). All 17 attribute
to `claude-opus-4-7` / `anthropic` / folder
`/home/user/quartet/validate`. ✔ `total_input_tokens > 0`,
✔ `total_cost > 0`. **✗ `requests == 5`** is the wrong invariant for
agentic sessions; the underlying `usage` row count is per-turn, not
per-session. (See Findings.)

## Flamegraph artefact (`artifacts/flamegraph.json`)

Richest session by token count = `6cdff70e` (estimated 40 738 tokens).

- `turns`: **2** (turn 1 = session start + AGENTS.md context_load;
  turn 2 = the user prompt + 5 assistant_text rounds + 4 tool_results).
- Fattest `assistant_text.cost_usd`: **$0.06252** — but every one of the
  5 `assistant_text` blocks in turn 2 reports the **same** $0.06252
  value (sum across all blocks = $0.31263, which is ~1.47× the
  session's actual $0.21251). See Findings.
- Block kinds emitted: `meta`, `context_load`, `user`, `assistant_text`,
  `tool_error`, `tool_result` — RFD 0012 schema fully populated.

## Evolve dry-run (`artifacts/evolve.txt`)

`pi --evolve dry-run` ran cleanly (exit 0). It loaded AGENTS.md, parsed
**4 mutable sections** (`Where things live`, `Conventions`, `Current
open RFDs`, `Optimisation lessons` — `House rules` correctly excluded
via the `<!-- pi:keep -->` fence). It loaded **4 cases** from session
outcomes (3 wins, 1 loss; the no-outcome `ping` session is excluded —
correct).

Gate decision: **SKIP — `InsufficientSamples { have: 4, need: 30 }`**.
RFD 0013's safety floor refused to mutate on 4 samples and made
**no model call**. No candidate body was generated; the printout shows
the *prompt that would have been sent* (template populated with the 3
wins and 1 loss verbatim) but `(no model call made; AGENTS.md
untouched)`.

This is the designed behaviour for n<30, so "no candidate" is a pass,
not a bug.

## Findings

1. **Stats `requests` field is per-turn, not per-session.** 5 prompts
   produced 17 rows. The task spec's "confirm `requests == 5`" check
   would fail on any agentic session. Either rename to
   `total_provider_turns` or add a `total_sessions` field. Minor doc
   issue, no functional bug.

2. **Flamegraph `cost_usd` is duplicated per assistant block.** In
   session `6cdff70e`, all 5 `assistant_text` blocks in turn 2 carry
   the *same* `cost_usd: 0.06252` figure. Summing them yields $0.31263,
   ~1.47× the real session cost ($0.21251 from `usage` events). Looks
   like the renderer is attaching one turn-level cost to every assistant
   block instead of pro-rating per-block (or only on the final block).
   This will mislead anyone using the flamegraph to find expensive
   blocks. **Concrete bug in RFD 0012.**

3. **No-outcome session for trivial prompts.** Session `0171bab4`
   (`say exactly: ping`, output = "ping") never got a judge `outcome`
   event, while the other four prompts did. The judge appears to skip
   sessions with output below some token threshold, or only fires when
   tool calls happen. Either way the evolve gate's case loader silently
   drops these — fine for a content-thin run, but if the threshold ever
   excludes a *meaningful* short answer, the daemon will overweight
   long-tool sessions. Worth documenting.

4. **Headless `ASK` for `bash` cost the experiment ~$0.30.** Run #4 and
   run #5 each tried `bash` first, hit `AUTO-APPROVE BLOCKED:
   user-confirmation required (headless): policy says ASK for tool
   bash`, then retried with safer tools. The retry ate 5×–6× the input
   tokens of the trivial prompts. The headless policy is correct, but
   the model isn't learning from it within a session — the
   `tool_error` reply doesn't seem to bias subsequent tool choice in
   the same prompt. Cheap evolve target.

5. **Judge misjudgement on run #5 is correct (fail/0.15)** — the model
   pulled `(0.5, 1.5)` from `pricing.json` (the AGENTS.md-flagged
   placeholder) for `claude-opus-4-7` instead of the real $5/$25.
   Validates that the judge can punish AGENTS.md violations.

6. **AGENTS.md auto-load works.** Every session shows a
   `context_load` event with path
   `/home/user/quartet/validate/AGENTS.md` and ~1 046 tokens. RFD 0011
   ✔.

## Verdict

**Loop works but needs two fixes:** the flamegraph per-block
`cost_usd` is duplicated across assistant blocks in a turn (over-states
total ~1.47× on the richest session) and the stats roll-up uses
"requests" to mean per-turn rather than per-session. Everything else
— AGENTS.md auto-load, judge scoring, evolve sample gate, dry-run
prompt rendering — behaves exactly as the RFDs specify.
