You are working inside the pi-rs Cargo workspace at /home/user/Playground/pi-rs.

Goal: implement the four slash commands that are still placeholders
in `crates/pi-coding-agent/src/modes/interactive.rs` (and any new
TUI mode if it now exists):

1. `/clone` — duplicate the current session's active branch into a
   new session. Use `pi_agent_core::SessionManager` and the existing
   `current_branch(session_id)` to read the entries, then
   `create(provider, model)` to make a new session, and append all
   entries with their original kinds (User, Assistant, ToolCall,
   ToolResult, Compaction). Return the new session's id, print it
   to the user. Add a method to `SessionManager` if needed:
       `pub fn clone_branch(&self, source_id: &str) -> std::io::Result<SessionMeta>`
   keep the change tight. Add unit tests in
   `crates/pi-agent-core/tests/session_clone.rs`.

2. `/scoped-models` — toggle a `bool` field
   `Settings.scoped_models: bool` (default false). When true, the
   interactive footer shows the active model in a different colour
   and the model can be changed for just the *next* user message
   via `Ctrl+L` then a model picker. Otherwise the change persists.
   Implement the storage in `Settings`, persist it in
   `~/.pi/agent/settings.json`. Add a `/scoped-models` slash
   handler that flips the bool and prints the new state.

3. `/share` — run `gh gist create` with the session exported as
   markdown. Use the existing `/export` machinery if you have it,
   otherwise build a small markdown render of the active branch:
       # session <id>
       _model: <provider>/<model>_
       ## user
       <text>
       ## assistant
       <text>
       ## tool: <name>
       <input>
       ## tool result
       <output>
   Pipe that to `bash -c "gh gist create -d 'pi-rs session' -"`.
   If gh is not installed, print a friendly message instead of
   crashing. Add a unit test that constructs a fake session and
   asserts the markdown renderer produces the right structure.

4. `/login` — drive the OAuth PKCE flow already in
   `pi_ai::oauth`. Build the authorize URL for Anthropic, print it
   so the user can open it, listen on 127.0.0.1:54545 for the
   callback, exchange the code, and store the token via
   `auth_storage.set("anthropic", AuthMethod::OAuth { ... })`.
   Print "logged in" on success, the error otherwise. Don't make
   real network calls in tests; just unit-test the URL builder
   produced by build_authorize_url with a known PKCE pair.

For all four:
- Add slash command entries in `slash::SlashRegistry::register_builtins`
  (they're already there as built-in names; just make sure the
  interactive.rs `match` arm calls the new logic instead of
  printing "[/X] not yet implemented in pi-rs").
- Tests go alongside, named `*_extra.rs` if a file already exists.

After writing, run:
    cargo test --workspace
and iterate until green. Output: DONE.
