# RFD 0020 — Autonomous model router for pi-rs

- **Status:** Discussion
- **Author:** pi-rs maintainers
- **Created:** 2026-04-28
- **Implemented:** &lt;pending&gt;

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

This RFD proposes **a three-stage autonomous routing pipeline**
implemented inside pi-rs:

1. **Tier-0 classifier** — a sub-50 ms BERT-or-embedding
   classifier over a small set of named routes
   (`trivial-edit`, `single-file`, `multi-file-refactor`,
   `research`, `reasoning`, `tool-heavy`) that maps each request
   to a (provider, model, thinking) tuple. Architectural
   inspiration: vLLM Semantic Router v0.1 "Iris" (98× latency
   reduction vs. an LLM-as-router) and aurelio-labs's
   embedding-cosine router (zero-training baseline).
2. **Cascade with unified deferral** — execute on the cheapest
   classified tier, run a deferral signal (the existing pi-rs
   trajectory judge is one source; a FrugalGPT-style DistilBERT
   scorer is another), and escalate on low confidence. Use the
   "cascade routing" decision rule from Dekoninck/Baader/Vechev
   (ETH SRI, ICML 2025) which strictly dominates router-only
   and cascade-only baselines on RouterBench.
3. **TALE-EP token-budget self-prediction** — the system prompt
   asks the executing model to first emit `&lt;budget&gt;N&lt;/budget&gt;`
   and then answer within it. Zero-training, drops in as a
   prompt change, reported 45% token reduction.

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

#### Model registry (`crates/pi-ai/src/registry.rs:24-52`)

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
pricing audit. `ModelRegistry::resolve()` (lines 105-121) is
the lookup mechanism; the router can call it to validate any
candidate decision.

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
  compared to centroids, threshold-decided. Single-digit ms
  with cached embeddings. Zero training, ships as the v0
  baseline.
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
* **Anthropic's official tiering** — "Sonnet default; route
  easy down to Haiku; escalate hard to Opus" — formalized as a
  3-tier stack, **reportedly cuts API spend 70-90% in
  production**. **This is the empirical floor pi-rs's autonomous
  router must beat.** Cleanest v1 ship: codify this rule, then
  learn the deviations from pi-stats logs.

## Proposal

A three-stage pipeline. Each stage is independently shippable.

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
   │  ModernBERT-style classifier OR     │
   │  embedding-cosine baseline:         │
   │    {trivial-edit, single-file,      │
   │     multi-file, research,           │
   │     reasoning, tool-heavy}          │
   │  → (provider, model, ThinkingLevel) │
   │  + budget: u32                      │
   │  Sub-50 ms on CPU.                  │
   └────────────┬────────────────────────┘
                │
                ▼
   ┌─────────────────────────────────────┐
   │  Stage 2: Cascade with deferral      │
   │  ───────────────────────────────     │
   │  Run on classified tier.             │
   │  Trajectory-judge-style scorer       │
   │  evaluates the result.               │
   │  Apply ETH unified routing-cascade   │
   │  rule:                               │
   │   accept if "likely sufficient";     │
   │   escalate to next tier if not;      │
   │   skip tiers if "obviously not       │
   │    enough" (DP).                     │
   └────────────┬────────────────────────┘
                │
                ▼
   ┌─────────────────────────────────────┐
   │  Stage 3: TALE-EP budget enforcement │
   │  ────────────────────────────────    │
   │  System prompt instructs the chosen  │
   │  model:                              │
   │    "First emit <budget>N</budget>;   │
   │     then answer within N tokens."    │
   │  Runtime hard-caps `max_tokens` at   │
   │  N + safety margin.                  │
   └────────────┬────────────────────────┘
                │
                ▼
   stream events back to caller as today
```

The pipeline is **opt-in** for v1 (`pi --route auto`), default
for v2 once empirical Pareto curves on pi-stats data
demonstrate it beats Anthropic's manual 3-tier rule.

### Stage 1: Tier-0 classifier

#### Routes

A small, named, *operator-defined* set. Six routes covers most
agentic-coding traffic; the system supports adding more without
retraining (each route is a centroid + threshold over example
prompts).

| Route id            | Example                                | Default tier            |
| ------------------- | -------------------------------------- | ----------------------- |
| `trivial-edit`      | "rename `foo` to `bar` in this file"   | Haiku 4.5 + Off         |
| `single-file`       | "add a Display impl"                   | Sonnet 4.6 + Low        |
| `multi-file`        | "extract this trait into its own crate" | Sonnet 4.6 + Medium    |
| `research`          | "audit OpenAI's Responses API"         | Sonnet 4.6 + Medium + web_search |
| `reasoning`         | "prove this loop terminates"           | gpt-5.4 Responses + High |
| `tool-heavy`        | "run the test suite, then fix what fails" | Sonnet 4.6 + Medium  |

Defaults are seedable from Anthropic's 3-tier guidance + the
RFD-0009 pricing audit. The routing table lives in
`~/.pi/agent/router.toml` (per user) and
`<repo>/.pi/router.toml` (per repo, takes precedence). Both
files are observable, hand-editable, and version-controllable.

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
    pub thinking:        ThinkingLevel,  // ThinkingLevel::Off
    pub max_tokens:      Option<u32>,    // budget for stage 3
    pub route_id:        String,         // "trivial-edit" — for telemetry
    pub confidence:      f32,            // 0..1, drives stage 2 deferral
    pub fallback_chain:  Vec<(String, String, ThinkingLevel)>,
}
```

The `fallback_chain` is the cascade hierarchy stage 2 climbs
when the deferral signal fires. The classifier seeds a
sensible default; user can override per route.

### Stage 2: Cascade with unified deferral

The ETH 2024 algorithm: at each step, evaluate two
probabilities:

* `p_optimal`: this is the right model
* `p_sufficient`: this model's answer will pass the deferral
  test

and pick the model maximizing `(quality - cost · λ) ·
p_sufficient`. λ is the user's cost-quality tradeoff knob
(in `~/.pi/agent/router.toml`).

**Deferral signals**, in order of cost:

1. **TALE-EP self-consistency** — the model emits its own
   budget; if it goes over, the deferral fires. Free.
2. **Trajectory judge** — the existing pi-rs judge (Haiku),
   reused as a per-request gate rather than post-hoc audit.
   ~$0.001/check.
3. **DistilBERT scorer** (FrugalGPT-style) — trained on
   pi-stats × judge labels offline, deployed via `ort`. ~5 ms
   inference. Ship in v2 once we have enough labelled data.

**Cascade tiers**, default chain (overridable):

```
ollama:tier-0 (free)  →  haiku  →  sonnet  →  opus  →  gpt-5.4 high
```

`ollama:tier-0` is **opt-in**: only if `~/.pi/agent/router.toml`
sets `enable_local = true` AND `GET http://localhost:11434/api/tags`
returns at least one model. We do not assume Ollama is running.

### Stage 3: TALE-EP budget controller

A system-prompt addendum:

```
Before answering, on a single line, emit:
  <budget>N</budget>
where N is your best estimate of the token count needed for
a high-quality answer. Then answer in at most N tokens.
```

Runtime hard-cap: `max_tokens = parsed_budget + 50` (safety
margin for the closing tag and accidental over-runs). On parse
failure, fall back to the route's default `max_tokens`. The
TALE paper reports 45.3% reduction on average; pi-rs validates
this on its own workload via pi-stats before turning it on by
default.

`Thinking::Adaptive` (RFD 0003) **stays**, used in addition to
TALE-EP — TALE-EP caps the *output*, Adaptive controls
*reasoning compute*. The two are orthogonal.

## Pi-rs concrete design

### New crate: `pi-router`

Sits between `pi-coding-agent` and `pi-ai`. Public API:

```rust
pub trait Router: Send + Sync {
    /// Pick a model + thinking + budget for this request.
    fn route(
        &self,
        prompt: &str,
        history: &[Message],
        tools: &[ToolSpec],
        ctx: &RoutingContext,
    ) -> Result<RoutingDecision>;

    /// Update the router's internal state from a completed
    /// trajectory (cost, judge verdict, latency). Used for
    /// online learning / Pareto frontier tracking.
    fn observe(&self, decision: &RoutingDecision, outcome: &Outcome);
}

pub struct RoutingContext<'a> {
    pub registry:     &'a ModelRegistry,
    pub stats_db:     Option<&'a pi_stats::Connection>,
    pub user_lambda:  f64,           // cost↔quality tradeoff
    pub force:        Option<ForceOverride>,
    pub session_id:   &'a str,
}
```

Three concrete `Router` implementations, ordered by ship date:

1. **`StaticRouter`** — reads the routing table directly.
   Equivalent to today's manual model picking but expressed in
   the router shape. Ships day 0 of this RFD; lets the rest of
   the runtime integrate without depending on the classifier.
2. **`EmbeddingRouter`** — Stage 1 + Stage 2 + Stage 3, with
   the embedding-cosine classifier. Ships in milestone 1.
3. **`LearnedRouter`** — Stage 1 swapped for the ModernBERT
   classifier trained on pi-stats. Ships in milestone 2.

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
id               = "trivial-edit"
examples         = [
  "rename {foo} to {bar}",
  "add a doc comment to {func}",
  "remove this println",
]
threshold        = 0.72
provider         = "anthropic"
model            = "claude-haiku-4-5-20251001"
thinking         = "off"
max_tokens       = 800
fallback_chain   = ["sonnet:low", "sonnet:medium"]

[[route]]
id               = "reasoning"
examples         = [
  "prove that {claim}",
  "find a counterexample to {prop}",
  "is this loop guaranteed to terminate?",
]
threshold        = 0.65
provider         = "openai"
model            = "gpt-5.4"
thinking         = "high"
max_tokens       = 8000
fallback_chain   = ["opus:high"]

# … five more routes …

[router.learn]
flamegraph_path  = "~/.pi/sessions/*/flamegraph.json"
update_cooldown  = "24h"
min_samples      = 50
cost_cap_per_day = 0.50            # USD spent on learning
```

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

1. **`crates/pi-router/tests/embedding_router_routes.rs`** —
   given a fixed `router.toml` with 6 routes and 5 examples
   each, hit the router with 30 hand-labelled prompts; assert
   correct route ≥ 90% of the time. Pure: no network.
2. **`crates/pi-router/tests/cascade_decision_rule.rs`** — given
   a fixed (cost, p_optimal, p_sufficient) table for 4 tiers,
   assert the ETH unified rule returns the cost-optimal tier.
   Hand-derived golden output.
3. **`crates/pi-router/tests/tale_ep_budget_extraction.rs`** —
   feed model output beginning with `<budget>123</budget>...`,
   assert the runtime hard-cap is applied. Round-trip with
   parse failures (`<budget>foo</budget>` → fallback to default).
4. **`crates/pi-router/tests/static_router_compat.rs`** — assert
   that with `mode = "static"`, the router emits the same
   decision as today's CLI flag dispatch for 20 sample requests.
   The router is non-disruptive by default.
5. **`crates/pi-router/tests/observe_updates_stats.rs`** —
   `Router::observe()` writes a `routing_decision` SessionEntry,
   `pi_stats::ingest::sync_all` picks it up, `dashboard()`
   surfaces it in `by_route_id`. End-to-end smoke.
6. **End-to-end manual** — `pi --route auto -p
   "rename foo to bar in src/main.rs"` routes to Haiku, costs
   < $0.001. `pi --route auto -p "prove the borrow checker is
   sound"` routes to gpt-5.4 high. Both visible in `pi /cost`.

### Empirical validation (post-ship)

* Run RouterBench-style evaluation on pi-stats trajectories:
  re-emit each historical request through the router with
  every (route, model) combo, plot the Pareto frontier, assert
  the router's chosen path is on or near the frontier.
* AB-test against Anthropic's manual 3-tier rule: 100 sessions
  with `mode = "static"` (manual tiers), 100 with `mode =
  "auto"` (learned router). Compare cost, judge-pass rate, and
  user-reported quality. Target: ≥ 30% cost reduction at
  iso-quality before flipping `mode = "auto"` to default.

## Out of scope

* **xRouter-style LLM router** — too expensive at pi-rs's per-
  request scale; would dominate the cost the router saves.
* **Speculative decoding** — pi-rs doesn't host its own
  weights; it dispatches to provider HTTP APIs. If we ever ship
  a local-first mode, this RFD's `tier:0 = ollama` slot is the
  natural place to layer spec-decode in via llama.cpp's
  built-in support.
* **Multi-round routing (Router-R1)** — the cascade already
  gives most of the multi-round benefit, and Router-R1's
  multi-LLM aggregation conflicts with pi-rs's single-stream
  output contract.
* **VeriMAP-style planner-emitted VFs per subtask** — the
  trajectory judge is a single VF over the whole turn, which
  is enough for v1. Per-subtask VFs are a follow-up RFD.
* **Per-token streaming budget enforcement** — TALE-EP caps the
  full response; mid-stream interruption would require provider
  cooperation we don't have today.
* **Cross-provider Pareto evaluation harness** — landing
  RouterBench's harness as a pi-rs CI dependency is its own
  RFD; for now we emit the cost/quality JSON and keep the plot
  external.

## Open questions

1. **Is Anthropic's 3-tier rule strong enough as v1?** The
   literature suggests learned routers add ~10-30% cost
   improvement on top, but only after ≥1k labelled trajectories.
   Pi-stats today has thousands per heavy user; for new users,
   the manual tiers are the only signal. **Lean: ship
   `EmbeddingRouter` with manual centroids, gate
   `LearnedRouter` behind ≥500 trajectories in pi-stats.**
2. **Where do the routing centroid examples live?** Per-user
   (`~/.pi/agent/router.toml`), per-repo
   (`<repo>/.pi/router.toml`), or bundled defaults
   (`crates/pi-router/data/default_routes.toml`)? **Lean:
   bundled defaults override-able by per-repo, override-able by
   per-user.** Pi-rs precedent matches this (AGENTS.md, agent
   files).
3. **Do we expose λ (cost-quality tradeoff) to the user?**
   `pi --route auto --lambda 2.0` (heavy on quality) vs. the
   default `1.0`. **Lean: yes; trivial to add and useful.**
4. **What about the deferral signal cost?** Calling the
   trajectory judge per request adds latency. **Lean: gate
   stage 2 by `confidence < 0.85` from stage 1; if the
   classifier is confident, skip the judge.**
5. **Budget compliance enforcement** — TALE-EP's hard cap can
   truncate mid-answer. Do we re-prompt with a higher budget?
   **Lean: yes, if the response ends mid-token (no stop reason
   = "stop") and the deferral fires.**
6. **Multi-tenancy + safety** — should the router refuse to
   send PII to local models with weaker guardrails?
   **Lean: yes, Stage 1 includes a PII-detector head (vLLM
   Iris already does this); on positive detection, force the
   tier ≥ 1 paid path.**
7. **Compatibility with subagents that pin a model.** Today
   `.pi/agents/code-reviewer.md` says `model: gpt-5.4`. **Lean:
   pinned model = force override; the router does not second-
   guess the agent author.**

## Implementation plan

This RFD splits naturally into milestones, each shippable
independently and dogfood-able through pi-rs's own evolve loop.

| Milestone | Worktree                        | Scope                                                                                                              | Est. LOC |
| --------- | ------------------------------- | ------------------------------------------------------------------------------------------------------------------ | -------- |
| **M1**    | `claude/router-static`          | New `pi-router` crate. `Router` trait + `StaticRouter`. Reads `router.toml`. `--route static` flag wired in CLI.    | ~500     |
| **M2**    | `claude/router-classifier`      | Embedding-cosine classifier. `EmbeddingRouter`. Ships with default 6-route bundle. `--route auto` is now valid.   | ~700     |
| **M3**    | `claude/router-cascade`         | ETH unified routing-cascade decision rule + judge integration as deferral signal. Cascade tier descent.            | ~600     |
| **M4**    | `claude/router-tale-ep`         | TALE-EP system-prompt addendum + budget extractor + runtime hard-cap. Off by default; turn on per-route in TOML.   | ~250     |
| **M5**    | `claude/router-stats`           | `pi-stats` extensions: `by_route_id`, dashboard panel, `routing_decision` SessionEntry kind, flamegraph annotation. | ~400     |
| **M6**    | `claude/router-evolve`          | `RoutingMutator` + plumbing into the evolve daemon. AGENTS.md gains a routing section.                              | ~500     |
| **M7**    | `claude/router-learned`         | ModernBERT-style classifier head trained on pi-stats. Replaces the embedding classifier when `≥500` samples.        | ~600     |
| **M8**    | `claude/router-ollama-tier0`    | Local-model discovery, tool-call validator, fall-through escalation.                                                | ~400     |

Each milestone follows the established pi-rs dogfood pattern
(RFD 0011 / 0019): branch from main, implement on Opus 4.7 in
a worktree, generate a commit message that fully documents
the diff, push for review by the bundled `code-reviewer`
subagent (now functional on `gpt-5.4` after RFD 0019 landed),
merge via the generic merge orchestrator. Total: roughly
~4000 LOC across 8 worktrees, expected ~$5–10 in dogfood spend.

The order is dependency-first (M1 unlocks everything; M2
unlocks M3-4; M5 unlocks M7) but M4 (TALE-EP) and M6 (evolve
plumbing) can run in parallel with M3.

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
