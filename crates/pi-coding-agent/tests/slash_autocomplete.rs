//! Slash-command autocomplete tests.
//!
//! The autocomplete tests are implemented as unit tests inside
//! `pi_coding_agent::modes::interactive` because the `build_frame`
//! function is `pub(crate)` (it uses view internals that should not
//! be part of the public API).
//!
//! Run them with:
//!   cargo test -p pi-coding-agent --lib build_frame_slash_autocomplete
//!
//! Tests:
//! - `build_frame_slash_autocomplete_shows_matching_commands`
//! - `build_frame_slash_autocomplete_empty_when_no_matches`
//! - `build_frame_slash_autocomplete_highlights_first_match`
//! - `build_frame_slash_autocomplete_hides_when_editor_empty`
//! - `build_frame_slash_autocomplete_limits_to_five`
//!
//! All are in `crates/pi-coding-agent/src/modes/interactive.rs`
//! under `mod tests`.

// This file intentionally has no tests at the top level.
// See the documentation above for how to run the autocomplete tests.
