# pi-rs

A self-contained Rust rewrite of [pi](https://github.com/badlogic/pi-mono),
Mario Zechner's minimal terminal coding-agent harness. Same public surface,
same JSONL session format, same skill / extension contracts — plus a
batteries-included set of native modules: an autoresearch loop, a SQLite-backed
stats dashboard, worktree-isolated subagents, a flamegraph viewer, and a
self-improvement (`--evolve`) daemon that mutates `AGENTS.md` based on
recorded outcomes.

If you've used upstream `pi`, this is a drop-in. If you haven't: it's a
single static binary that talks to Anthropic, OpenAI, Google, Bedrock,
Azure, and ~12 OpenAI-compat providers, runs your code through a TUI or
in non-interactive print/JSON/RPC modes, and gives you somewhere to put
the Markdown that tells the agent how your repo works.

## Install

```sh
# Build the release binary (static-musl works too — see Makefile).
cargo build --release --bin pi
./target/release/pi --help

# Or symlink onto your PATH.
ln -sf "$PWD/target/release/pi" ~/.local/bin/pi
```

Provider credentials are read from environment variables at startup; no
keys ever land in committed files.

```sh
export ANTHROPIC_API_KEY=sk-ant-…
export OPENAI_API_KEY=sk-…
export GOOGLE_API_KEY=…
# or run `pi` then `/login` for OAuth (Anthropic, ChatGPT, Copilot, Gemini).
```

## Quick start

Three commands that exercise the headline features.

### 1. One-shot print mode

```sh
pi -p "say hi"
```

Reads stdin if piped, streams the assistant reply, exits.

### 2. Stats: ingest sessions, dump JSON

```sh
pi --stats sync && pi --stats json
```

`sync` walks every `~/.pi/agent/sessions/<cwd>/*.jsonl` file and folds
`Usage` / `Outcome` rows into `~/.pi/agent/stats.db`. `json` prints the
roll-up: tokens in/out, cache hit rate, $ spent, per-model and per-cwd
breakdowns. See [RFD 0004](rfd/0004-stats-crate.md).

### 3. Worktree-isolated task

```sh
pi --worktree -p "refactor crates/pi-tools/src/edit.rs to use anyhow"
```

Allocates a fresh `git worktree` under
`~/.pi/wt/data/<encoded-repo>/<task-id>/`, runs the agent there, then
either cherry-picks the result onto a `pi/task/<id>` branch or emits a
`.patch`. Your working tree is never touched. See
[RFD 0006](rfd/0006-worktree-isolated-tasks.md).

## Authoring an `AGENTS.md`

`AGENTS.md` (or `CLAUDE.md`) is the per-repo brief the agent reads on
every session. Pi-rs auto-discovers them by walking cwd → ancestors →
`~/.pi/agent/AGENTS.md`, sorts by depth, and joins them into the system
prompt. Use `--no-context-files` to opt out.

Anything between `<!-- pi:keep -->` and `<!-- /pi:keep -->` is treated
as a load-bearing invariant by the `--evolve` daemon (see below) — it
will rewrite the rest of the file but never touch a `pi:keep` block.
Outcomes recorded by the trajectory judge feed back into evolve, which
proposes `AGENTS.md` rewrites by H2 section.

```markdown
# AGENTS.md — my-cool-repo

<!-- pi:keep -->
## House rules
- Never use `--no-verify`.
- API keys are env-var only.
<!-- /pi:keep -->

## Where things live
- Source: `crates/foo/src/`
- Integration tests skip cleanly when their tool is absent.

## Optimisation lessons
<!-- This block is mutable. The evolve daemon may rewrite it. -->
- The legacy build target was 5× slower than the musl target.
```

See [RFD 0011](rfd/0011-self-dogfood-evolve.md) for the discovery and
mutation rules.

## Subagents (`task` tool)

The `task` tool delegates a self-contained unit of work to a subagent:
a fresh runtime with its own message history, model selection,
allowlisted tool registry, session JSONL, and (optionally) git
worktree. The subagent's full transcript collapses into a single
`tool_result` in the parent's stream, so context windows stay clean.

Subagents are Markdown-with-frontmatter files in
`~/.pi/agent/agents/*.md` or `<repo>/.pi/agents/*.md`:

```markdown
---
name: code-reviewer
description: Reads a diff and flags regressions. Read-only.
model: sonnet
tools: [read, grep, find, ls]
spawns: []
---
You are a careful code reviewer. Look for...
```

| Frontmatter | Meaning |
| --- | --- |
| `name` | Tool-call key. Required. |
| `description` | Shown to the parent. Required. |
| `model` | Role alias (`sonnet`, `opus`) or `provider/model`. |
| `tools` | Allowlist; empty = inherit parent registry. |
| `spawns` | `*` for unrestricted nesting, list for allowlist, omitted = no nested `task`. |
| `thinking` | `low` / `medium` / `high` / `off`. |

Use a subagent when (a) the work is read-only and benefits from a
clean context (review, search), or (b) the work mutates files and
should be sandboxed in its own worktree. See
[RFD 0005](rfd/0005-subagents-task-tool.md) and
[RFD 0006](rfd/0006-worktree-isolated-tasks.md).

## Stats dashboard

```sh
pi --stats sync          # JSONL → SQLite
pi --stats json          # roll-up to stdout
pi --stats server        # http://127.0.0.1:3847
```

`server` mounts an embedded React dashboard at
`http://127.0.0.1:3847`: per-model spend, cache hit rates, daily cost,
context-window heatmaps, $/session. Backed by `~/.pi/agent/stats.db`.
See [RFD 0004](rfd/0004-stats-crate.md). Cache-rate accuracy depends
on [RFDs 0008](rfd/0008-populate-usage-fields.md) /
[0010](rfd/0010-differential-cache-pricing.md) /
[0015](rfd/0015-usage-population-other-providers.md).

## Flamegraph

```sh
pi --flamegraph                                      # opens HTML viewer
pi --flamegraph --flamegraph-format json > traj.json # machine-readable
```

Renders a session's trajectory as a token-weighted flamegraph: which
tools / messages / context loads consumed the most context window.
The JSON shape exists so `--evolve` (and your own scripts) can ingest
trajectory shape without parsing HTML. See
[RFD 0012](rfd/0012-judge-context-and-flamegraph-json.md).

## Evolve

The evolve daemon reads recorded `Outcome` entries from your sessions,
identifies the worst-scoring H2 section in `AGENTS.md`, asks the slow
model for a rewrite, benchmarks the candidate against current on a
sample of replayed sessions, and either applies it or rolls back.

```sh
pi --evolve status     # show daily $ usage, queued candidates
pi --evolve dry-run    # propose a rewrite, print the diff, don't apply
pi --evolve apply      # land the rewrite if score delta ≥ MARGIN (default 0.10)
pi --evolve off        # disable
```

Auto-apply is gated by the rollback-on-regression contract in
`evolve::apply`: if the next batch of outcomes regresses, the change is
reverted. `<!-- pi:keep -->` blocks are never modified. Spend is bounded
by `daily_cost_cap_usd` in settings. See
[RFD 0013](rfd/0013-evolve-auto-apply.md) and
[RFD 0011](rfd/0011-self-dogfood-evolve.md).

## Where things live

| Crate | Purpose |
| --- | --- |
| `pi-ai` | Unified LLM API (Anthropic / OpenAI / Google / Bedrock / Azure / OpenAI-compat); SSE streaming, tool use, OAuth subscriptions, cost computation. |
| `pi-tools` | Built-in `read`, `write`, `edit`, `bash` plus disabled-by-default `grep`, `find`, `ls`. |
| `pi-agent-core` | Agent loop, JSONL sessions, compaction, settings, context-file discovery, runtime config. |
| `pi-tui` | Diff renderer, editor primitives, theme registry, picker overlays. |
| `pi-stats` | JSONL ingest → SQLite → axum HTTP API + embedded React dashboard. |
| `pi-coding-agent` | The `pi` binary: CLI, modes, slash commands, skills, prompts, themes, packages, extensions, and native modules (autoresearch, task, worktree, evolve, flamegraph, trajectory). |

Native modules live under `crates/pi-coding-agent/src/native/`:

```
autoresearch/  # init/run/log_experiment + autoresearch-create skill
task/          # subagent definition + executor (RFD 0005)
worktree/      # git-worktree allocation + reconcile (RFD 0006)
trajectory/    # outcome recorder + LLM judge (RFD 0011/0012)
evolve/        # AGENTS.md mutation daemon (RFD 0011/0013)
lsp/           # on-write formatter + diagnostics (RFD 0001/0007)
```

## Configuration

Pi-rs reads layered settings; project files override user files.

```
~/.pi/agent/settings.json     # global settings
~/.pi/agent/SYSTEM.md         # custom system-prompt addendum
~/.pi/agent/AGENTS.md         # global context
~/.pi/agent/keybindings.json  # custom keybindings
~/.pi/agent/sessions/         # JSONL sessions, organised by cwd
~/.pi/agent/agents/           # subagent definitions (RFD 0005)
~/.pi/agent/skills/           # skills (Agent Skills spec)
~/.pi/agent/prompts/          # slash-command prompt templates
~/.pi/agent/themes/           # themes
~/.pi/agent/packages/         # installed pi packages
~/.pi/agent/stats.db          # pi-stats SQLite (RFD 0004)
~/.pi/wt/data/                # worktree allocations (RFD 0006)

.pi/settings.json             # project overrides
.pi/agents/ .pi/skills/ .pi/prompts/ .pi/themes/
AGENTS.md / CLAUDE.md         # auto-loaded context (cwd + parents)
```

`settings.json` shape (abbreviated):

```jsonc
{
  "provider": "anthropic",
  "model": "claude-sonnet-4-6",
  "thinking": "off",                 // off | low | medium | high
  "model_roles": {                   // role aliases for subagents
    "opus": "anthropic/claude-opus-4-7",
    "sonnet": "anthropic/claude-sonnet-4-6",
    "haiku": "anthropic/claude-haiku-4-5"
  },
  "task": {
    "max_concurrency": 4,            // subagent fan-out cap
    "agent_models": {                // per-agent model overrides
      "code-reviewer": "haiku"
    }
  },
  "evolve": {
    "enabled": false,                // master switch, opt-in
    "daily_cost_cap_usd": 5.0,
    "min_samples": 20                // min outcomes before a tick
  },
  "lsp": {
    "enabled": false,
    "format_on_write": false,
    "diagnostics_on_write": true,
    "languages": { "rust": { "command": ["rust-analyzer"] } }
  }
}
```

Environment overrides:

```
PI_CODING_AGENT_DIR     override ~/.pi/agent
PI_PACKAGE_DIR          override package directory
PI_WORKTREE_ROOT        override ~/.pi/wt (used by tests)
PI_SKIP_VERSION_CHECK   skip startup version check
PI_TELEMETRY=0          opt out of telemetry (no-op anyway)
ANTHROPIC_API_KEY / OPENAI_API_KEY / GOOGLE_API_KEY / …
```

## Modes

| Mode | Flag | Behaviour |
| --- | --- | --- |
| Interactive | (default) | Raw-mode TUI: editor, scroll buffer, hotkeys, picker overlays, message queue (Enter steers, Alt+Enter follow-up). |
| Print | `-p`, `--print` | Reads stdin if piped, streams the assistant reply, exits. |
| JSON | `--json` | Print mode emitting a JSONL event stream for tooling. |
| RPC | `--rpc` | Bidirectional JSONL on stdin/stdout for process integration. |
| SDK | (library) | Use `pi-agent-core` directly: `AgentSession`, `SessionManager`, `Runtime`. |

## Built-in tools

```
read        path, [offset], [limit]            -> file content
write       path, content                      -> creates parent dirs, writes
edit        path, old_string, new_string,
            [replace_all]                      -> exact-match surgical edit
bash        command, [timeout_ms], [cwd]       -> stdout/stderr/exit
grep        pattern, [path], [glob]            -> matching lines
find        glob, [path]                       -> matching paths
ls          path                               -> directory listing
task        agent, assignment, [tasks]         -> subagent (RFD 0005)
```

`--tools <list>` allowlists, `--no-builtin-tools` and `--no-tools`
disable subsets.

## Further reading

The design behind every feature above lives in [`rfd/`](rfd/). Start
with [`rfd/README.md`](rfd/README.md) for the index. Highlights:

- [RFD 0001](rfd/0001-lsp-write-hook.md) — LSP-on-write hook.
- [RFD 0003](rfd/0003-adaptive-thinking.md) — adaptive thinking on Opus 4.7+.
- [RFD 0004](rfd/0004-stats-crate.md) — `pi --stats`.
- [RFD 0005](rfd/0005-subagents-task-tool.md) — `task` tool.
- [RFD 0006](rfd/0006-worktree-isolated-tasks.md) — `pi --worktree`.
- [RFD 0011](rfd/0011-self-dogfood-evolve.md) — `AGENTS.md` + evolve + flamegraph.
- [RFD 0012](rfd/0012-judge-context-and-flamegraph-json.md) — judge + flamegraph JSON.
- [RFD 0013](rfd/0013-evolve-auto-apply.md) — `--evolve apply` auto-apply.
- [RFD 0014](rfd/0014-real-tokenizer.md) — real tokenizer for `ContextLoad`.
- [RFD 0015](rfd/0015-usage-population-other-providers.md) — `Usage` everywhere.
