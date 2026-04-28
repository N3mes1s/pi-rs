//! Tests for the per-provider OAuth endpoint constructors and the
//! `endpoints_for_provider` helper.

use pi_ai::oauth::{build_authorize_url, endpoints_for_provider, OAuthEndpoints, Pkce};

// ── helper ────────────────────────────────────────────────────────────────────

/// Percent-encode every non-alphanumeric byte, matching the encoder used by
/// `build_authorize_url` (NON_ALPHANUMERIC set from the `percent-encoding`
/// crate). Used to build expected query-string fragments.
fn enc(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_string()
            } else {
                // NON_ALPHANUMERIC encodes everything that isn't A-Z a-z 0-9.
                format!("%{:02X}", c as u8)
            }
        })
        .collect()
}

// ── per-provider builder tests ────────────────────────────────────────────────

#[test]
fn anthropic_endpoints_values() {
    let ep = OAuthEndpoints::anthropic();
    assert_eq!(ep.authorize_url, "https://claude.ai/oauth/authorize");
    assert_eq!(ep.token_url, "https://console.anthropic.com/v1/oauth/token");
    assert_eq!(ep.client_id, "9d1c250a-e61b-44d9-88ed-5944d1962f5e");
    assert_eq!(ep.redirect_uri, "http://localhost:54545/callback");
    assert_eq!(ep.scope, "org:create_api_key user:profile user:inference");
}

#[test]
fn openai_subscription_endpoints_values() {
    let ep = OAuthEndpoints::openai_subscription();
    assert_eq!(ep.authorize_url, "https://auth.openai.com/oauth/authorize");
    assert_eq!(ep.token_url, "https://auth.openai.com/oauth/token");
    assert_eq!(ep.client_id, "app_eYqaQy3Gj4Sc9XUSfL2bWWxn");
    assert_eq!(ep.redirect_uri, "http://localhost:54545/callback");
    assert_eq!(ep.scope, "openid profile email offline_access");
}

#[test]
fn github_copilot_endpoints_values() {
    let ep = OAuthEndpoints::github_copilot();
    assert_eq!(ep.authorize_url, "https://github.com/login/oauth/authorize");
    assert_eq!(ep.token_url, "https://github.com/login/oauth/access_token");
    assert_eq!(ep.client_id, "Iv1.b507a08c87ecfe98");
    assert_eq!(ep.redirect_uri, "http://localhost:54545/callback");
    assert_eq!(ep.scope, "copilot read:user");
}

#[test]
fn gemini_cli_endpoints_values() {
    let ep = OAuthEndpoints::gemini_cli();
    assert_eq!(
        ep.authorize_url,
        "https://accounts.google.com/o/oauth2/v2/auth"
    );
    assert_eq!(ep.token_url, "https://oauth2.googleapis.com/token");
    assert_eq!(
        ep.client_id,
        "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com"
    );
    assert_eq!(ep.redirect_uri, "http://localhost:54545/callback");
    assert_eq!(
        ep.scope,
        "openid email profile https://www.googleapis.com/auth/cloud-platform"
    );
}

#[test]
fn antigravity_endpoints_values() {
    let ep = OAuthEndpoints::antigravity();
    assert_eq!(
        ep.authorize_url,
        "https://accounts.google.com/o/oauth2/v2/auth"
    );
    assert_eq!(ep.token_url, "https://oauth2.googleapis.com/token");
    assert_eq!(ep.client_id, "32555940559.apps.googleusercontent.com");
    assert_eq!(ep.redirect_uri, "http://localhost:54545/callback");
    assert_eq!(
        ep.scope,
        "openid email profile https://www.googleapis.com/auth/cloud-platform"
    );
}

// ── endpoints_for_provider ────────────────────────────────────────────────────

#[test]
fn endpoints_for_provider_anthropic_alias() {
    let ep = endpoints_for_provider("anthropic").expect("anthropic");
    assert_eq!(ep.authorize_url, OAuthEndpoints::anthropic().authorize_url);
    assert_eq!(ep.client_id, OAuthEndpoints::anthropic().client_id);
}

#[test]
fn endpoints_for_provider_claude_alias_returns_anthropic() {
    let via_claude = endpoints_for_provider("claude").expect("claude");
    let direct = OAuthEndpoints::anthropic();
    assert_eq!(via_claude.authorize_url, direct.authorize_url);
    assert_eq!(via_claude.token_url, direct.token_url);
    assert_eq!(via_claude.client_id, direct.client_id);
    assert_eq!(via_claude.scope, direct.scope);
}

#[test]
fn endpoints_for_provider_chatgpt_alias_returns_openai() {
    let via_chatgpt = endpoints_for_provider("chatgpt").expect("chatgpt");
    let direct = OAuthEndpoints::openai_subscription();
    assert_eq!(via_chatgpt.authorize_url, direct.authorize_url);
    assert_eq!(via_chatgpt.token_url, direct.token_url);
    assert_eq!(via_chatgpt.client_id, direct.client_id);
    assert_eq!(via_chatgpt.scope, direct.scope);
}

#[test]
fn endpoints_for_provider_openai_alias() {
    let ep = endpoints_for_provider("openai").expect("openai");
    assert_eq!(
        ep.client_id,
        OAuthEndpoints::openai_subscription().client_id
    );
}

#[test]
fn endpoints_for_provider_copilot_alias() {
    let ep = endpoints_for_provider("copilot").expect("copilot");
    assert_eq!(ep.client_id, OAuthEndpoints::github_copilot().client_id);
}

#[test]
fn endpoints_for_provider_github_alias() {
    let ep = endpoints_for_provider("github").expect("github");
    assert_eq!(ep.client_id, OAuthEndpoints::github_copilot().client_id);
}

#[test]
fn endpoints_for_provider_gemini() {
    let ep = endpoints_for_provider("gemini").expect("gemini");
    assert_eq!(ep.client_id, OAuthEndpoints::gemini_cli().client_id);
}

#[test]
fn endpoints_for_provider_antigravity() {
    let ep = endpoints_for_provider("antigravity").expect("antigravity");
    assert_eq!(ep.client_id, OAuthEndpoints::antigravity().client_id);
}

#[test]
fn endpoints_for_provider_nonexistent_is_none() {
    assert!(endpoints_for_provider("nonexistent").is_none());
    assert!(endpoints_for_provider("").is_none());
    assert!(endpoints_for_provider("ANTHROPIC").is_none()); // case-sensitive
}

// ── build_authorize_url for each provider ─────────────────────────────────────

fn check_authorize_url(ep: &OAuthEndpoints) {
    let pkce = Pkce::from_bytes(&[42u8; 32]);
    let url = build_authorize_url(ep, &pkce, "teststate");

    // Base authorize URL appears before the `?`
    assert!(
        url.starts_with(&ep.authorize_url),
        "URL should start with authorize_url. got: {url}"
    );

    // client_id is present and encoded
    assert!(
        url.contains(&format!("client_id={}", enc(&ep.client_id))),
        "missing client_id in: {url}"
    );

    // scope is present and encoded
    assert!(
        url.contains(&format!("scope={}", enc(&ep.scope))),
        "missing scope in: {url}"
    );

    // standard PKCE params
    assert!(url.contains("response_type=code"), "missing response_type");
    assert!(url.contains("code_challenge_method=S256"), "missing ccm");
    assert!(
        url.contains(&format!("code_challenge={}", enc(&pkce.challenge))),
        "missing code_challenge"
    );

    // state is present
    assert!(url.contains("state=teststate"), "missing state in: {url}");
}

#[test]
fn build_authorize_url_anthropic_contains_correct_base_and_client_id() {
    check_authorize_url(&OAuthEndpoints::anthropic());
}

#[test]
fn build_authorize_url_openai_contains_correct_base_and_client_id() {
    check_authorize_url(&OAuthEndpoints::openai_subscription());
}

#[test]
fn build_authorize_url_github_copilot_contains_correct_base_and_client_id() {
    check_authorize_url(&OAuthEndpoints::github_copilot());
}

#[test]
fn build_authorize_url_gemini_cli_contains_correct_base_and_client_id() {
    check_authorize_url(&OAuthEndpoints::gemini_cli());
}

#[test]
fn build_authorize_url_antigravity_contains_correct_base_and_client_id() {
    check_authorize_url(&OAuthEndpoints::antigravity());
}
