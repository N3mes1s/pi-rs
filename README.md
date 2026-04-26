# pi-rs

A complete Rust reimplementation of [pi](https://github.com/badlogic/pi-mono) — the minimal terminal coding agent harness by Mario Zechner — built as an experiment in this Playground.

`pi-rs` keeps pi's design philosophy ("if I don't need it, it won't be built") and matches its
public surface: four built-in tools (read, write, edit, bash), session JSONL with branching,
slash commands, skills, prompt templates, themes, AGENTS.md context loading, four operating
modes (interactive / print / json / rpc), an SDK crate, and multi-provider LLM support.

## Workspace

| Crate | Mirror of | Purpose |
| --- | --- | --- |
| `pi-ai` | `pi-ai` | Unified LLM API: Anthropic Messages, OpenAI Chat Completions, OpenAI-compat (DeepSeek, Groq, Cerebras, xAI, OpenRouter, Mistral, Fireworks, ZAI…) |
| `pi-tools` | (built-ins) | `read`, `write`, `edit`, `bash`, `grep`, `find`, `ls` |
| `pi-agent-core` | `pi-agent-core` | Agent loop, session manager (JSONL tree), event stream, compaction |
| `pi-tui` | `pi-tui` | Terminal UI with differential rendering and editor primitives |
| `pi-coding-agent` | `pi-coding-agent` | The `pi` binary: CLI parsing, modes, slash commands, packages, skills, prompts, themes |

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

# Resume with picker
pi -r

# Pipe input
cat README.md | pi -p "summarise this text"

# Pick model / provider explicitly
pi --provider anthropic --model claude-sonnet-4-6 "implement quicksort in rust"
pi --provider openai --model gpt-4o "..."
```

## Configuration

Mirrors pi's directory layout:

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
PI_CODING_AGENT_DIR    override ~/.pi/agent
PI_PACKAGE_DIR         override package directory
PI_TELEMETRY=0         opt out of telemetry (telemetry is a no-op anyway)
PI_SKIP_VERSION_CHECK  skip startup version check
ANTHROPIC_API_KEY / OPENAI_API_KEY / ...   provider credentials
```

## Modes

| Mode | Flag | Behaviour |
| --- | --- | --- |
| Interactive | (default) | TUI editor + scrolling messages, slash commands, queue (Enter steers, Alt+Enter follow-up) |
| Print | `-p / --print` | Reads stdin if piped, runs prompt, prints final assistant text, exits |
| JSON | `--json` | Same as print but emits a JSONL event stream (one event per line) |
| RPC | `--rpc` | Bidirectional JSONL on stdin/stdout for process integration |
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

## Slash commands

`/login /logout /model /scoped-models /settings /resume /tree /fork /clone
/compact /export /share /hotkeys /quit /help` — plus any `<name>.md` template
in `~/.pi/agent/prompts/` becomes `/<name>` automatically.

## Sessions

JSONL tree, one entry per message with `id` + `parent_id`. Stored under
`~/.pi/agent/sessions/<cwd-slug>/<session-id>.jsonl`. Branching via `/fork` and
`/clone`, time-travel via `/tree`.

## Status

This is an experiment — the public surface and behaviour mirror pi as documented in
`mariozechner.at/posts/2025-11-30-pi-coding-agent` and the pi-mono README. Expect rough
edges around the most TUI-heavy interactions; the agent loop, sessions, providers,
tools, slash commands, and print/json/rpc modes are all wired up.
