You are working inside the pi-rs Cargo workspace at /home/user/Playground/pi-rs.

Goal: extend the OAuth PKCE module with provider-specific
subscription endpoints (Claude Pro/Max, ChatGPT Plus, GitHub
Copilot, Gemini CLI, Antigravity). The existing
`pi_ai::oauth::OAuthEndpoints::anthropic()` already covers Claude
Pro/Max — keep that.

Step 1. Add to `crates/pi-ai/src/oauth.rs`:

    impl OAuthEndpoints {
        pub fn openai_subscription() -> Self { ... }
        pub fn github_copilot() -> Self { ... }
        pub fn gemini_cli() -> Self { ... }
        pub fn antigravity() -> Self { ... }
    }

Use these endpoint values (placeholders that match upstream
pi-mono's published constants — these may rotate; we ship them as
defaults):

- ChatGPT Plus/Pro:
    authorize: https://auth.openai.com/oauth/authorize
    token:     https://auth.openai.com/oauth/token
    client_id: app_eYqaQy3Gj4Sc9XUSfL2bWWxn
    redirect:  http://localhost:54545/callback
    scope:     "openid profile email offline_access"

- GitHub Copilot:
    authorize: https://github.com/login/oauth/authorize
    token:     https://github.com/login/oauth/access_token
    client_id: Iv1.b507a08c87ecfe98
    redirect:  http://localhost:54545/callback
    scope:     "copilot read:user"

- Gemini CLI:
    authorize: https://accounts.google.com/o/oauth2/v2/auth
    token:     https://oauth2.googleapis.com/token
    client_id: 681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com
    redirect:  http://localhost:54545/callback
    scope:     "openid email profile https://www.googleapis.com/auth/cloud-platform"

- Antigravity:
    authorize: https://accounts.google.com/o/oauth2/v2/auth
    token:     https://oauth2.googleapis.com/token
    client_id: 32555940559.apps.googleusercontent.com
    redirect:  http://localhost:54545/callback
    scope:     "openid email profile https://www.googleapis.com/auth/cloud-platform"

Step 2. Add a top-level helper:

    pub fn endpoints_for_provider(name: &str) -> Option<OAuthEndpoints>

That returns:
- "anthropic" / "claude" → anthropic()
- "openai" / "chatgpt" → openai_subscription()
- "copilot" / "github" → github_copilot()
- "gemini" → gemini_cli()
- "antigravity" → antigravity()
- otherwise None

Step 3. Update the TUI's `/login [provider]` slash command in
`crates/pi-coding-agent/src/modes/interactive.rs`. It currently
hard-codes Anthropic. Make it:
- if no arg, default to "anthropic"
- otherwise call `endpoints_for_provider(arg)` — if None, print a
  friendly message listing supported providers
- run the same PKCE flow; on success store via `auth.set(provider,
  AuthMethod::OAuth { ... })`.

Step 4. Tests in `crates/pi-ai/tests/oauth_endpoints.rs`:
- each per-provider builder returns the documented values
- `endpoints_for_provider("claude")` returns the Anthropic endpoint
- `endpoints_for_provider("chatgpt")` returns the OpenAI endpoint
- `endpoints_for_provider("nonexistent")` is None
- `build_authorize_url(&ep, &pkce, "state")` for each provider
  contains the correct authorize_url and client_id

Build clean: `cargo build --workspace`
Tests green: `cargo test -p pi-ai --test oauth_endpoints`

When done output: DONE.
