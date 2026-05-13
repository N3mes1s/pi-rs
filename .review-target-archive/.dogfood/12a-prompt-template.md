You are working inside the pi-rs Cargo workspace at /home/user/Playground/pi-rs.

Goal: honour the existing `--prompt-template <name|@path>` CLI flag.

Background:
- `Cli::prompt_template: Option<String>` is parsed in
  `crates/pi-coding-agent/src/cli.rs` but no code reads it yet.
- `crates/pi-coding-agent/src/prompts.rs` exposes `PromptRegistry`
  with `render(vars)` substituting `{{var}}`.

Changes:

1. Add a free helper in `prompts.rs`:

       pub fn resolve(
           spec: &str,
           registry: &PromptRegistry,
           args: &str,
       ) -> Result<String, String>

   Behaviour:
   - if `spec` starts with `@`, treat the rest as a filesystem
     path. Read it via `std::fs::read_to_string`. On error return
     `Err`. Then run the same `{{args}}` / `{{ARGS}}` substitution
     as a registered template would.
   - otherwise look up `spec` in `registry`. If found, render with
     `args`. If not found, return `Err(format!("template not found:
     {spec}"))`.

2. In `crates/pi-coding-agent/src/modes/print.rs` and
   `crates/pi-coding-agent/src/modes/json.rs`, before reading
   stdin / building the prompt, check
   `startup.cli.prompt_template`. If `Some(spec)`, call
   `prompts::resolve(&spec, &startup.prompts, &joined_args)` where
   `joined_args` is the positional args joined with spaces. The
   resolved string takes precedence over positional prompt + stdin
   (use it as the single user prompt).

3. In `crates/pi-coding-agent/src/modes/interactive.rs`'s
   line-based and TUI entry points: if
   `startup.cli.prompt_template` is `Some`, pre-fill the editor
   with the resolved string instead of an empty buffer.

4. Tests in `crates/pi-coding-agent/tests/prompt_template_resolve.rs`:
   - `@` path reads the file and substitutes `{{args}}`
   - bare name resolves through PromptRegistry
   - `{{args}}` and `{{ARGS}}` both substitute
   - missing template returns Err with the spec name in the message
   - missing file (`@/nonexistent`) returns Err

Build clean: `cargo build --workspace`
Tests green: `cargo test -p pi-coding-agent --test prompt_template_resolve`

When done output: DONE.
