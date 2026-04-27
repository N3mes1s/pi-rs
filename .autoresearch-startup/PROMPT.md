You are running an **autoresearch loop** on the pi-rs Rust workspace
at /home/user/Playground/pi-rs.

The session is configured under
`/home/user/Playground/pi-rs/.autoresearch-startup/`:
- `autoresearch.config.json` — name, metric, direction (lower=better)
- `autoresearch.sh` — benchmark script. Outputs `METRIC startup_us=<value>`
  measuring 200 runs of `./target/release/pi --list` (a no-network
  startup path). Internally rebuilds release.
- `autoresearch.checks.sh` — sanity check (must build).
- `autoresearch.md` — objectives + scope + ideas.

You have these tools:
- `init_experiment`, `run_experiment`, `log_experiment` (autoresearch native)
- `read`, `write`, `edit`, `bash`, `grep`, `find`, `ls` (built-in)

**Workflow:**

1. Call `init_experiment` ONCE with name="pi-rs-startup",
   metric="startup_us", unit="µs", direction="lower". Pass
   `working_dir`: "/home/user/Playground/pi-rs/.autoresearch-startup".

2. Run a baseline: `run_experiment` with `command: "bash
   /home/user/Playground/pi-rs/.autoresearch-startup/autoresearch.sh"`,
   `idea: "baseline"`. Note the metric value.

3. **Loop** for up to **5 experiments**:
   a. Pick ONE idea from `autoresearch.md`'s "Ideas to try" list
      (or invent one based on your reading of the code).
   b. Make the smallest possible code change with `edit` or `write`.
      DO NOT change behaviour — only make startup faster.
   c. `run_experiment` again with the new idea description.
   d. If the metric improved by ≥ 50µs (≥ 2% of baseline),
      `log_experiment` with `kept: true`. Otherwise `kept: false`
      (which reverts via git).
   e. Continue.

4. After 5 experiments OR if you've exhausted ideas, summarise:
   - baseline µs
   - best µs achieved
   - which ideas worked
   - which ideas regressed
   - one paragraph of what you'd try next

**Constraints:**
- Behaviour MUST be preserved — every existing test still passes.
- Never break the release build (autoresearch.sh would fail).
- Each experiment changes ≤ ~30 lines of code unless you can
  justify more.
- Stay focused: this is a startup-time exercise, not a refactor.

Stop after 5 experiments and output: SUMMARY: <bulleted findings>.
