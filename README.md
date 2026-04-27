# pi-rs

**A 1:1 Rust rewrite of [pi](https://github.com/badlogic/pi-mono)** — Mario
Zechner's minimal terminal coding agent harness — packaged with native ports
of common pi extensions like
[pi-autoresearch](https://github.com/davebcn87/pi-autoresearch).

`pi-rs` is **not** a fork or a derivative; it's a faithful re-implementation
in Rust of the same agent harness, plus a small set of native modules that
make it self-contained for common workflows. It tracks pi's design philosophy
("if I don't need it, it won't be built") and matches its public surface so
upstream skills, prompt templates, and packages work without modification.

## Scope

### What pi-rs replicates from upstream pi

- **Built-in tools**: `read`, `write`, `edit`, `bash`, plus the
  disabled-by-default extras `grep`, `find`, `ls`.
- **Session management**: JSONL tree, parent-id branching, `/fork`,
  `/clone`, `/resume`, `/tree`. Default location
  `~/.pi/agent/sessions/<cwd-slug>/<session-id>.jsonl`.
- **Slash commands**: `/login` (OAuth PKCE — Anthropic, ChatGPT, GitHub
  Copilot, Gemini CLI, Antigravity), `/logout`, `/model`, `/scoped-models`,
  `/settings` (interactive), `/resume`, `/tree`, `/fork`, `/clone`,
  `/compact` (heuristic + LLM-driven), `/export` (HTML), `/share`
  (`gh gist create`), `/hotkeys`, `/help`, `/quit`, plus `/<name>.md`
  prompt templates.
- **Skills** (Agent Skills spec): `~/.pi/agent/skills/`, `.agents/skills/`,
  `.pi/skills/`. The `autoresearch-create` skill from upstream
  pi-autoresearch ships built-in.
- **Themes**: dark + light + JSON-loadable user themes, hot-reloaded by the
  TUI render loop via `notify`.
- **Keybindings**: defaults match upstream + JSON overrides at
  `~/.pi/agent/keybindings.json`. Extensions can register their own chords.
- **Operating modes**: Interactive (raw-mode TUI with picker overlays for
  every flow), Print (`-p`), JSON event stream (`--json`), RPC
  (bidirectional JSONL), SDK (the workspace crates are usable as a library).
- **Multi-provider LLM API**: Anthropic Messages, OpenAI Chat Completions,
  OpenAI-compat (Fireworks, Cerebras, Groq, xAI, OpenRouter, DeepSeek,
  Mistral, ZAI, Hugging Face, Ollama, Kimi, MiniMax), Google Generative AI
  (Gemini), AWS Bedrock (Anthropic), Azure OpenAI. Streaming SSE, tool use,
  thinking budgets, OAuth subscriptions.
- **Extensions**: subprocess-based (Rust-idiomatic — matches pi.dev's
  "CLI tools with READMEs" stance). Extensions can register tools, slash
  commands, keybindings, event hooks (`tool_call`, `tool_result`,
  `assistant_message`, `user_message`), replace built-in tools, and run
  async startup hooks. Manifest schema documented in
  `crates/pi-coding-agent/src/extensions.rs`.
- **Packages**: `pi install npm:…|git:…|https://…` from the package
  registry. Auto-discovery of `pi-extension.json` manifests.

### Native modules (Rust ports of common pi extensions)

These are bundled in pi-rs rather than installed as separate packages:

- **pi-autoresearch** — autonomous experiment loop with confidence scoring,
  dashboard, and JSONL persistence. Tools `init_experiment`,
  `run_experiment`, `log_experiment` match upstream byte-for-byte; skill
  `autoresearch-create` ships built-in. See
  `crates/pi-coding-agent/src/autoresearch/` and
  `crates/pi-coding-agent/skills/autoresearch-create/SKILL.md`.

### Planned native modules

- **auto-approval** — pre-approve tool calls matching a policy
  (read/write/bash patterns) so long-running runs don't stall on every
  permission prompt. Shape mirroring upstream pi's permission flow.
- **pi-pods** — vLLM/llama.cpp-pod orchestration, like upstream's
  `packages/pods`.
- **pi-mom** — Slack bot bridge analogous to upstream's `packages/mom`.
- **pi-web-ui** — browser-served UI mirroring `packages/web-ui`.

## Workspace

| Crate | Purpose |
| --- | --- |
| `pi-ai` | Unified LLM API (Anthropic / OpenAI / Google / Bedrock / Azure / OpenAI-compat) |
| `pi-tools` | Built-in `read`, `write`, `edit`, `bash`, `grep`, `find`, `ls` |
| `pi-agent-core` | Agent loop, JSONL sessions, compaction, settings |
| `pi-tui` | Diff renderer + editor primitives + theme registry |
| `pi-coding-agent` | The `pi` binary: CLI, modes, slash commands, skills, prompts, themes, packages, extensions, autoresearch |

## Build

```sh
cargo build --release -p pi-coding-agent
./target/release/pi --help
```

## Quick start

```sh
# Print mode (non-interactive, exits after response)
pi -p "list files in this directory"

# JSON event stream
pi --json -p "read README.md and summarise it"

# Continue most recent session
pi -c

# Pick model / provider explicitly
pi --provider anthropic --model claude-sonnet-4-6 "implement quicksort in rust"
pi --provider openai --model gpt-4o "..."

# Run an autoresearch loop on any repo
cd /path/to/some/repo
pi --provider anthropic --model claude-opus-4-7 \
    "/autoresearch optimize cargo build time, target the test workload"
```

## Configuration

Directory layout matches upstream pi:

```
~/.pi/agent/settings.json     # global settings
~/.pi/agent/SYSTEM.md         # custom system prompt addendum
~/.pi/agent/AGENTS.md         # global context
~/.pi/agent/keybindings.json  # custom keybindings
~/.pi/agent/sessions/         # JSONL sessions, organised by cwd
~/.pi/agent/skills/           # skills (Agent Skills spec)
~/.pi/agent/prompts/          # slash-command prompt templates
~/.pi/agent/themes/           # themes
~/.pi/agent/packages/         # installed pi packages

.pi/settings.json             # project overrides
.pi/SYSTEM.md / .pi/skills / .pi/prompts / .pi/themes
AGENTS.md / CLAUDE.md         # auto-loaded context (cwd + parents)
```

Environment:

```
PI_CODING_AGENT_DIR     override ~/.pi/agent
PI_PACKAGE_DIR          override package directory
PI_TELEMETRY=0          opt out of telemetry (no-op anyway)
PI_SKIP_VERSION_CHECK   skip startup version check
ANTHROPIC_API_KEY / OPENAI_API_KEY / GOOGLE_API_KEY / ...
                        provider credentials
```

## Modes

| Mode | Flag | Behaviour |
| --- | --- | --- |
| Interactive | (default) | Raw-mode TUI: editor, scroll buffer, hotkeys, picker overlays, message queue (Enter steers, Alt+Enter follow-up). |
| Print | `-p / --print` | Reads stdin if piped, streams the assistant reply, exits. |
| JSON | `--json` | Same as print but emits a JSONL event stream. |
| RPC | `--rpc` | Bidirectional JSONL on stdin/stdout for process integration. |
| SDK | (library) | Use `pi-agent-core` directly: `AgentSession`, `SessionManager`, etc. |

## Built-in tools

```
read        path, [offset], [limit]            -> file content (image -> attachment)
write       path, content                      -> creates parent dirs, writes
edit        path, old_string, new_string,
            [replace_all]                      -> exact-match surgical edit
bash        command, [timeout_ms], [cwd]       -> stdout/stderr/exit
grep        pattern, [path], [glob], [-n]      -> matching lines
find        glob, [path]                       -> matching paths
ls          path                               -> directory listing
```

`--tools <list>` allowlist, `--no-builtin-tools`, `--no-tools` work as in upstream pi.

## Autoresearch (native module)

`pi-rs` ships pi-autoresearch as a native module. The on-disk schema and tool
contracts are byte-for-byte compatible with upstream
[pi-autoresearch](https://github.com/davebcn87/pi-autoresearch):

```
autoresearch.config.json (optional, in cwd)
autoresearch.md          (session document — agent writes via the skill)
autoresearch.sh          (benchmark, prints METRIC name=value lines)
autoresearch.checks.sh   (optional correctness gate, runs after benchmark)
autoresearch.jsonl       (append-only run log)
autoresearch.ideas.md    (optional deferred-idea backlog)
```

Tools: `init_experiment`, `run_experiment`, `log_experiment`. Status enum:
`keep` / `discard` / `crash` / `checks_failed` (only `keep` triggers a git
commit; the rest auto-revert via `git reset --hard`).

The upstream `autoresearch-create` skill is shipped built-in at
`crates/pi-coding-agent/skills/autoresearch-create/SKILL.md`. To start a
session, just `/autoresearch <goal>` or `pi -p "/autoresearch optimize
build time of this project"` and the agent will write the session files
and start the loop autonomously.

## Sessions

JSONL tree, one entry per message with `id` + `parent_id`. Stored under
`~/.pi/agent/sessions/<cwd-slug>/<session-id>.jsonl`. Branching via `/fork`
and `/clone`, time-travel via `/tree`.

## Status

* All four operating modes wired up.
* All seven slash-command flows (login/share/clone/scoped-models/settings/
  resume/tree/fork) implemented.
* Autoresearch native module shipped with upstream-compatible JSONL schema.
* Live-tested against the Anthropic API (Opus 4.7 + Sonnet 4.6 + Haiku 4.5).
* Coverage on the testable surface: ≥ 90% lines / ≥ 90% functions.
