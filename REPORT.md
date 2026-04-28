# REPORT — `/cost` slash command

**Feature shipped**: New `/cost` builtin in the pi-rs interactive TUI that runs `pi-stats` ingest then prints the cost rollup for the current cwd as an inline assistant note. Commit `faefb5a`.

**Tests**: 9 passing, 0 failing.
- `cargo test -p pi-coding-agent --test slash` → 5/5 (pre-existing, still green).
- `cargo test -p pi-coding-agent --test slash_cost` → 4/4 (new): builtin registration, `/cost` parsing, formatter happy path, formatter empty-folder path.

## What the new tooling did well
- `pi --worktree` style isolation: working in `/home/user/quartet/realtask` with its own `CARGO_TARGET_DIR` meant the build cache stayed local and didn't disturb the parent checkout.
- The `task` tool's `code-reviewer` subagent ran end-to-end against an inline `.pi/agents/code-reviewer.md` (model `claude-opus-4-7`, tools `[read, grep, find, bash]`) and returned a structured verdict in one shot — no transcript bleed-through.
- `pi-stats`' public surface (`ingest::default_db_path`, `aggregate::by_folder`) was already shaped exactly the way a slash handler wants to consume it; no plumbing changes needed.
- Mirroring an existing builtin (`compact`) made wiring both `handle_slash` and `handle_slash_line` mechanical.

## What was awkward
- The slash dispatcher is duplicated between `crates/pi-coding-agent/src/modes/interactive.rs::handle_slash` (TUI) and `handle_slash_line` (line mode). Every new builtin has to be added in two arms ~750 lines apart.
- `FolderStats::folder` is an exact-string match on whatever `Meta.cwd` recorded. There's no canonicalisation helper, so a user whose shell `cwd` ends up symlink-resolved differently from the recorded value will get "no recorded usage". The formatter can't tell.
- `pi_stats::ingest::sync_all` is sync-only, forcing a `spawn_blocking` shim from the async slash handler. A native async entry point (or at least an `async` wrapper in `pi_stats::cli`) would remove the boilerplate.
- No bundled `code-reviewer.md` — every repo has to ship its own. The skill template in `rfd/0005` helps, but a built-in default would have skipped a step.

## Suggested follow-up RFDs
- **Unify slash builtins**: extract the `handle_slash` / `handle_slash_line` match arms into a `BuiltinHandler` trait so each command lives in one file.
- **Canonicalise folder keys in pi-stats**: `std::fs::canonicalize` (or strip-trailing-slash) the cwd both at ingest and at query time so `/cost` doesn't silently miss.
- **Bundle a default `code-reviewer` agent** via `include_dir!` so `task` invocations work out-of-the-box on a fresh repo.
