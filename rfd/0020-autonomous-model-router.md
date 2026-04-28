# RFD 0020 — Autonomous model router for pi-rs

- **Status:** Discussion (v1.1)
- **Author:** pi-rs maintainers
- **Created:** 2026-04-28
- **Implemented:** &lt;pending&gt;

## Revision history

- **v0.5 (44cd15c)** — initial draft. Three-stage pipeline
  (classifier → cascade w/ always-on judge deferral → TALE-EP
  budget). New `pi-router` crate. 8 milestones, 6 routes,
  ~4000 LOC.
- **v1.0 (6764130)** — applied first critique pass. Two-stage
  pipeline (classifier → escalate-on-failure), router lives as
  a module in `pi-agent-core`, TALE-EP demoted to opt-in,
  3 routes, 4 milestones (~1700 LOC).
- **v1.1 (this revision)** — applied second critique pass
  (gpt-5.4 thinking=xhigh, see commit-message body). Findings
  that mattered:
  * Stage 2 (escalate-on-failure) is **dropped from v1**.
    Detection is incomplete (silent semantic failures slip
    past `FinishReason::Stop`), the stop-reason vocabulary
    didn't match pi-rs's `FinishReason::*` enum, mid-stream
    escalation isn't implementable inside the existing
    `StreamInterceptor` shape, and the M3 LOC estimate was
    not believable. v1 is now a **measurement-only** ship; v2
    designs escalation against real failure-distribution data.
  * `Outcome` and `RoutingContext` had crate-boundary leaks
    (citing `pi_stats::Connection` / `JudgeVerdict` that
    `pi-agent-core` cannot depend on). Both reshaped to carry
    primitives only; adapters live in `pi-coding-agent`.
  * `cache_hit: bool` replaced with `cache_read_tokens` /
    `cache_write_tokens: u64` so the router can do real cost
    math (RFD 0010 already exposes these on `Usage`).
  * Acceptance criteria reworked: 100 sessions/arm at ±2 pp
    is statistically impossible (real CI ±13.9 pp, power ~6%).
    Either ≥1500 sessions/arm or a much wider guardrail.
  * Test plan paths refreshed (`crates/pi-router/...` →
    `crates/pi-agent-core/...`). Stale ETH-rule and TALE-hard-
    cap tests removed.
  * Citation softening on Anthropic 70-90% and aurelio "single-
    digit ms" claims.

## Summary

Pi-rs today picks its model the same way it did on day one: the
user passes `--model` / `--provider` / `--thinking` flags and that
choice carries unchanged through the entire session. Every prompt
— "rename a variable", "explain this trait bound", "do a
multi-file refactor across three crates", "audit the OpenAI
pricing page" — gets the same model and the same thinking budget.
That is a **flat dispatch**, and on a workload as heterogeneous
as agentic coding it leaves a lot of money on the floor: the
literature reports 70–98% cost reductions with no quality loss
when a routing or cascading layer is added (Anthropic's own
3-tier guidance, FrugalGPT, RouteLLM, vLLM Semantic Router,
ETH's unified cascade-routing). RFD 0019 just shipped the OpenAI
Responses API; pi-rs now has a healthy fleet of dispatchable
endpoints (Anthropic Opus/Sonnet/Haiku, OpenAI gpt-5/5.4/mini/
nano via Responses, gpt-4o via Chat Completions, o-series, Google
Gemini 2.5, Bedrock, OpenAi-compat for Cohere/DeepSeek/Groq/
Fireworks, plus the Ollama compat shim for local models). The
expensive infrastructure exists; what's missing is the layer
that decides **which one** to use per request.

This RFD proposes **a single-stage autonomous classifier**
inside pi-rs, plus the telemetry needed to design escalation
in v2:

1. **Tier-0 classifier** — a sub-50 ms embedding-cosine
   classifier over a small set of named routes
   (`fast`, `default`, `hard`) that maps each request to a
   `(provider, model, thinking)` tuple, plus a `fallback_chain`
   the *user* can drive manually via `pi --model X` if the
   classifier picks wrong. Architectural inspiration:
   aurelio-labs's embedding-cosine router (zero-training
   baseline) and vLLM Semantic Router v0.1 "Iris" (BERT
   classifier).
2. **Telemetry** — every classified request emits a
   `routing_decision` session entry; `pi-stats` aggregates
   per-route cost and judge-pass rate. **The data this
   produces is the input v2 needs to design escalation
   correctly.**

**No escalation-on-failure in v1.** The v1.0 draft included a
Stage-2 escalation layer ("walk fallback_chain on parse error /
non-stop / 5xx"). The second critique surfaced that escalation
detection is incomplete (silent semantic failures pass through
with `FinishReason::Stop`), the runtime's `StreamInterceptor`
hooks only `TextDelta` (not `ToolCallStart` / `ToolInputDelta`),
and the LOC estimate was unbelievable. We ship v1 as
measurement-only and design escalation in v2 against real
failure-distribution data from pi-stats.

**TALE-EP** (token-budget self-prediction) is **opt-in per
route, telemetry-only**. v1 parses the `&lt;budget&gt;` tag for the
`(predicted, actual)` pair into pi-stats; the runtime does
**not** enforce the cap. Whether to enforce in v2 is a
data-driven decision.

Anthropic's manual 3-tier guidance (Sonnet default, Haiku
triage, Opus / extended-thinking Sonnet escalation) is the
**empirical floor** v1 must beat: a learned classifier should
match or exceed it on cost at iso-quality. The 70-90% cost-
reduction figure cited by the community is anecdotal — pi-rs
will measure its own number on its own workload, not assume.

Pi-rs's existing primitives slot in directly: `ModelRegistry`
becomes the routing target catalogue, `pi-stats` is the
empirical cost-per-task source for offline router training, the
trajectory judge is the deferral signal, the evolve daemon
(RFD 0011-0013) is retargeted to optimize routing decisions, and
Ollama (already wired as an OpenAi-compat provider at
`crates/pi-ai/src/registry.rs:484-491`) is the free baseline
tier. The router is **opt-in** behind a `--route auto` flag for
v1, becomes the default in v2 once empirical Pareto curves on
pi-stats data clear Anthropic's own 3-tier baseline.

## Background

### What pi-rs already has

An audit of the routing-relevant primitives, with file:line
references, so the proposal can lean on existing code rather
than re-inventing.

#### Model registry (`crates/pi-ai/src/registry.rs`)

The struct (post-RFD 0019) includes:

```rust
pub struct ModelInfo {
    pub provider:                String,
    pub id:                      String,
    pub alias:                   Option&lt;String&gt;,
    pub context_window:          u32,
    pub max_output_tokens:       u32,
    pub supports_thinking:       bool,
    pub supports_tools:          bool,
    pub supports_vision:         bool,
    pub input_cost_per_mtok:     f64,
    pub output_cost_per_mtok:    f64,
    pub cache_read_cost_per_mtok:  Option&lt;f64&gt;,  // RFD 0010
    pub cache_write_cost_per_mtok: Option&lt;f64&gt;,  // RFD 0010
    pub api_kind:                ApiKind,         // RFD 0019
}
```

Every routing target the router will pick from is already
described here, with cost fields populated by the RFD-0009
pricing audit. `ModelRegistry::resolve()` is the lookup
mechanism; the router can call it to validate any candidate
decision. **Prerequisite for this RFD**: a small additive
`tier: u8` field on `ModelInfo` (0 for free/local, 1-3 for
paid tiers) is added in milestone M1 below — used by the
escalation-on-failure step to walk the `fallback_chain`.

#### Provider dispatch (`crates/pi-ai/src/provider.rs:50-125`)

`Provider::stream(req, model)` is the single sink for every LLM
call. The factory (`crates/pi-agent-core/src/runtime.rs:86-100`)
constructs a provider lazily based on `ProviderKind`. The router
slots in **before** the factory: pick `(provider_name, model_id,
thinking)`, then let the existing dispatch run unchanged. No
changes to provider implementations are needed.

#### Thinking knob (`crates/pi-ai/src/message.rs:14-19`)

`ThinkingLevel::{Off, Low, Medium, High}` is already separated
from the model choice — passable per-request, settable per
agent file (`thinking:` frontmatter,
`crates/pi-coding-agent/src/native/task/definition.rs:106`),
overridable per CLI invocation (`--thinking`, `cli.rs:20-22`).
The router emits a `ThinkingLevel` alongside the model and the
runtime already knows how to consume it.

#### Subagent dispatch (`crates/pi-coding-agent/src/native/task/`)

The `task` tool already gives every subagent its own model +
thinking + tool allowlist via YAML frontmatter
(`task/definition.rs:89-116`). Today this is hand-coded in
`.pi/agents/<name>.md`; the router can populate it dynamically
or supplement it with a *fallback chain*. Subagent results are
streamed back as tool results (`task/executor.rs`), so a
cascading router that retries on a low-quality result has a
natural integration point.

#### Trajectory judge (`crates/pi-coding-agent/src/native/trajectory/judge.rs:31-185`)

Inputs: user task, agent reply, `TrajectoryFeatures`
(test_runs, compile_runs, edit_errors, repeated_reads,
last_termination, turn_counts) plus system prompt size.
Output: `JudgeVerdict { success, score 0.0–1.0, reason,
salient_wins, salient_failures }`. Default model:
`anthropic/claude-haiku-4-5-20251001` (lines 82-83 — already
the cheapest model in the fleet). This is **the** deferral
signal for the cascade stage.

#### Cost telemetry + flamegraph (`crates/pi-coding-agent/src/native/trajectory/flamegraph.rs:38-59`)

Per-Usage `cost_usd` is attached to the assistant block that
emitted it (RFD 0012). The whole trajectory serializes to JSON.
That data flow already feeds the evolve daemon.

#### Pi-stats (`crates/pi-stats/src/{ingest,aggregate}.rs`)

Ingests every session's JSONL into SQLite, aggregates into
`OverallStats` / `ModelStats` (per provider×model: requests,
cost, tokens) / `FolderStats` (per cwd) / `TimeSeriesPoint`. The
schema includes model + provider + folder, so a query like
"what's the cheapest model that has historically completed tasks
in `~/src/my-project`?" is a single SQL aggregation away.

#### Evolve daemon (`crates/pi-coding-agent/src/evolve/`)

Section-by-section AGENTS.md mutator that uses trajectory judge
verdicts as the win/loss signal, runs a slow model
(default Sonnet) to rewrite, replays candidates against recent
trajectories, picks a Pareto frontier, applies under a 24h
cooldown + per-day cost cap. **Same shape applies to routing
table mutations**: the *subject* changes from "AGENTS.md
section text" to "routing decision tree", the *metric* from
"task pass rate" to "cost per successful task", but the loop
is identical.

#### Local model entrypoint (`crates/pi-ai/src/registry.rs:484-491`)

```rust
ProviderConfig {
    name: "ollama",
    kind: ProviderKind::OpenAiCompat,
    base_url: "http://localhost:11434/v1",
    auth_format: "Bearer {token}",
    models: vec![],   // user picks
}
```

Ollama is already a peer to OpenAI in the registry; the only
gap is "no models pre-registered" because users vary on which
weights they pull. The router can treat `ollama/*` as
`tier:0 cost=$0` and let users opt in via a config knob (or a
pi-rs auto-discovery step that calls `GET
http://localhost:11434/api/tags`).

### What pi-rs does NOT have

* **No pre-flight classifier.** Every request goes through the
  user-specified model. `crates/pi-coding-agent/src/cli.rs`
  mentions `roles.smol` as a "cheap model for X" knob and the
  auto-judge uses Haiku, but there is no general dispatcher that
  picks per-request.
* **No cascade.** A subagent that fails today produces a tool
  error and the parent retries on the same model. There is no
  escalation tier.
* **No deferral signal exposed to the dispatch layer.** The
  trajectory judge runs *post-hoc*, not as a gate.
* **No budget controller.** `Thinking::*` is a coarse 4-level
  enum; there's no per-request token budget the model is asked
  to honour.
* **No empirical-routing learner.** Pi-stats has the data; the
  evolve loop has the optimization shape; nobody connects the
  two for routing decisions.

The smallest design that fills all five gaps is the three-stage
pipeline below.

## Research landscape (2023-2026)

A condensed, citable map of techniques the proposal draws from.
Full URLs at the bottom of this RFD.

### Routing (one-shot model picker)

* **RouteLLM** (Ong et al., LMSYS, Jun 2024). Trains four router
  classes — similarity-weighted ranking, matrix factorization,
  BERT classifier, causal LLM classifier — on Chatbot Arena
  preferences augmented with MMLU/MT-Bench gold labels. Emits
  `P(strong beats weak | prompt)`; threshold τ chosen offline
  for a quality target. **≥85% cost cut on MT-Bench, 45% on
  MMLU, 35% on GSM8K, 95% of GPT-4 quality preserved.** Routers
  generalize to model pairs not seen at training. Matrix-
  factorization variant ~20MB, Rust-deployable via `candle` or
  `tract`.
* **xRouter** (Salesforce AI Research, Qian et al., Oct 2025).
  7B Qwen2.5 backbone, RL-trained with DAPO over 20+ heteroge-
  neous downstream models as "tools". Reward = success-gated ×
  cost-shaped on Reasoning360. **xRouter-7B-2 ≈ GPT-5 accuracy
  on Olympiad Bench at ~⅛ the cost; 60–80% reduction at near-
  frontier accuracy.** The "router-is-an-LLM-with-tools" shape
  maps onto pi-rs's existing `task` subagent dispatcher; the
  cost is high enough that a small classifier wins on local
  workloads.
* **Router-R1** (UIUC ULab, Jun 2025). **Multi-round** routing:
  the router LLM interleaves `<think>` and `<route>` actions,
  aggregates responses across multiple downstream LLMs into one
  answer. Conditions on JSON model descriptors (price, latency,
  exemplar perf), so adding a provider doesn't need retraining.
* **LLMRouter library** (UIUC ULab, Dec 2025). 16+ routers
  across single-round / graph / multi-round / personalized
  families, unified CLI, data-gen pipelines for 11 benchmarks.
  We use it as the *training-data + offline-evaluation harness*
  for whatever pi-rs ultimately ships, not as a runtime dep.
* **vLLM Semantic Router v0.1 "Iris"** (Jan 5, 2026). ModernBERT
  + LoRA classifiers across 14 MMLU domains plus jailbreak/PII/
  fact-check heads. Flash-Attention + prompt compression
  cut routing latency from seconds to **tens of ms (98×
  reduction vs LLM-as-router)**. On MMLU-Pro: **+10.2 pp
  accuracy with -47.1% latency and -48.5% tokens.** The exact
  shape of pi-rs's tier-0 classifier — sub-50 ms on CPU via
  `ort`/`tract`, no GPU needed.
* **aurelio-labs/semantic-router**. Embedding-cosine routing:
  each route is a list of utterances, query embedded once,
  compared to centroids, threshold-decided. Low-latency once
  embeddings/centroids are cached (exact figure depends on
  embedding backend + hardware — the project's own demo cites
  single-digit ms; we'll measure on our own deployment).
  Zero training, ships as the v0 baseline.
* **RouterBench** (Hu et al., Mar 2024). 405k inference outcomes
  × 64 tasks across an LLM fleet, with a formal cost-quality
  framework: each router is a 2D curve, compared by
  non-decreasing convex hull and AIQ. **This is the evaluation
  shape pi-rs adopts for its own router** — emit cost/quality
  pairs from `pi-stats` and plot the Pareto frontier.

### Cascading (cheap-first, escalate on low confidence)

* **FrugalGPT** (Chen, Zaharia, Zou, Stanford, May 2023). The
  canonical work. Three orthogonal levers: prompt adaptation,
  approximation (caching + finetune), and the LLM cascade. The
  cascade trains a **DistilBERT regressor** s(prompt, response)
  on (prompt, output, label) triples; cheapest model fires
  first, escalation continues if `s < τᵢ`. Threshold vector τ
  solved as constrained LP minimizing cost s.t. accuracy ≥ K.
  **Up to 98% cost reduction at GPT-4 parity, or +4% accuracy
  iso-cost.**
* **C3PO** (Valk et al., NeurIPS 2025). Self-supervised cascade
  construction with **conformal prediction** for probabilistic
  budget guarantees `P(cost > B) ≤ α`. No labels — regret is
  measured against most-powerful-model outputs. SOTA on GSM8K /
  MATH-500 / BBH / AIME; matches MPM accuracy within 2/5/10%
  margin at <20% MPM cost. ~200 lines of Rust on top of pi-stats.
* **A Unified Approach to Routing and Cascading for LLMs**
  (Dekoninck/Baader/Vechev, ETH SRI, Oct 2024 / ICML 2025).
  Proves optimality of routing under perfect quality estimators
  and of cascading under perfect deferral signals; introduces
  **cascade routing** which at each step picks "the model most
  likely to be optimal *and* most likely to be sufficient",
  allowing skips and reorderings. **Beats RouterBench convex
  hulls by 1–4 pp absolute (13–80% relative).** This is the
  decision rule we propose to ship — not "router XOR cascade"
  but the unified DP.
* **IBM "frugal" routing** (2024). Routing-cascade hybrid over a
  fleet of *specialists*. Highlights that domain specialists
  (DeepSeek-R1 for math, Cohere for retrieval/rerank, Groq for
  low-latency Haiku-class) can beat a single generalist if the
  router is accurate. Pi-rs already has all of these via the
  OpenAi-compat adapter — the routing layer activates them.

### Multi-agent decomposition

* **VeriMAP** (Megagon, Oct 2025). Verification-Aware Planner
  emits a **DAG of subtasks with structured I/O** plus per-node
  **Verification Functions** (Python *and* natural-language).
  Executors run on cheap models; VFs gate progress; only on VF
  failure does the planner replan or escalate. Maps directly
  onto pi-rs's `task` dispatcher — planner emits `(model_tier,
  vf)` per subtask; the trajectory judge is one VF flavour.
* **MACE** (Multi-Agent Claim Verification, Apr 2026). Pure
  zero-shot prompting Planner / Executor / Verifier triad
  (no finetuning). 27–92B models hit 80–100% of frontier
  accuracy versus 235B SOTA. Demonstrates the prompting-only
  baseline buys most of the benefit; we don't need RL training
  to start.
* **Agent-Oriented Planning** (OpenReview EqcLAU6gyU). Three
  formal design principles for meta-agent planners:
  **solvability, completeness, non-redundancy**. Useful as
  acceptance criteria for pi-rs's planner.

### Token-budget self-prediction

* **SelfBudgeter** (May 2025). Two-phase training (cold-start
  SFT + RL) teaching the model to emit `<budget>N</budget>`
  before answering and self-stop. **-61% average response
  length on math reasoning, accuracy maintained.** The natural
  successor to `Thinking::Adaptive`.
* **Reasoning in Token Economies** (EMNLP 2024). Headline
  finding: **CoT + self-consistency, given equal compute
  budget, beats Reflexion / multi-agent debate / ToT on
  every dataset except HotpotQA**, and the complex strategies
  often *get worse* with more budget. **Hard implication: don't
  ship Reflexion-style loops by default.**
* **TALE** (Token-Budget-Aware LLM Reasoning, Han et al., ACL
  2025 Findings). Two variants: TALE-EP (prompt-only — ask the
  LLM to estimate its own budget then reason within it) and
  TALE-PT (finetuned). **Average 45.3% token reduction with
  negligible accuracy loss.** TALE-EP has zero training cost
  and drops in as a system-prompt change — this is what we
  ship for the budget controller.

### Local models + speculative decoding

* **Speculative decoding** (llama.cpp). Small draft model
  proposes k tokens; large validator scores in one batched
  forward; rejected tokens resampled. **1.83-2.5× on dense
  models** (Llama-3.1-8B + 1B/0.5B drafts). **April 2026
  benchmark on Qwen3.6-35B-A3B (MoE): no config achieves net
  speedup** — expert-loading dominates. So spec-decode is a
  dense-model optimization; not free for current MoEs.
* **GPT-OSS-20B** (OpenAI, Apache-2.0; 21B total / 3.6B active).
  Runs in 16 GB VRAM, ~o3-mini quality, native function-calling.
  Caveat: community reports of inconsistent tool-call format on
  some local serving stacks → pi-rs needs a strict tool-call
  validator with fall-through to a paid API on parse failure.
* **Ollama in production** (52M monthly downloads Q1 2026).
  Stable OpenAI-compatible HTTP API, concurrent requests,
  hot-swap, GPU memory management, MCP for tools. Pi-rs already
  has the OpenAI-compat adapter — Ollama is a **zero-code**
  additional provider; flag it as `tier:0` (free) target.

### Production routing systems (interface references)

* **Portkey AI Gateway** — config-by-ID routing, fallbacks,
  retries, conditional metadata routing, weighted load
  balancing, circuit breakers, canary, caching. Strong
  observability surface. Good *interface* model: routing
  changes via JSON config without redeploys.
* **OpenRouter** — pass-through pricing + 5.5% credit fee,
  dynamic suffix routing (`:nitro` throughput-sorted, `:floor`
  price-sorted, `:exacto` quality-tuned), auto-fallback on 5xx,
  billed only on success.
* **Anthropic's tiering guidance** — "Sonnet default; route
  easy down to Haiku; escalate hard to Opus" is formalized in
  the platform docs as a 3-tier stack and is widely cited
  (community-reported "70-90% cost reduction" is a paraphrase,
  not a documented Anthropic figure — pi-rs measures its own
  number). This is the empirical floor v1 must beat. Cleanest
  v1 ship: codify this rule, then learn the deviations from
  pi-stats logs.

## Proposal

A single-stage classifier in v1, with telemetry plumbing to set
up v2's escalation design.

```
┌──────────────────────────────────────────────────────────────────┐
│                       request comes in                           │
│  (user prompt, conversation history, tools, attachments)         │
└────────────────┬─────────────────────────────────────────────────┘
                 │
                 ▼
   ┌─────────────────────────────────────┐
   │  Stage 1: Tier-0 classifier         │
   │  ───────────────────────────────    │
   │  Embedding-cosine over named routes │
   │    { fast, default, hard }          │
   │  → RoutingDecision {                │
   │      provider, model,               │
   │      thinking, max_tokens,          │
   │      route_id, similarity,          │
   │      fallback_chain (user-driven),  │
   │      use_tale (per-route opt-in)    │
   │    }                                │
   │  Sub-50 ms on CPU, no GPU dep.      │
   └────────────┬────────────────────────┘
                │
                ▼
   ┌─────────────────────────────────────┐
   │  Telemetry                           │
   │  ───────────────────────────────     │
   │  Emit `routing_decision` session     │
   │  entry. pi-stats aggregates per      │
   │  route. Trajectory judge runs        │
   │  post-hoc as today.                  │
   │  → input data for v2's escalation    │
   │    design.                           │
   └────────────┬────────────────────────┘
                │
                ▼
   stream events back to caller as today
```

The pipeline is **opt-in** for v1 (`pi --route auto`), default
for v2. v2 is gated on:
* enough pi-stats data to characterize failure modes (≥1500
  classified sessions across all three routes), AND
* an AB-test showing a measurable win on the same workload.

**Stage 2 (escalate-on-failure) is in v2.** The v1.0 design was
moved to "Future work" because (a) the failure detector misses
semantic-quality failures with `FinishReason::Stop`,
(b) `runtime.rs`'s `StreamInterceptor` only hooks `TextDelta`
not tool-call events, so mid-stream escalation needs the
runtime's loop redesigned, not a 300-LOC patch, and (c) we
don't yet know what failure modes actually matter on
pi-rs traffic — better to measure first.

User can always force a tier with `pi --model X --thinking Y`,
which bypasses the router entirely. That's the v1 escape hatch.

### Stage 1: Tier-0 classifier

#### Routes

Three routes — `fast`, `default`, `hard`. The embedding-cosine
classifier's discrimination ceiling on short coding prompts is
~3 buckets; six was vanity. More routes can be added by users
in their config without code changes (each route is a centroid
over example prompts), but v1 ships with three.

| Route id   | Example                                       | Default tier                |
| ---------- | --------------------------------------------- | --------------------------- |
| `fast`     | "rename `foo` to `bar` in this file"          | Haiku 4.5 + Off             |
| `default`  | "extract this trait into its own crate"       | Sonnet 4.6 + Medium         |
| `hard`     | "prove this loop terminates"                  | gpt-5.4 Responses + High    |

Defaults are seedable from Anthropic's 3-tier guidance + the
RFD-0009 pricing audit. The routing table lives in
`~/.pi/agent/router.toml` (per user) and
`<repo>/.pi/router.toml` (per repo, takes precedence). Both
files are observable, hand-editable, and version-controllable.

**Migration invariant**: when no `router.toml` exists, `--route
static` is functionally identical to today's `--model` /
`--thinking` flag dispatch. A user upgrading the binary across
this RFD sees zero behavior change unless they opt in.

#### Implementation choices (ranked)

1. **Embedding-cosine (ship first)**. Each route lists 5-15
   example prompts; pi-rs computes embeddings (via the existing
   tokenizer + a small embedding model — `gte-small` or
   `all-MiniLM-L6-v2` via `ort`/`tract`); at request time embed
   the prompt, cosine vs centroids, route to the highest above
   threshold. **Zero training, single-digit ms with cached
   centroids, Rust-only deployable.** Inspired by
   aurelio-labs/semantic-router.
2. **ModernBERT classifier (ship second)**. Train a head on
   pi-stats trajectories labelled by the trajectory judge.
   ~50 ms on CPU, generalizes better. Use the LLMRouter library
   as the offline harness; deploy via `ort`. Inspired by vLLM
   Iris.
3. **xRouter-style LLM router (do not ship)**. Too expensive at
   pi-rs's per-request scale (would dominate the cost the
   router is trying to save). Listed only to mark it as out
   of scope.

#### Output

```rust
pub struct RoutingDecision {
    pub provider:        String,         // "anthropic"
    pub model:           String,         // "claude-haiku-4-5-..."
    pub thinking:        ThinkingLevel,
    pub max_tokens:      Option<u32>,    // per-route cap, advisory budget
    pub route_id:        String,         // "fast" — for telemetry
    pub similarity:      f32,            // raw cosine, NOT a probability
    pub fallback_chain:  Vec<(String, String, ThinkingLevel)>,
    pub use_tale:        bool,           // opt-in TALE-EP per route
}

pub struct Outcome {
    pub cost_usd:           f64,
    pub latency_ms:         u32,
    pub stop_reason:        StopReason,
    pub tool_call_parse_ok: bool,
    pub max_tokens_overrun: bool,         // true if output > 2× cap
    pub judge_verdict:      Option<JudgeVerdict>,  // post-hoc only
}
```

The `fallback_chain` is the cascade hierarchy stage 2 climbs
when escalation fires. The classifier seeds a sensible default;
user can override per route. `similarity` is **raw cosine
distance**, not a probability — calibration via Platt scaling
against pi-stats labels is a v2 concern.

### Stage 2: deferred to v2 (what we'd build, and why we're not building it now)

The v1.0 draft proposed an "escalate on concretely-broken
response" stage. Second critique pass surfaced four unresolved
issues that together make this premature:

1. **Detection completeness.** The failure list (parse error /
   non-stop / overrun / 5xx) is a good *cost* check but a poor
   *quality* check. The most common silent failure on a
   coding-agent workload is "model returned a clean
   `FinishReason::Stop` with a confident-but-wrong answer";
   that response slips through. Real escalation needs either
   (a) a calibrated quality scorer (FrugalGPT DistilBERT or the
   trajectory judge per-request, both have cost/latency we
   don't yet know is justified) or (b) a multi-turn user-
   correction loop, which is its own design.

2. **Vocabulary mismatch.** The v1.0 prose used wire strings
   (`stop_reason != "end_turn"`) but pi-rs normalizes to
   `pi_ai::FinishReason::{Stop, ToolUse, Length, Refusal,
   Aborted, Other}`. The detection logic needs to be re-
   expressed against the normalized API, which surfaces edge
   cases (e.g. `FinishReason::ToolUse` is a normal mid-turn
   state, not a failure).

3. **Stream-level abort isn't where the v1.0 RFD said it was.**
   The runtime's existing `StreamInterceptor::on_text_delta`
   is the only mid-stream hook; tool-call events are
   end-of-stream-only. So aborting on a malformed tool call
   means letting the stream complete, synthesizing an error
   tool result, and *then* re-prompting on the next tier — a
   redesign of the runtime loop, not a small patch.

4. **Context-window and history semantics.** A failed first-
   tier attempt may have emitted partial assistant text + a
   broken tool call. Escalating to a second tier needs a
   policy: include the failed attempt in history (consumes
   window, biases the next model) or strip it (re-cost the
   prefix). v1.0 didn't choose.

**v2 of this RFD owns Stage 2** with a concrete design against
real pi-stats failure-distribution data collected by v1's
telemetry. Until then, **the user remains the escalator** via
`pi --model X` overrides. The classified `RoutingDecision`
still carries a `fallback_chain: Vec&lt;(provider, model,
thinking)&gt;` — but in v1 the chain is documentation, not a
live mechanism.

**Local models / Ollama tier-0** is similarly deferred. The
discovery story is racy (mid-session model adds), tool-call
schema validation needs to be redesigned (the same gap that
broke our first integration test), and the cost win is small
on agentic-coding traffic. Tracked as its own follow-up RFD.

### TALE-EP (per-route opt-in, not a stage)

Demoted from a core stage to a per-route flag because the TALE
paper's 45-70% token reductions are reported exclusively on
math-reasoning benchmarks (GSM8K, GSM8K-Zero, MathBench).
Coding-agent traffic is dominated by tool calls where the
budget tag is at best uninformative and at worst causes the
runtime to truncate a partial diff. v1 therefore:

* Ships `use_tale = true` on the `hard` route only.
* Adds the system-prompt addendum:
  ```
  Before answering, on a single line, emit:
    <budget>N</budget>
  where N is your best estimate of the token count needed for
  a high-quality answer. Then answer in at most N tokens.
  ```
* Parses the tag for **telemetry only** — emits
  `(predicted_budget, actual_tokens)` into pi-stats. The
  runtime does **not** cap on the parsed budget in v1.
* After 90 days of pi-stats data, the v2 RFD decides whether
  to enforce the cap, broaden TALE-EP to the `default` route,
  or remove it.

`Thinking::Adaptive` (RFD 0003) is unaffected — Adaptive
controls *reasoning compute*; TALE-EP (when enforced) caps the
*output*. The two are orthogonal.

## Pi-rs concrete design

### Module: `pi_agent_core::router`

**No new crate.** A new `Router` trait + `StaticRouter` +
`EmbeddingRouter` live in `crates/pi-agent-core/src/router.rs`,
alongside the existing `RuntimeConfig`. The trajectory-judge-
based learning piece (deferred to v2) lives in
`crates/pi-coding-agent/src/evolve/` where the judge already
sits, avoiding a circular dep.

Public API:

```rust
pub trait Router: Send + Sync {
    /// Pick a model + thinking for this request.
    fn route(
        &self,
        prompt: &str,
        history: &[Message],
        tools: &[ToolSpec],
        ctx: &RoutingContext,
    ) -> Result<RoutingDecision>;

    /// Post-hoc: record the trajectory's final cost / outcome.
    /// Drives the v2 LearnedRouter.
    fn observe(&self, decision: &RoutingDecision, outcome: &Outcome);
}

/// Read-only context the router sees per request. Carries
/// **primitives only** — no `pi-stats` or `pi-coding-agent`
/// types, to keep `pi-agent-core` dep-clean. Adapters live
/// upstream.
pub struct RoutingContext<'a> {
    pub registry:           &'a ModelRegistry,
    pub user_lambda:        f64,         // cost↔quality knob (v2)
    pub force:              Option<ForceOverride>,
    pub session_id:         &'a str,
    /// Recent-history cache state from the previous turn's
    /// `Usage`. Replaces the v1.0 `cache_hit: bool`.
    pub cache_read_tokens:  u64,
    pub cache_write_tokens: u64,
}

/// A normalized post-turn outcome. Carries primitives only;
/// `pi-coding-agent` adapts judge verdicts into
/// `quality_score` before calling `Router::observe`.
pub struct Outcome {
    pub cost_usd:            f64,
    pub latency_ms:          u32,
    pub ttft_ms:             Option<u32>,
    pub stop_reason:         pi_ai::FinishReason,
    pub tool_call_parse_ok:  bool,
    pub max_tokens_overrun:  bool,
    pub retry_count:         u8,
    pub reasoning_tokens:    u64,
    pub cache_read_tokens:   u64,
    pub cache_write_tokens:  u64,
    pub quality_score:       Option<f32>,   // 0..1, judge-fed
    pub final_provider_error:Option<String>,
}
```

Two concrete `Router` implementations in v1, ordered by ship
date:

1. **`StaticRouter`** — reads the routing table directly.
   Equivalent to today's manual model picking but expressed in
   the router shape. Ships in M1; lets the rest of the runtime
   integrate without depending on the classifier. Migration-
   safe: when no `router.toml` exists, behaves identically to
   today's CLI dispatch.
2. **`EmbeddingRouter`** — Stage 1 (3 routes). Ships in M2.

Concurrency: `EmbeddingRouter` holds centroids as
`Arc&lt;Vec&lt;f32&gt;&gt;` and the embedding-model session as `Arc&lt;_&gt;`,
recomputed only on explicit `Router::reload()` (called when
`router.toml` mtime changes). No locks on the hot path.

The v0.5 draft's `LearnedRouter` (ModernBERT trained on pi-
stats) is **deferred to v2**. The bandit problem of learning
routing decisions from sparse, slow feedback is genuinely
different from the prose-rewrite problem the evolve daemon was
designed for; we don't have the labelled trajectories yet, and
shipping v1 is what creates the data we'd train on.

### Configuration: `~/.pi/agent/router.toml`

```toml
[router]
mode             = "auto"           # "off" | "static" | "auto" | "learned"
default_lambda   = 1.0              # cost-quality tradeoff
enable_local     = false
local_endpoint   = "http://localhost:11434"

[router.budget]
strategy         = "tale-ep"        # "off" | "tale-ep" | "selfbudgeter"
safety_margin    = 50

[[route]]
id               = "fast"
examples         = [
  "rename foo to bar in this file",
  "add a doc comment to this function",
  "remove the println at line 42",
]
threshold        = 0.55              # raw cosine, similarity-space
provider         = "anthropic"
model            = "claude-haiku-4-5-20251001"
thinking         = "off"
max_tokens       = 800
fallback_chain   = ["sonnet:low", "sonnet:medium"]

[[route]]
id               = "default"
examples         = [
  "extract this trait into its own crate",
  "audit OpenAI's Responses API and write an RFD",
  "run the test suite and fix what fails",
]
threshold        = 0.50
provider         = "anthropic"
model            = "claude-sonnet-4-6"
thinking         = "medium"
max_tokens       = 4000
fallback_chain   = ["opus:medium", "gpt-5.4:high"]

[[route]]
id               = "hard"
examples         = [
  "prove that this loop terminates",
  "find a counterexample to this invariant",
  "is the borrow checker sound for this pattern?",
]
threshold        = 0.50
provider         = "openai"
model            = "gpt-5.4"
thinking         = "high"
max_tokens       = 8000
use_tale         = true              # opt-in TALE-EP, telemetry-only
fallback_chain   = ["opus:high"]
```

The v0.5 draft's `[router.learn]` block (flamegraph_path,
update_cooldown, cost_cap_per_day) is deferred to v2 with the
LearnedRouter. The TOML schema is intentionally minimal in v1
to avoid baking in v2 decisions.

### Integration points

| Where                                                        | Change                                                                                                                              |
| ------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------- |
| `crates/pi-coding-agent/src/cli.rs:20-22`                    | Add `--route {off,static,auto,learned}`, default `static`. Existing `--model` / `--thinking` become **route overrides**.            |
| `crates/pi-agent-core/src/runtime.rs:175-192`                | Before constructing `AgentSessionInner`, call `router.route(...)` and use its decision. Existing CLI flags suppress the router.     |
| `crates/pi-coding-agent/src/native/task/executor.rs`         | Per-subagent: if the agent's frontmatter doesn't pin a model, route. Existing `model:` field acts as a force override.              |
| `crates/pi-coding-agent/src/native/trajectory/judge.rs:31`   | Optional second-use as a stage-2 deferral signal. Same struct, exposed as a `Router::probe()` helper.                               |
| `crates/pi-coding-agent/src/evolve/`                         | New `RoutingMutator` mirrors `SectionMutator`. Same loop, different subject.                                                        |
| `crates/pi-stats/src/aggregate.rs`                           | New view: `by_route_id` (per route: requests, mean cost, judge-pass rate). Drives the empirical Pareto curve.                       |
| `crates/pi-ai/src/registry.rs`                               | Add `tier: u8` to `ModelInfo` (0 for local/free, 1-3 for paid tiers). Used by the cascade fallback chain.                           |

### Telemetry additions

* Every `RoutingDecision` is recorded in the JSONL session log
  (new entry kind `routing_decision`).
* `pi-stats` ingests it; `dashboard` gains a "router efficiency"
  panel (router decisions vs. cost-optimal in hindsight).
* `flamegraph.json` annotates each turn with the route id used,
  so the evolve daemon can correlate "this route → these
  outcomes".

### Local-model story (Ollama tier-0)

The flag `enable_local = true` activates a discovery step at
session start: pi-rs calls `GET /api/tags`, registers each
returned model in the runtime registry as
`ollama/<id>` with `cost = 0.0`, `tier = 0`. The cascade then
naturally tries them first (subject to the embedding-classifier
saying "trivial-edit"). Tool-call output validation is
mandatory for local models — on parse failure, the cascade
treats the call as a deferral and escalates.

## Test plan

All paths refreshed (no more `crates/pi-router/...`). Tests
that referenced the deleted Stage 2 (ETH cascade rule, TALE-EP
hard-cap enforcement) are removed; new tests cover the failure
modes the critique surfaced.

1. **`crates/pi-agent-core/tests/embedding_router_routes.rs`** —
   given the bundled 3-route default config, hit the router
   with 30 hand-labelled prompts; assert correct route on the
   unambiguous prompts and that the ambiguous prompts (debug-
   ging, semantic review — see Open question 4) deterministi-
   cally fall to one specific route. No network.
2. **`crates/pi-agent-core/tests/static_router_compat.rs`** —
   with `mode = "static"`, the router emits the same decision
   as today's CLI flag dispatch for 20 sample requests. Pins
   migration safety.
3. **`crates/pi-agent-core/tests/router_no_config_falls_back.rs`** —
   with no `router.toml` present, `Router::route` returns the
   `RuntimeConfig`'s default model + thinking unchanged.
4. **`crates/pi-agent-core/tests/router_resolve_failure.rs`** —
   if a configured route's model isn't in `ModelRegistry`
   (e.g. a stale `ollama/...` entry), `Router::route` returns
   `RouterError::UnknownModel(name)` rather than panicking.
5. **`crates/pi-agent-core/tests/router_force_override.rs`** —
   `RoutingContext::force = Some(ForceOverride::CliFlag {
   model, thinking })` short-circuits the classifier and
   returns the override unchanged.
6. **`crates/pi-stats/tests/by_route_id_aggregation.rs`** —
   ingest a synthetic JSONL containing
   `routing_decision` + `usage` pairs across three routes;
   assert `dashboard().by_route_id` returns expected counts and
   per-route mean cost.
7. **`crates/pi-stats/tests/session_entry_unknown_kind.rs`** —
   feed a session JSONL containing a `kind: "future_kind"`
   line; ingestion must **skip the line and continue**, not
   fail-fast. Pins forward-compatibility for new entry kinds.
8. **`crates/pi-coding-agent/tests/router_observe_writes_stats.rs`** —
   `Router::observe(decision, outcome)` writes a SessionEntry,
   `pi_stats::ingest::sync_all` picks it up, `dashboard()`
   surfaces it. End-to-end smoke.
9. **`crates/pi-coding-agent/tests/tale_ep_budget_telemetry.rs`** —
   for a route with `use_tale = true`, the parsed `&lt;budget&gt;`
   is written to telemetry alongside actual `output_tokens`.
   Runtime does **not** cap; assert `max_tokens` honored
   verbatim from the route config.
10. **End-to-end manual** — `pi --route auto -p "rename foo to
    bar in src/main.rs"` routes to Haiku. `pi --route auto -p
    "prove the borrow checker is sound for this pattern"`
    routes to gpt-5.4 with thinking=high. Both visible in
    `pi /cost`.

### Empirical validation (post-ship)

* Run a RouterBench-shaped evaluation on pi-stats trajec-
  tories: re-emit each historical request through the router
  with every (route, model) combo, plot the cost-quality
  Pareto frontier, assert the router's chosen path lies on or
  near the frontier.
* AB-test per the **revised** acceptance criteria above
  (≥1500 sessions, ≥30% cost reduction, ≤5 pp pass-rate
  drop, ≤+50 ms TTFT delta).

## Out of scope (v1)

Split into two groups: **deferred** items get their own
follow-up RFD or a v2 of this one when the prerequisite data
exists; **architecturally rejected** items don't fit pi-rs's
shape and won't ship at all.

### Deferred to v2 / follow-up RFDs

* **Stage 2: escalate-on-failure**. v2 of *this* RFD; designed
  against pi-stats failure-distribution data captured by v1's
  telemetry milestone (M3).
* **ETH cascade-routing decision rule** (calibrated
  `p_optimal`, `p_sufficient`). v2 of this RFD; needs the
  labelled-outcome data v1 collects.
* **`LearnedRouter`** (ModernBERT trained on pi-stats). v2 of
  this RFD; needs ≥1500 labelled trajectories.
* **`RoutingMutator` + evolve daemon plumbing**. v2 of this
  RFD; same data dependency.
* **TALE-EP hard-cap enforcement**. v2 of this RFD; v1 captures
  `(predicted_budget, actual_tokens)` pairs to inform whether
  enforcement helps or hurts on real coding traffic.
* **Ollama / local tier-0**. Its own follow-up RFD. The
  discovery race conditions and tool-call schema validation
  warrant a focused design.
* **VeriMAP-style per-subtask verification functions**. Its
  own follow-up RFD; the trajectory judge is one VF over the
  whole turn — per-subtask VFs need a planner redesign.
* **PII detection head**. Its own follow-up RFD if needed —
  taxonomy choice, false-positive economics, and a
  classifier-bake-off all warrant a focused design.
* **Cross-provider Pareto evaluation harness** (RouterBench
  in CI). Its own follow-up RFD; v1 emits the cost/quality
  JSON, plotting stays external.

### Architecturally rejected

* **xRouter-style LLM-as-router**. Too expensive at pi-rs's
  per-request scale — the router itself would dominate the
  cost it's trying to save. (RL-trained 7B routers make sense
  at SaaS scale, not on a developer laptop.)
* **Speculative decoding**. Pi-rs doesn't host its own
  weights; it dispatches over provider HTTP APIs. The
  technique requires control of the inference loop we don't
  own.
* **Multi-round routing (Router-R1)**. Conflicts with pi-rs's
  single-stream output contract — pi-rs streams one assistant
  turn at a time, not aggregated multi-LLM responses.
* **Per-token streaming budget enforcement**. Would require
  provider cooperation (mid-stream cancellation tied to a
  semantic budget) that no major provider exposes.

## Open questions (v1)

Critique pass dropped OQ4 (deferral signal cost — answered by
the Stage-2 redesign) and OQ6 (PII — moved to out-of-scope).
Remaining:

1. **Where do the routing centroid examples live?** Per-user
   (`~/.pi/agent/router.toml`), per-repo
   (`<repo>/.pi/router.toml`), or bundled defaults
   (`crates/pi-agent-core/data/default_routes.toml`)? **Lean:
   bundled defaults override-able by per-repo, override-able
   by per-user.** Pi-rs precedent matches this (AGENTS.md,
   agent files).
2. **Do we expose λ (cost-quality tradeoff) to the user?**
   `pi --route auto --lambda 2.0`. **Lean: yes; trivial,
   useful, but only meaningful once Stage 2 has a calibrated
   confidence signal in v2 — for v1 it's an unused field on
   `RoutingContext` reserved for the v2 surface.**
3. **Compatibility with subagents that pin a model.** Today
   `.pi/agents/code-reviewer.md` says `model: gpt-5.4`. The
   precedence ladder when both an agent file and a route apply:
   * `--model` CLI flag → wins everything (force override).
   * Agent frontmatter `model:` → wins over the router.
   * `--route` decision → applies if no force override.
   * Settings.json `default_model` → fallback if no router.
   **Lean: spell this out in the docstring on `Router::route`.**
4. **Embedding-model distribution.** `gte-small` ONNX is ~140
   MB. Bundle in the binary (size jump), download on first run
   (offline-mode regression), or require user provisioning?
   **Lean: download on first run with a clean error if offline,
   plus a `pi router fetch-embeddings` command.**
5. **`pi /route` UI affordance.** v0.5 critique flagged the
   asymmetric failure cost: routing a hard prompt to Haiku
   produces a fast wrong answer the user trusts because the UI
   is silent. **Lean: ship a `pi /route` slash command that
   shows the last decision + a one-key escalation
   (`pi /route up`).**
6. **What about the `off` mode?** v0.5 listed `mode = "off"`
   in the TOML enum but didn't wire it. **Lean: `off` skips
   the trait entirely — the runtime checks the mode flag and,
   if `off`, dispatches as today without ever calling
   `Router::route`. Test 4 in §Test plan pins this.**
7. **Provider rate-limit awareness in `cascade_step`.** Should
   429s short-circuit the chain to the next provider rather
   than the next tier? **Lean: yes. `Outcome.stop_reason` gains
   a `RateLimited(provider)` variant; `cascade_step` skips any
   chain entry on the rate-limited provider for the rest of
   the session.**

## Implementation plan

v1 ships in **three milestones**. Total: ~1300 LOC, expected
$2 in dogfood spend.

| Milestone | Worktree                       | Scope                                                                                                                                                                                       | Est. LOC |
| --------- | ------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------- |
| **M1**    | `claude/router-static`         | `Router` trait + `Outcome` + `RoutingContext` + `StaticRouter` in `pi_agent_core::router`. Adds `tier: u8` to `ModelInfo`. Reads `router.toml`. `--route static` flag. Migration-safe.        | ~600 |
| **M2**    | `claude/router-classifier`     | `EmbeddingRouter`. `gte-small` ONNX downloaded on first run (`pi router fetch-embeddings`), not bundled in binary. 3-route default bundle. `--route auto` flag.                              | ~700 |
| **M3**    | `claude/router-stats`          | `pi-stats` extensions: `by_route_id` aggregation, `routing_decision` SessionEntry kind (with `unknown ⇒ skip` forward-compatibility), dashboard panel. TALE-EP telemetry-only on `hard` route. | ~400 |

**v1.0 draft's M3 (`router-escalate`)** is removed. Stage 2 is
in v2 of this RFD, designed against real failure-distribution
data from the M3 telemetry above.

**v0.5 draft's M5/M6/M7 (RoutingMutator, LearnedRouter, Ollama
tier-0)** stay deferred. They need data v1 hasn't generated yet.

Order: M1 unlocks M2 and M3. M2 and M3 can run in parallel.

### Acceptance criteria for v1 → v2 flip

The v1.0 draft proposed "100 sessions/arm with judge pass-rate
within ±2 pp." The second critique noted this is statistically
indefensible: 95% CI on a 100/arm pass-rate difference is
~±13.9 pp, power to detect a true 2 pp difference is ~6%. The
revised criteria:

* **Volume gate**: ≥1500 classified sessions across all three
  routes recorded in pi-stats.
* **Cost win**: `--route auto` ≤ 0.7× `--route static` 30-day
  rolling cost on the same workload (Pareto-frontier
  evaluation per RouterBench shape).
* **Quality guardrail**: judge pass-rate ≥ static baseline
  − 5 pp, sustained over the 30-day window (a wider, honest
  guardrail given the realistic sample size).
* **Latency guardrail**: median TTFT no worse than +50 ms
  (the classifier's budget).

When all four hold, flip the default to `auto` and start
designing v2 escalation against the captured pi-stats failure
distribution.

## References

* [RouteLLM (arXiv 2406.18665)](https://arxiv.org/abs/2406.18665) —
  preference-data routing, ≥85% cost cut, 95% quality.
* [LMSYS RouteLLM blog](https://www.lmsys.org/blog/2024-07-01-routellm/)
* [xRouter (arXiv 2510.08439)](https://arxiv.org/abs/2510.08439) —
  RL-trained 7B router agent.
* [Router-R1 (arXiv 2506.09033)](https://arxiv.org/abs/2506.09033) —
  multi-round routing.
* [LLMRouter library (UIUC ULab)](https://github.com/ulab-uiuc/LLMRouter)
* [vLLM Semantic Router v0.1 Iris (Jan 2026)](https://blog.vllm.ai/2026/01/05/vllm-sr-iris.html)
* [vLLM Semantic Router GitHub](https://github.com/vllm-project/semantic-router)
* [aurelio-labs/semantic-router](https://github.com/aurelio-labs/semantic-router)
* [RouterBench (arXiv 2403.12031)](https://arxiv.org/abs/2403.12031)
* [FrugalGPT (arXiv 2305.05176)](https://arxiv.org/abs/2305.05176) —
  the canonical cascade paper, 98% cost reduction.
* [C3PO (arXiv 2511.07396)](https://arxiv.org/abs/2511.07396) —
  conformal-prediction budget guarantees.
* [Cascade Routing (ETH SRI, arXiv 2410.10347)](https://arxiv.org/abs/2410.10347) —
  the unified decision rule we adopt.
* [VeriMAP (arXiv 2510.17109)](https://arxiv.org/abs/2510.17109) —
  per-subtask verification functions.
* [SelfBudgeter (arXiv 2505.11274)](https://arxiv.org/abs/2505.11274)
* [Reasoning in Token Economies (arXiv 2406.06461)](https://arxiv.org/abs/2406.06461)
* [TALE (arXiv 2412.18547)](https://arxiv.org/abs/2412.18547) —
  prompt-only budget self-prediction, 45% reduction.
* [llama.cpp speculative decoding](https://github.com/ggml-org/llama.cpp/blob/master/docs/speculative.md)
* [GPT-OSS-20B (HF)](https://huggingface.co/openai/gpt-oss-20b)
* [Ollama](https://github.com/ollama/ollama)
* [Portkey AI Gateway](https://github.com/Portkey-AI/gateway)
* [OpenRouter routing docs](https://openrouter.ai/docs/guides/routing/provider-selection)
* [Anthropic model overview](https://platform.claude.com/docs/en/about-claude/models/overview)
