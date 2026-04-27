You are working inside the pi-rs Cargo workspace at /home/user/Playground/pi-rs.

Goal: prettier picker labels for `/resume` and `/tree`.

Background:
- `crates/pi-coding-agent/src/picker.rs` exposes `PickItem<T>` with a
  free-form `label: String`.
- `/resume` and `/tree` currently feed raw session/entry IDs as
  labels. SessionMeta has `provider/model/title/created_at/updated_at`;
  SessionEntry has a `kind` (User/Assistant/ToolCall/...) and a
  short `text` field accessible via the message.

Changes:

1. Add to `crates/pi-coding-agent/src/picker.rs`:

       pub fn format_session_label(meta: &pi_agent_core::SessionMeta) -> String

   Returns the format:
       "<short_id>  <provider>/<model>  <YYYY-MM-DD HH:MM>  <title>"
   where:
   - short_id is the first 8 chars of meta.id
   - timestamp is formatted from `meta.updated_at` (millis since
     epoch) using `chrono` UTC `format("%Y-%m-%d %H:%M")`
   - title falls back to "(no title)" if None or empty

   And:

       pub fn format_tree_entry(entry: &pi_agent_core::SessionEntry) -> String

   Returns:
       "<kind>  <short_text>"
   where kind is one of "user", "assistant", "tool_call: <name>",
   "tool_result", "compaction", "meta", "system"; short_text is up
   to 60 chars of the message text (or "" for kinds without text).

2. Update the call sites in
   `crates/pi-coding-agent/src/modes/interactive.rs` that build the
   `Picker` for `/resume` and `/tree` to use these formatters.

3. Tests in `crates/pi-coding-agent/tests/picker_labels.rs`:
   - format_session_label: full case (title set), missing title,
     short_id is exactly 8 chars, timestamp is correctly formatted
     for a known epoch (e.g. updated_at: 1_700_000_000_000 → check
     the resulting "YYYY-MM-DD HH:MM" prefix matches what chrono
     emits for that timestamp).
   - format_tree_entry: each kind variant produces the right
     prefix; long messages are truncated to 60 chars; messages
     containing newlines have them replaced with spaces.

Build clean: `cargo build --workspace`
Tests green: `cargo test -p pi-coding-agent --test picker_labels`

When done output: DONE.
