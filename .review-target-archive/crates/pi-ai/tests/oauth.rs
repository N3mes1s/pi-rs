use pi_ai::auth::AuthMethod;
use pi_ai::oauth::{
    build_authorize_url, exchange_code, is_expired, OAuthEndpoints, Pkce, TokenResponse,
};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn pkce_from_bytes_is_deterministic() {
    let bytes = [7u8; 32];
    let a = Pkce::from_bytes(&bytes);
    let b = Pkce::from_bytes(&bytes);
    assert_eq!(a.verifier, b.verifier);
    assert_eq!(a.challenge, b.challenge);
    assert_eq!(a.method, "S256");
    // Differs for different input.
    let c = Pkce::from_bytes(&[8u8; 32]);
    assert_ne!(a.verifier, c.verifier);
}

#[test]
fn pkce_new_produces_43_byte_url_safe_verifier() {
    let p = Pkce::new();
    // 32 bytes base64-url-no-pad encodes to ceil(32 * 4/3) = 43 chars.
    assert_eq!(p.verifier.len(), 43);
    let url_safe = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    assert!(p.verifier.chars().all(url_safe));
    assert!(p.challenge.chars().all(url_safe));
    assert_eq!(p.method, "S256");
}

#[test]
fn build_authorize_url_includes_and_encodes_all_params() {
    let ep = OAuthEndpoints::anthropic();
    let pkce = Pkce::from_bytes(&[1u8; 32]);
    let url = build_authorize_url(&ep, &pkce, "state with spaces & symbols");

    // Starts at the configured authorize endpoint
    assert!(url.starts_with("https://claude.ai/oauth/authorize?"));

    // Required params present
    assert!(url.contains("response_type=code"));
    assert!(url.contains("code_challenge_method=S256"));

    // The encoder uses NON_ALPHANUMERIC, so any non-alphanumeric byte
    // (including `-` and `_`) is percent-encoded.
    let enc = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_string()
                } else {
                    format!("%{:02X}", c as u8)
                }
            })
            .collect()
    };

    assert!(url.contains(&format!("code_challenge={}", enc(&pkce.challenge))));
    assert!(url.contains(&format!("client_id={}", enc(&ep.client_id))));

    // redirect_uri encoded (`:` and `/` are non-alphanumeric).
    assert!(url.contains(&format!("redirect_uri={}", enc(&ep.redirect_uri))));

    // scope encoded — every non-alphanumeric byte (spaces, `:`, `_`).
    assert!(url.contains(&format!("scope={}", enc(&ep.scope))));

    // state percent-encoded (spaces, `&`).
    assert!(url.contains(&format!("state={}", enc("state with spaces & symbols"))));
}

#[test]
fn is_expired_logic() {
    let past = chrono::Utc::now().timestamp() - 1; // already past, includes 60s grace
    let future = chrono::Utc::now().timestamp() + 3600;

    let expired = AuthMethod::OAuth {
        access_token: "x".into(),
        refresh_token: None,
        expires_at: Some(past),
    };
    let fresh = AuthMethod::OAuth {
        access_token: "x".into(),
        refresh_token: None,
        expires_at: Some(future),
    };
    let no_exp = AuthMethod::OAuth {
        access_token: "x".into(),
        refresh_token: None,
        expires_at: None,
    };
    let api_key = AuthMethod::ApiKey { value: "k".into() };

    assert!(is_expired(&expired));
    assert!(!is_expired(&fresh));
    assert!(!is_expired(&no_exp));
    assert!(!is_expired(&api_key));
}

#[tokio::test]
async fn exchange_code_round_trip_against_mock() {
    let server = MockServer::start().await;
    let token_body = json!({
        "access_token": "atoken",
        "refresh_token": "rtoken",
        "expires_in": 3600,
        "token_type": "Bearer",
    });

    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&token_body))
        .mount(&server)
        .await;

    let ep = OAuthEndpoints {
        token_url: format!("{}/oauth/token", server.uri()),
        ..OAuthEndpoints::anthropic()
    };
    let pkce = Pkce::from_bytes(&[2u8; 32]);
    let client = reqwest::Client::new();

    let resp: TokenResponse = exchange_code(&client, &ep, &pkce, "the-code")
        .await
        .unwrap();
    assert_eq!(resp.access_token, "atoken");
    assert_eq!(resp.refresh_token.as_deref(), Some("rtoken"));
    assert_eq!(resp.expires_in, Some(3600));
    assert_eq!(resp.token_type.as_deref(), Some("Bearer"));

    // Ensure into_auth_method computes a future expires_at.
    let now = chrono::Utc::now().timestamp();
    let am = resp.into_auth_method();
    match am {
        AuthMethod::OAuth {
            access_token,
            refresh_token,
            expires_at,
        } => {
            assert_eq!(access_token, "atoken");
            assert_eq!(refresh_token.as_deref(), Some("rtoken"));
            let exp = expires_at.unwrap();
            assert!(exp >= now + 3500 && exp <= now + 3700);
        }
        _ => panic!("expected OAuth"),
    }
}
