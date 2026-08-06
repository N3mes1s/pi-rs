# agents-fleet — self-evolving build-safety agents

Three compiled agents (RFD 0028) that continuously verify the pi-rs build
is safe and sound, wired into the halo supervisor (RFD 0025) so their
findings are implemented, reviewed, merged, and learned from without a
human in the inner loop.

| Agent | Model | Watches | Verdict line |
| ----- | ----- | ------- | ------------ |
| `build-sentinel` | sonnet-4-6 / low | `cargo build --workspace` health, warning creep | `BUILD_SOUND` / `BUILD_BROKEN` |
| `test-warden` | sonnet-4-6 / medium | test suites; classifies real vs. stale vs. flaky failures | `TESTS_SOUND` / `TESTS_FAILING` / `TESTS_FLAKY` |
| `supply-chain-auditor` | opus-4-7 / high | cargo-deny, advisory/license drift, path-dep + `[patch]` tampering | `SUPPLY_CHAIN_SOUND` / `SUPPLY_CHAIN_ATTENTION` |

## How the loop self-evolves

```
                    ┌────────────────────────────────────────────┐
                    │ halo cycle n (RFD 0025, 8 canonical steps) │
   backlog ──────▶  │ pick → synthesise → prep_branch →          │
      ▲             │ orchestrate (implementer+reviewer) →       │
      │             │ keep_marker → smoke → rollback →           │
      │             │ evolve_tick  ── mutates AGENTS.md ─────────┼──▶ better
      │             └────────────────┬───────────────────────────┘    priors for
      │                              │ then (RFD 0028 §D.6)           cycle n+1
      │             ┌────────────────▼───────────────────────────┐
      └──────────── │ compiled-agent dispatch: build-sentinel,   │
  `pi --halo-add-   │ test-warden, supply-chain-auditor          │
   proposal` per    │ (each audits the freshly-merged tree)      │
   finding          └────────────────────────────────────────────┘
```

Three reinforcing feedback paths:

1. **Findings → fixes.** Each agent files its findings as backlog
   proposals; the next cycle's implementer + reviewer land the fix, and
   the agents re-audit the result one cycle later.
2. **Outcomes → priors.** `evolve_tick` (RFDs 0011/0013) rewrites
   AGENTS.md sections based on recorded success/failure outcomes, with
   benchmark-gated apply and rollback-on-regression — so what the loop
   learns about this repo compounds.
3. **The fleet rewrites itself.** The manifests in this directory are
   in-repo files, i.e. legitimate proposal targets. When an agent's
   workflow proves too noisy or too blind, halo can propose a prompt
   change here; the next dispatch recompiles and runs it. Guardrails
   still apply: every such change goes through the reviewer and lands
   on `halo/auto-merge`, never `main`.

## "Does a compiled agent recompile itself?"

No — and that's by design. An RFD 0028 binary is an immutable artifact:
at runtime it cannot alter its own prompt, tools, budgets, or code.
Evolution happens one level up, at *build time*, and the loop — not the
binary — is the "self" that evolves:

```
halo implementer edits agents-fleet/<name>.toml   (reviewed, merged)
        │
        ▼  next dispatch
fleet-run.sh: pi-build codegen → cargo build → exec fresh binary
```

halo dispatches `fleet-run.sh <name>` instead of a raw binary path. On
every dispatch the shim regenerates the crate from the manifest and
rebuilds. pi-build's codegen is deterministic, so an *unchanged*
manifest produces byte-identical sources and the rebuild is a ~1s cargo
cache hit; a manifest rewritten by the previous halo cycle produces a
genuinely new binary before the agent runs. The shim itself is in-repo
and goes through the same reviewed-proposal path if the loop wants to
change *how* rebuilding works. A rebuild failure exits 75 → halo's
`alert` policy, never a silently-stale agent.

## Build the fleet

The dispatch shim builds on demand — `./agents-fleet/fleet-run.sh
<name>` regenerates + rebuilds before running, so there is no separate
build step to forget. To pre-warm all three explicitly:

```sh
for a in build-sentinel test-warden supply-chain-auditor; do
  true | ./agents-fleet/fleet-run.sh $a --jsonl >/dev/null; done
```

(Each exits 64 — no prompt — after building.) The shim handles the two
local-build quirks itself until `pi-sdk` is published to crates.io: it
appends a `[patch.crates-io]` pin to this repo's `crates/pi-sdk` and an
empty `[workspace]` table (the generated crate lives under `target/`,
inside the pi-rs workspace dir, and must opt out).

## Run

One-shot, standalone (needs `ANTHROPIC_API_KEY`):

```sh
echo "Audit the current workspace build." | \
  ./agents-fleet/fleet-run.sh build-sentinel --jsonl
```

Supervised + self-evolving: copy `halo-build-safety.toml` to
`<halo-clone>/.pi/halo.toml` in a dedicated halo-owned clone matching
`~/halo/pi-rs*`, then:

```sh
pi --halo --halo-max-cycles 3   # bounded first run
pi --halo-status --watch        # live state
```

## Exit-code contract (RFD 0028 §Cross-cutting #5)

| Code | Meaning | halo policy here |
| ---- | ------- | ---------------- |
| 0 | clean run | continue |
| 1 | internal runtime error | alert |
| 2 | usage/config (missing `ANTHROPIC_API_KEY`, unknown model) | alert |
| 3 | budget/cap exhausted | throttle (exponential backoff, pause after 5) |
| 64 | no prompt on argv/stdin | alert |
