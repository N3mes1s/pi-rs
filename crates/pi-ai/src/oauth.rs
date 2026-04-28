//! OAuth 2.0 + PKCE helpers used by `/login`.
//!
//! pi.dev's coding-agent supports subscription logins (Claude Pro/Max,
//! ChatGPT Plus/Pro, Copilot, Gemini CLI, Antigravity). We don't ship the
//! provider-specific endpoints here (those rotate and require browser
//! interaction); instead we expose a generic, well-tested PKCE flow that
//! providers can wire their endpoint URLs into. The Anthropic flow is
//! pre-configured.

use base64::Engine;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::auth::AuthMethod;
use crate::AiError;

/// Per-provider OAuth endpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthEndpoints {
    pub authorize_url: String,
    pub token_url: String,
    pub client_id: String,
    pub redirect_uri: String,
    pub scope: String,
}

impl OAuthEndpoints {
    pub fn anthropic() -> Self {
        Self {
            authorize_url: "https://claude.ai/oauth/authorize".into(),
            token_url: "https://console.anthropic.com/v1/oauth/token".into(),
            client_id: "9d1c250a-e61b-44d9-88ed-5944d1962f5e".into(),
            redirect_uri: "http://localhost:54545/callback".into(),
            scope: "org:create_api_key user:profile user:inference".into(),
        }
    }

    /// ChatGPT Plus / Pro subscription OAuth endpoints.
    pub fn openai_subscription() -> Self {
        Self {
            authorize_url: "https://auth.openai.com/oauth/authorize".into(),
            token_url: "https://auth.openai.com/oauth/token".into(),
            client_id: "app_eYqaQy3Gj4Sc9XUSfL2bWWxn".into(),
            redirect_uri: "http://localhost:54545/callback".into(),
            scope: "openid profile email offline_access".into(),
        }
    }

    /// GitHub Copilot subscription OAuth endpoints.
    pub fn github_copilot() -> Self {
        Self {
            authorize_url: "https://github.com/login/oauth/authorize".into(),
            token_url: "https://github.com/login/oauth/access_token".into(),
            client_id: "Iv1.b507a08c87ecfe98".into(),
            redirect_uri: "http://localhost:54545/callback".into(),
            scope: "copilot read:user".into(),
        }
    }

    /// Gemini CLI subscription OAuth endpoints.
    pub fn gemini_cli() -> Self {
        Self {
            authorize_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token_url: "https://oauth2.googleapis.com/token".into(),
            client_id: "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com"
                .into(),
            redirect_uri: "http://localhost:54545/callback".into(),
            scope: "openid email profile https://www.googleapis.com/auth/cloud-platform".into(),
        }
    }

    /// Antigravity subscription OAuth endpoints.
    pub fn antigravity() -> Self {
        Self {
            authorize_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token_url: "https://oauth2.googleapis.com/token".into(),
            client_id: "32555940559.apps.googleusercontent.com".into(),
            redirect_uri: "http://localhost:54545/callback".into(),
            scope: "openid email profile https://www.googleapis.com/auth/cloud-platform".into(),
        }
    }
}

/// Return the pre-configured [`OAuthEndpoints`] for a provider name.
///
/// Supported names (case-sensitive):
/// - `"anthropic"` / `"claude"` → Anthropic (Claude Pro/Max)
/// - `"openai"` / `"chatgpt"` → OpenAI (ChatGPT Plus/Pro)
/// - `"copilot"` / `"github"` → GitHub Copilot
/// - `"gemini"` → Gemini CLI
/// - `"antigravity"` → Antigravity
///
/// Returns `None` for any unrecognised name.
pub fn endpoints_for_provider(name: &str) -> Option<OAuthEndpoints> {
    match name {
        "anthropic" | "claude" => Some(OAuthEndpoints::anthropic()),
        "openai" | "chatgpt" => Some(OAuthEndpoints::openai_subscription()),
        "copilot" | "github" => Some(OAuthEndpoints::github_copilot()),
        "gemini" => Some(OAuthEndpoints::gemini_cli()),
        "antigravity" => Some(OAuthEndpoints::antigravity()),
        _ => None,
    }
}

/// In-memory PKCE pair: verifier (kept private) and challenge (sent to
/// the authorize endpoint).
#[derive(Debug, Clone)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
    pub method: &'static str,
}

impl Pkce {
    pub fn new() -> Self {
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        Self::from_bytes(&bytes)
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        let mut h = Sha256::new();
        h.update(verifier.as_bytes());
        let digest = h.finalize();
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
        Self {
            verifier,
            challenge,
            method: "S256",
        }
    }
}

/// Build the URL to launch in the browser to begin the OAuth flow.
pub fn build_authorize_url(ep: &OAuthEndpoints, pkce: &Pkce, state: &str) -> String {
    use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
    let enc = |s: &str| utf8_percent_encode(s, NON_ALPHANUMERIC).to_string();
    format!(
        "{base}?response_type=code&client_id={cid}&redirect_uri={ru}&scope={sc}&code_challenge={cc}&code_challenge_method={ccm}&state={st}",
        base = ep.authorize_url,
        cid = enc(&ep.client_id),
        ru = enc(&ep.redirect_uri),
        sc = enc(&ep.scope),
        cc = enc(&pkce.challenge),
        ccm = pkce.method,
        st = enc(state),
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<i64>,
    #[serde(default)]
    pub token_type: Option<String>,
}

impl TokenResponse {
    pub fn into_auth_method(self) -> AuthMethod {
        let expires_at = self.expires_in.map(|s| chrono::Utc::now().timestamp() + s);
        AuthMethod::OAuth {
            access_token: self.access_token,
            refresh_token: self.refresh_token,
            expires_at,
        }
    }
}

/// Exchange the authorization code for a token.
pub async fn exchange_code(
    client: &reqwest::Client,
    ep: &OAuthEndpoints,
    pkce: &Pkce,
    code: &str,
) -> Result<TokenResponse, AiError> {
    let resp = client
        .post(&ep.token_url)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", ep.client_id.as_str()),
            ("code", code),
            ("redirect_uri", ep.redirect_uri.as_str()),
            ("code_verifier", pkce.verifier.as_str()),
        ])
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(AiError::Provider { status, body });
    }
    let body = resp.text().await?;
    Ok(serde_json::from_str(&body)?)
}

/// Refresh an expired access token using a stored refresh token.
pub async fn refresh(
    client: &reqwest::Client,
    ep: &OAuthEndpoints,
    refresh_token: &str,
) -> Result<TokenResponse, AiError> {
    let resp = client
        .post(&ep.token_url)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", ep.client_id.as_str()),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(AiError::Provider { status, body });
    }
    let body = resp.text().await?;
    Ok(serde_json::from_str(&body)?)
}

/// True if a stored OAuth method has expired (with 60s grace).
pub fn is_expired(method: &AuthMethod) -> bool {
    if let AuthMethod::OAuth {
        expires_at: Some(t),
        ..
    } = method
    {
        return chrono::Utc::now().timestamp() >= *t - 60;
    }
    false
}

/// Spawn a tiny one-shot HTTP listener for the redirect callback.
/// Returns the captured `code` (and `state`) on success.
pub async fn listen_for_callback(
    bind_addr: &str,
    expected_state: &str,
) -> std::io::Result<(String, String)> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    let listener = TcpListener::bind(bind_addr).await?;
    let (mut sock, _) = listener.accept().await?;
    let mut buf = vec![0u8; 4096];
    let n = sock.read(&mut buf).await?;
    let req = String::from_utf8_lossy(&buf[..n]).to_string();
    let line = req.lines().next().unwrap_or("");
    let path = line.split_whitespace().nth(1).unwrap_or("");
    let qs = path.split_once('?').map(|(_, q)| q).unwrap_or("");
    let mut code = String::new();
    let mut state = String::new();
    for kv in qs.split('&') {
        if let Some((k, v)) = kv.split_once('=') {
            let dv = percent_encoding::percent_decode_str(v)
                .decode_utf8_lossy()
                .to_string();
            match k {
                "code" => code = dv,
                "state" => state = dv,
                _ => {}
            }
        }
    }
    let body = if state == expected_state && !code.is_empty() {
        "<!doctype html><html><body><h2>pi-rs: login complete. You may close this window.</h2></body></html>"
    } else {
        "<!doctype html><html><body><h2>pi-rs: login failed (state mismatch).</h2></body></html>"
    };
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = sock.write_all(resp.as_bytes()).await;
    let _ = sock.shutdown().await;
    if state != expected_state || code.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "OAuth callback state mismatch or missing code",
        ));
    }
    Ok((code, state))
}
