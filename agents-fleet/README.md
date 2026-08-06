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

## Architecture: immutable core + evolving brain

Each agent is split in two, so it can evolve **without rebuilding and
without access to the pi-rs source code**:

```
┌─────────────────────────────────────────────────────────────┐
│ CORE — compiled binary (immutable, RFD 0028)                │
│   provider + model + auth · tool allowlist · token/recursion │
│   caps · JSONL event contract · exit-code contract           │
│   Changes REQUIRE pi-build + review → new versioned artifact │
├─────────────────────────────────────────────────────────────┤
│ BRAIN — brains/<name>.md (mutable, runtime-loaded)          │
│   the workflow: what to check, how to classify, what to      │
│   propose, output format                                     │
│   Changes are a text edit; picked up on the NEXT dispatch —  │
│   no toolchain, no rebuild, evolve-style candidate + review  │
└─────────────────────────────────────────────────────────────┘
```

The core is generated with `system_prompt_file = "agents-fleet/brains/<name>.md"`
(a pi-build manifest knob): at startup the binary loads the brain fresh;
if the file is missing it falls back to a baked degraded-mode prompt
that reports the problem and refuses to guess. This is the RFDs
0011/0013 pattern (frozen `pi` binary + evolving AGENTS.md) applied to
compiled agents. The safety-relevant surface — which tools the agent may
use, its spend caps, its provider — is deliberately NOT in the brain: a
brain edit can never grant itself new tools or a bigger budget.

## How an agent knows it has been evolved

Self-knowledge is layered:

- **`--version`** (baked at compile time): `name version (provider/model)
  manifest-sha256:<core identity> brain-fnv64:<brain fingerprint>`. The
  manifest sha is the core's content-addressed identity; version strings
  can lie, hashes can't.
- **Lineage journal** (`~/.pi/fleet/<name>.lineage.jsonl`, written by the
  dispatch shim): one row per dispatch with core sha, brain sha, and
  `evolved` / `brain_evolved` flags computed against the previous row. A
  core change without an `[agent].version` bump is flagged.
- **Step 0 in every brain**: the agent starts each run by hashing its own
  manifest + brain, reading its lineage tail and `git log` on its brain
  file. If it detects it was evolved since the last run, it must diff the
  change and state out loud what changed about itself. The
  supply-chain-auditor is additionally instructed to treat a brain
  revision that *weakens its own checks* as a priority-1.0 finding
  rather than complying silently.

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
   AGENTS.md sections based on recorded outcomes, benchmark-gated with
   rollback-on-regression.
3. **The fleet rewrites its own brains.** When a run exposes a deficiency
   in an agent's *own* workflow, the agent writes a complete revised
   brain to `brains/<name>.md.candidate` and files a proposal to promote
   it. It never overwrites its live brain directly — promotion goes
   through halo's reviewed loop, lands on `halo/auto-merge`, and takes
   effect on the next dispatch with no rebuild.

## Real-deployment topology

Two planes, connected only by artifacts:

- **Build plane** (CI or the halo clone — has the pi-rs toolchain).
  Core changes: manifest edit → review → `pi-build` → versioned binary
  published (release/registry). Brain changes: text edit → review →
  merged; ships with the repo checkout or any file sync.
- **Run plane** (deployment host — NO toolchain). Needs: the halo
  supervisor (`pi`), the fleet binaries, the brains/ directory, halo.toml.
  Brains evolve in place; cores evolve by artifact swap. `fleet-run.sh`
  in `FLEET_MODE=run` refuses to run a binary whose baked manifest-sha
  no longer matches the manifest on disk (exit 76 → halo alerts on a
  stale core instead of silently auditing with yesterday's caps).

In this repo halo supervises pi-rs itself, so both planes coincide and
`FLEET_MODE=rebuild` (auto-detected) regenerates + rebuilds on dispatch —
a cargo cache hit (~1s) when the manifest is unchanged.

## Build the fleet

The dispatch shim builds on demand in rebuild mode — there is no
separate build step to forget. To pre-warm all three explicitly:

```sh
for a in build-sentinel test-warden supply-chain-auditor; do
  true | ./agents-fleet/fleet-run.sh $a --jsonl >/dev/null; done
```

(Each exits 64 — no prompt — after building.) The shim handles the two
local-build quirks until `pi-sdk` is published to crates.io: it appends
a `[patch.crates-io]` pin to this repo's `crates/pi-sdk` and an empty
`[workspace]` table (the generated crate lives under `target/`, inside
the pi-rs workspace dir, and must opt out).

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

## Exit-code contract

| Code | Meaning | halo policy here |
| ---- | ------- | ---------------- |
| 0 | clean run | continue |
| 1 | internal runtime error (RFD 0028) | alert |
| 2 | usage/config: missing `ANTHROPIC_API_KEY`, unknown model (RFD 0028) | alert |
| 3 | budget/cap exhausted (RFD 0028) | throttle (exponential backoff, pause after 5) |
| 64 | no prompt on argv/stdin (RFD 0028) | alert |
| 66 | manifest missing (shim, rebuild mode) | alert |
| 69 | artifact missing (shim, run mode) | alert |
| 75 | rebuild failed (shim, rebuild mode) | alert |
| 76 | stale core artifact (shim, run mode) | alert |
