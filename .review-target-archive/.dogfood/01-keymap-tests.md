You are working inside the pi-rs Cargo workspace at /home/user/Playground/pi-rs.

Task: add a unit test module to `crates/pi-coding-agent/src/keymap.rs` that
covers the `parse_chord` function. The tests should:

1. Verify that `parse_chord("Enter")` returns a Chord with no modifiers and
   `ChordCode::Enter`.
2. Verify that `parse_chord("Ctrl+L")` returns a Chord with the Ctrl
   modifier and `ChordCode::Char('l')`.
3. Verify that `parse_chord("Shift+Ctrl+P")` returns a Chord with both
   shift and ctrl modifiers and `ChordCode::Char('p')`.
4. Verify that `parse_chord("F5")` returns `ChordCode::F(5)`.
5. Verify that `parse_chord("nonsense")` returns None for an unknown key
   like `parse_chord("Ctrl+Bogus")`.

Add the tests inside a `#[cfg(test)] mod tests { ... }` block at the end
of the file. Use `#[test]` (not async) since these are pure functions.

Once the tests are written, run:

    cargo test -p pi-coding-agent keymap::tests

If any test fails, fix the implementation OR fix the test (whichever is
correct based on the actual behaviour) and re-run until all tests pass.
Do not modify any other file.

When done, output a single line: DONE.
