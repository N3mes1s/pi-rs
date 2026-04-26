You are working inside the pi-rs Cargo workspace at /home/user/Playground/pi-rs.

Goal: push line coverage past 90% on every TESTABLE module. Modules
that are pure IO/runtime glue and need a real TTY/network/process
boundary should be excluded from the coverage measurement, NOT
hand-tested. Specifically these are excluded:

- `crates/pi-coding-agent/src/bin/pi.rs`
- `crates/pi-coding-agent/src/modes/interactive.rs`
- `crates/pi-coding-agent/src/modes/print.rs`
- `crates/pi-coding-agent/src/modes/json.rs`
- `crates/pi-coding-agent/src/modes/rpc.rs`
- `crates/pi-coding-agent/src/startup.rs`
- `crates/pi-coding-agent/src/sdk.rs`
- `crates/pi-coding-agent/src/telemetry.rs`
- `crates/pi-ai/examples/probe.rs`
- `crates/pi-ai/src/provider/anthropic.rs` already has stream tests;
  the unhit lines are an unreachable error branch — leave it.
- `crates/pi-ai/src/provider/openai.rs` same.

Step 1: create `pi-rs/.config/nextest.toml` and a `pi-rs/llvm-cov.toml`
or, simpler, just commit the right CLI invocation in a shell script
`pi-rs/scripts/coverage.sh` that runs:

    cargo llvm-cov --workspace \
        --ignore-filename-regex '(modes/(interactive|print|json|rpc)\.rs|/bin/pi\.rs|startup\.rs|sdk\.rs|telemetry\.rs|examples/)' \
        --summary-only

and prints the summary. Make it executable. Add it to git.

Step 2: identify the modules currently below 90% line coverage by
running:

    cd /home/user/Playground/pi-rs && cargo llvm-cov --workspace \
        --ignore-filename-regex '(modes/(interactive|print|json|rpc)\.rs|/bin/pi\.rs|startup\.rs|sdk\.rs|telemetry\.rs|examples/)' \
        --summary-only 2>&1 | tail -40

For each module under 90%, add new tests in
`crates/<crate>/tests/<module>_extra.rs` (do NOT modify source) that
cover the missing branches. Re-run coverage until every listed
module is ≥ 90% lines.

Specific modules likely needing attention (verify with the report):
- `pi-tools/src/lib.rs` — exercise `truncate_for_model` via several
  sizes of input (small, exactly at limit, just over, much over)
- `pi-tools/src/{grep,find,bash,read,write,edit,ls}.rs` — already
  partly covered; add cases for: bash with explicit `cwd` param,
  read with offset and limit, write that creates nested parent
  dirs, edit with `replace_all = true` matching all occurrences,
  ls in a tempdir with mixed dirs and files (ensure the trailing
  `/` for dirs appears).
- `pi-coding-agent/src/cmd.rs` — exercise `run_install` with a
  bogus spec (assert it errors); `run_update` over an empty dir.
- `pi-coding-agent/src/skills.rs` — bare `name.md` without
  description, dir with no SKILL.md (skipped).
- `pi-coding-agent/src/extensions.rs` — `discover` returns empty
  for a non-existent root; manifest with no tools and no commands;
  invoke timeout path (sleep > timeout_ms).
- `pi-coding-agent/src/themes.rs` — invalid JSON theme file is
  ignored; `read_theme` on missing path returns None.
- `pi-coding-agent/src/picker.rs` — `with_limit(3)` then more than
  three items → only three returned; `selected_value` on empty
  picker → None.
- `pi-coding-agent/src/keymap.rs` — `Keymap::load_overrides` on
  missing path returns Err; on bad JSON returns an empty map
  (because the function unwraps_or_default).
- `pi-coding-agent/src/packages.rs` — `package_dirs` returns
  conventional dirs alongside manifest-declared ones.
- `pi-tui/src/renderer.rs` — render a frame with mixed plain +
  coloured spans into a `Vec<u8>` writer, assert SGR escape codes
  appear, and a second render with the same frame should write
  fewer bytes than the first (diff renderer skipping unchanged
  lines).

Step 3: at the end, run `bash scripts/coverage.sh 2>&1 | tail -50`
and report the new TOTAL.

When done, output: DONE.
