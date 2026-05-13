# RFD 0018 — OpenAI device-flow login (and what it can / can't do)

- **Status:** Discussion
- **Author:** pi-rs maintainers
- **Created:** 2026-04-28
- **Implemented:** &lt;pending&gt;

## Summary

Pi-rs already has the **scaffolding** for an OAuth login flow against
OpenAI: `OAuthEndpoints::openai_subscription()` in
`crates/pi-ai/src/oauth.rs:40` configures the
`auth.openai.com/oauth/{authorize,token}` endpoints, the PKCE state
struct, and a `TokenResponse` parser. There's even a `/login` slash
command in the TUI. **What's missing is a top-level CLI surface
(`pi --login openai`) and — more importantly — a clear-eyed
acknowledgement that an OpenAI subscription token is NOT a substitute
for an API key**: the OAuth scope pi-rs requests is OIDC
(`openid profile email offline_access`), which gets a profile token
for the ChatGPT website but NOT API access to
`/v1/chat/completions`. Anthropic's equivalent (`org:create_api_key
user:profile user:inference`) DOES grant API access — that's the
asymmetry that makes Claude Code's "use your Claude Pro for API
calls" feature work and an OpenAI mirror impossible today.

This RFD does two things:

1. Wires the missing `pi --login <provider>` CLI plumbing on top of
   the existing OAuth machinery, so the `openai` provider becomes
   *invokable* (and similarly fixes the gap for any other provider
   that lands an oauth endpoint set in the future).
2. Documents — in code comments and the RFD itself — exactly what
   you do and don't get with each provider's OAuth flow, so future
   readers don't repeat the "I'll just plug in a sub login" hope.

## Background

Inventory of the existing OAuth surface (all in
`crates/pi-ai/src/oauth.rs`):

```rust
pub struct OAuthEndpoints {
    pub authorize_url: String,
    pub token_url:     String,
    pub client_id:     String,
    pub redirect_uri:  String,
    pub scope:         String,
}

impl OAuthEndpoints {
    pub fn anthropic_subscription() -> Self { /* org:create_api_key + user:inference — REAL API ACCESS */ }
    pub fn openai_subscription()    -> Self { /* openid profile email offline_access — WEB SESSION ONLY */ }
    pub fn github_copilot()         -> Self { /* copilot read:user — separate `vscode-copilot/v1/chat` API */ }
}

pub fn endpoints_for_provider(name: &str) -> Option<OAuthEndpoints> {
    match name {
        "anthropic" | "claude"   => Some(OAuthEndpoints::anthropic_subscription()),
        "openai"    | "chatgpt"  => Some(OAuthEndpoints::openai_subscription()),
        "github"    | "copilot"  => Some(OAuthEndpoints::github_copilot()),
        _ => None,
    }
}

pub struct Pkce { pub verifier: String, pub challenge: String }
pub fn build_authorize_url(ep: &OAuthEndpoints, pkce: &Pkce, state: &str) -> String;
pub struct TokenResponse { pub access_token: String, pub refresh_token: Option<String>, pub expires_at: Option<i64> }
pub fn is_expired(method: &AuthMethod) -> bool;
```

What's _not_ wired:

* `pi --login <provider>` CLI flag (only the TUI `/login` slash
  command exists; not callable from print/json/rpc modes).
* The local-callback HTTP server that catches the redirect on
  `http://localhost:54545/callback` (the redirect URI is hard-coded
  in `OAuthEndpoints` but no actual listener exists in pi-rs today —
  TUI `/login` opens a browser then asks the user to paste the
  resulting code manually).
* A device-flow alternative (RFC 8628) for headless environments.
  OpenAI publishes a device-code endpoint at
  `https://auth.openai.com/oauth/device/code` (used by the official
  CLI / ChatGPT Code Assistant); pi-rs doesn't dispatch to it.
* Any documentation that the OpenAI scope above grants only profile
  info, not API call rights.

References:
* OAuth 2.0 Device Authorization Grant (RFC 8628) — the right shape
  for headless terminal apps.
* Anthropic OAuth scopes for Claude Code: `org:create_api_key
  user:profile user:inference` — the `user:inference` is what makes
  the token work as a bearer for `/v1/messages`.
* OpenAI's token endpoint replies: `id_token` + `access_token` for
  the OIDC scope; the `access_token` is good against
  `https://chat.openai.com/api/auth/session` but **not**
  `https://api.openai.com/v1/chat/completions`. The latter requires
  an `sk-...` API key issued through the developer dashboard.

## Proposal

### 1. Top-level `pi --login <provider>` CLI

```rust
// crates/pi-coding-agent/src/cli.rs
/// Authenticate via OAuth (browser or device flow). Provider is one of
/// `anthropic`, `openai`, `github`. Falls back to device flow when
/// `--device` is set or we detect we're in a headless terminal.
#[arg(long = "login", value_name = "PROVIDER")]
pub login: Option<String>,

/// Force the device-code flow even when a browser is available.
#[arg(long = "device", action = ArgAction::SetTrue, requires = "login")]
pub device: bool,
```

When `cli.login.is_some()`, the agent loop is short-circuited the
same way `pi --stats` does it.

### 2. New module `crates/pi-coding-agent/src/native/login.rs`

Two strategies:

```rust
pub async fn login_browser(provider: &str) -> Result<TokenResponse, LoginError> {
    let ep = endpoints_for_provider(provider).ok_or(LoginError::UnknownProvider)?;
    let pkce = Pkce::generate();
    let state = generate_state();
    let url = build_authorize_url(&ep, &pkce, &state);

    // Spin up a one-shot HTTP listener on 127.0.0.1:54545 (matches
    // the redirect_uri in OAuthEndpoints). The listener accepts
    // exactly one request, validates `state`, then exchanges the
    // returned `code` for a TokenResponse via `ep.token_url`.
    let listener = oneshot_callback_listener("127.0.0.1:54545")?;
    println!("Open this URL to log in:\n  {}", url);
    let_open::open_browser(&url).ok();
    let code = listener.await?;
    let tokens = exchange_code(&ep, &pkce, &code).await?;
    Ok(tokens)
}

pub async fn login_device(provider: &str) -> Result<TokenResponse, LoginError> {
    // RFC 8628 device-code flow. Useful for headless / SSH sessions
    // where no browser is available. Anthropic supports this for
    // Claude Code; OpenAI's device endpoint is at
    // https://auth.openai.com/oauth/device/code .
    let ep = device_endpoints_for_provider(provider).ok_or(LoginError::DeviceFlowUnsupported)?;
    let req = post_device_code(&ep).await?;
    println!("Enter code {} at {}", req.user_code, req.verification_uri);
    let tokens = poll_for_token(&ep, &req).await?;
    Ok(tokens)
}
```

`oneshot_callback_listener` is a small TCP listener using the
existing `tokio` runtime — no new deps.

### 3. Persist tokens via the existing `AuthStorage`

Pi-rs already has `pi_ai::AuthStorage::open(auth_path()) ⇒
~/.pi/agent/auth.json` and an `AuthMethod::OAuth { access_token,
refresh_token, expires_at }` variant. Login simply calls:

```rust
auth.set(provider, AuthMethod::OAuth {
    access_token:  tokens.access_token,
    refresh_token: tokens.refresh_token,
    expires_at:    tokens.expires_at,
});
```

Refresh-on-401 is a separate concern (RFD 0019 candidate) and out of
scope here.

### 4. Document the scope reality

The biggest deliverable is a comment block at the top of
`oauth.rs::openai_subscription` (and a section in this RFD)
spelling out what users do / don't get:

```rust
/// OAuth endpoints for an OpenAI subscription (ChatGPT) login.
///
/// **IMPORTANT — what this token does NOT do:** The scope below
/// (`openid profile email offline_access`) returns an OIDC profile
/// token usable against `chat.openai.com`'s session endpoints. It is
/// **NOT** a bearer token for `https://api.openai.com/v1/...`. OpenAI
/// does not currently expose a "use my ChatGPT Plus / Team / Edu
/// subscription as an API key" scope to third parties.
///
/// Use cases this DOES enable:
///   * Identity surface: "logged in as alice@example.com" UX in the TUI.
///   * Future: if OpenAI ships a sub→API scope, swap this string.
///
/// For developer-key access, set `OPENAI_API_KEY` directly. See
/// `pi --login openai --help` for the full picture.
///
/// Anthropic's analogous flow returns a token that DOES work as an
/// API bearer (scope includes `user:inference`).
pub fn openai_subscription() -> Self { ... }
```

A parallel comment goes on `anthropic_subscription` calling out that
the `user:inference` scope is the special sauce.

### 5. Surface the warning at login time

When `login_browser` / `login_device` finishes for `provider ==
"openai"`, print:

```
⚠  Logged in as alice@example.com.
  This OpenAI OAuth token grants profile + session access ONLY —
  it cannot be used against api.openai.com/v1/chat/completions.
  For API calls, set OPENAI_API_KEY (sk-proj-...) instead.
  See `pi --login openai --help` and RFD 0018.
```

The exact text lives in `login.rs::print_post_login_notes()`.

## Test plan

1. **`tests/login_url_round_trip.rs`** — `build_authorize_url` for
   `provider = "openai"` produces a URL whose `client_id`, `scope`,
   `state`, `code_challenge`, and `redirect_uri` match what we set
   on `OAuthEndpoints::openai_subscription` + the supplied PKCE.
   (Pure: no network.)
2. **`tests/login_browser_oneshot_callback.rs`** — start the
   listener on a free port, hit it with a hand-crafted GET like
   `http://127.0.0.1:PORT/callback?state=X&code=Y`, assert the
   future resolves with `code = "Y"`. Use `tokio::test`.
3. **`tests/login_device_flow_smoke.rs`** — uses a wiremock server
   to stand in for the device endpoint. Asserts pi-rs's poll loop
   honours the `interval` field in the device-code response and
   stops on `authorization_pending → success`. Skip the actual
   provider — wiremock is enough.
4. **Doc test on `openai_subscription`** — assert the docstring
   contains the literal "NOT a bearer token" so future edits can't
   silently lose the warning.
5. **End-to-end manual** — `pi --login openai` opens a browser,
   accepts the callback, prints the warning text, persists the
   token to `~/.pi/agent/auth.json`. Skip when `DISPLAY` is unset.

## Out of scope

- **Token refresh on 401** — when the bearer expires mid-session.
  RFD 0019 candidate; today's flow re-logs in.
- **Multi-account switching** — pi-rs's `AuthStorage` is keyed by
  provider, not (provider, account). Punt to RFD 0020.
- **Adding a real OpenAI API-access scope** — this would require
  OpenAI to ship one. Not possible from our side.
- **GitHub Copilot dispatch** — `OAuthEndpoints::github_copilot()`
  exists but pi-rs has no `ProviderKind::Copilot`. Separate RFD.
- **Refresh persistence** — when we *do* get a `refresh_token`,
  storing the rotation logic. Not needed for Anthropic (long-lived
  inference tokens) but will matter for Copilot if we wire it.

## Open questions

- **Should the warning text on OpenAI login be a hard error (refuse
  to persist the token) or a soft notice?** Lean soft — the token
  is still useful for "who am I logged in as" UX even if not for
  inference.
- **Should `pi --login` default to device flow when the binary
  detects a headless environment** (`DISPLAY` empty + stdin not a
  tty)? Lean yes — the failure mode otherwise is "browser never
  opens, callback never arrives, hang."
- **Do we need a `pi --logout <provider>` companion?** Yes —
  trivial; one-line `auth.remove(provider)`. Add as part of v1.
