//! Deterministic build_authorize_url test using a fixed PKCE seed.
//!
//! `/login` (in pi-coding-agent) drives the PKCE flow but never exposes a
//! browser. This test pins down the URL builder against a known PKCE input
//! so that any encoding regression shows up locally.

use pi_ai::oauth::{build_authorize_url, OAuthEndpoints, Pkce};

#[test]
fn login_authorize_url_is_deterministic_for_a_known_pkce_pair() {
    let pkce = Pkce::from_bytes(&[7u8; 32]);
    let ep = OAuthEndpoints {
        authorize_url: "https://example.test/auth".into(),
        token_url: "https://example.test/token".into(),
        client_id: "abc".into(),
        redirect_uri: "http://localhost:54545/callback".into(),
        scope: "a b c".into(),
    };
    let url = build_authorize_url(&ep, &pkce, "state-1");

    // Stable prefix.
    assert!(url.starts_with("https://example.test/auth?"));
    // The challenge for `[7u8; 32]` must not be empty.
    assert!(!pkce.challenge.is_empty());
    // The challenge appears in the URL (encoded).
    assert!(url.contains("code_challenge="));
    // PKCE method is pinned to S256.
    assert!(url.contains("code_challenge_method=S256"));
    // State round-trips (URL-encoded).
    assert!(url.contains("state=state%2D1"));
    // Two PKCE pairs from the same seed produce identical URLs.
    let pkce2 = Pkce::from_bytes(&[7u8; 32]);
    assert_eq!(pkce.verifier, pkce2.verifier);
    assert_eq!(pkce.challenge, pkce2.challenge);
    assert_eq!(url, build_authorize_url(&ep, &pkce2, "state-1"));
}
