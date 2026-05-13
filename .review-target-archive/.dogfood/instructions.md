# Dogfood: pi-rs builds pi-rs

This directory holds the prompts pi-rs uses to extend itself with the
Opus 4.7 backend. Each prompt is a single self-contained task. They are
applied with:

```sh
ANTHROPIC_API_KEY=… ./target/release/pi \
    --provider anthropic --model claude-opus-4-7 \
    --no-context-files --no-session \
    -p "$(cat .dogfood/<task>.md)"
```

After every dogfood run we `cargo build --workspace` and `cargo test
--workspace` to validate the change. If either fails, the change is
reverted (`git restore`) and the prompt is tightened.

Tasks (in order):

1. `01-tui-mode.md`  — replace the line-based interactive REPL with a
   raw-mode TUI built on `pi_tui::DiffRenderer` + `crossterm` + the
   message queue + `/help` overlay.
2. `02-pickers.md`   — wire the `picker` module into `/resume`, `/model`,
   `/tree`, `/fork`, `/clone` so each opens an inline fuzzy picker.
3. `03-tests.md`     — add tests for every module that doesn't already
   have them, targeting >90% line coverage on deterministic code.
4. `04-share.md`     — `/share` uploads a markdown rendering of the
   current branch as a GitHub gist via `gh api`.
5. `05-scoped.md`    — `/scoped-models` toggles the per-message model
   picker.
