You are working inside the pi-rs Cargo workspace at /home/user/Playground/pi-rs.

Goal: poll `HotThemes` from the TUI render loop so themes hot-reload
visibly without restarting pi-rs.

Background:
- `crates/pi-coding-agent/src/themes.rs` already exposes `HotThemes`
  with a `notify` watcher. The constructor `HotThemes::new(dirs)` and
  `snapshot()` are wired but the TUI doesn't currently use them.
- `crates/pi-coding-agent/src/modes/interactive.rs` `run_tui` reads
  `startup.themes` once and looks up the active theme by name.

Changes:

1. Add a `themes_handle: Option<crate::themes::HotThemes>` field to
   `Startup` (next to `themes`). In `startup::assemble`, construct it
   with the same dirs passed to `load_themes(...)`. Keep the
   one-shot `themes` registry too — it's the initial snapshot.

2. In `run_tui`, on every render tick (the existing 50 ms timer):
   - call `startup.themes_handle.as_ref().map(|h| h.snapshot())`
   - if Some, look up the active theme name in the new snapshot
   - if the resolved Theme's content differs from the cached one,
     mark the view dirty and use the new Theme for the next frame.

3. `Theme` should be cloneable + `PartialEq` so the diff check is
   trivial. If `pi_tui::Theme` doesn't already derive PartialEq, add
   it (and `PartialEq` for `ColorSpec`). Keep all Theme fields the
   same — just derives.

4. Tests in `crates/pi-coding-agent/tests/themes_hot.rs`:
   - construct a `HotThemes` over a tempdir
   - write a `mytheme.json` file with valid Theme JSON
   - poll `snapshot()` until it contains `"mytheme"` (timeout 1s)
   - rewrite the file with different colours
   - poll again and assert the new colours appear

   Use `std::time::Instant::now() + Duration::from_millis(1000)` as
   the deadline; `std::thread::sleep(Duration::from_millis(20))` between
   polls.

Build clean: `cargo build --workspace`
Tests green: `cargo test -p pi-coding-agent --test themes_hot`

When done output: DONE.
