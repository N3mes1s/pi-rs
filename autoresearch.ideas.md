# autoresearch.ideas — pi-rs Tier 1 follow-ups

Notes carried out of the Tier-1 implementation pass for the next agent.

## B2 — Todo tool: deferred polish

Shipped: data model, persistence, full 5-op tool, registration, tests.

Deferred:
- `Block::Todo` renderer entry + render-above-editor display.
- `Ctrl+T` toggle for the panel — collides with the existing
  thinking-collapse mapping (which is the bare-Ctrl+T fallback in
  `handle_key` after the `OpenTree` keymap entry). Either:
  1. Re-bind thinking-collapse to a different chord (e.g. `Ctrl+H`)
     so `Ctrl+T` can take the todo panel, OR
  2. Pick a different chord for todo (e.g. `Ctrl+Y` — `y` for "yet
     another panel"). I'd lean toward option 2 to avoid breaking
     muscle memory.

## B4 — /background mode

Shipped: friendly `/background` slash that prints a "not yet" message.

Deferred (the full design):
- Detach the UI but keep `run_loop` running.
- Background-only event listener writes to
  `~/.pi/agent/sessions/<id>.jsonl`.
- Unix socket that `pi -c --attach` re-opens.
This is real work — process lifecycle, socket protocol, attach
handshake. Maybe a one-week project of its own.

## B5 — TTSR

Shipped: rule-loader + matcher + injection mechanic, fully unit-tested.

Deferred:
- Wiring into the *real* `run_loop` (which lives in `pi-agent-core`);
  the current pi-agent-core stream loop has no extension point for
  per-delta inspection. Wiring requires either a hook on
  `AgentSessionRuntime::stream_loop` or a wrapper provider that
  intercepts deltas. Pure plumbing, no design work.

## A4 — dashboard widget

The widget polls `autoresearch.jsonl` after every TurnComplete, which
costs an O(file-size) read on every assistant turn even when the
session does not have an autoresearch loop active. Cheap in absolute
terms (it's a file existence check + serde_json line parse), but
worth measuring once the session count grows.
