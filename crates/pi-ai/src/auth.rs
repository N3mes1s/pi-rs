use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// How to authenticate against a provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthMethod {
    /// API key sent as a header (Authorization or x-api-key).
    ApiKey { value: String },
    /// OAuth token from a subscription login flow (Claude Pro/Max,
    /// ChatGPT Plus/Pro, GitHub Copilot, Gemini CLI, Antigravity).
    OAuth {
        access_token: String,
        refresh_token: Option<String>,
        expires_at: Option<i64>,
    },
    /// No auth required (local LLM / mock).
    None,
}

/// Persistent storage of credentials per provider.
#[derive(Debug, Default, Clone)]
pub struct AuthStorage {
    inner: Arc<Mutex<AuthData>>,
    path: Option<PathBuf>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct AuthData {
    providers: HashMap<String, AuthMethod>,
}

impl AuthStorage {
    pub fn in_memory() -> Self {
        Self::default()
    }

    pub fn open(path: PathBuf) -> std::io::Result<Self> {
        let data = if path.exists() {
            let txt = std::fs::read_to_string(&path)?;
            serde_json::from_str(&txt).unwrap_or_default()
        } else {
            AuthData::default()
        };
        Ok(Self {
            inner: Arc::new(Mutex::new(data)),
            path: Some(path),
        })
    }

    pub const ENV_KEYS: &'static [(&'static str, &'static str)] = &[
        ("anthropic", "ANTHROPIC_API_KEY"),
        ("openai", "OPENAI_API_KEY"),
        ("fireworks", "FIREWORKS_API_KEY"),
        ("google", "GOOGLE_API_KEY"),
    ];

    pub fn from_env() -> Self {
        let mut data = AuthData::default();
        for (provider, env) in Self::ENV_KEYS {
            if let Ok(val) = std::env::var(env) {
                if !val.is_empty() {
                    data.providers
                        .insert((*provider).to_string(), AuthMethod::ApiKey { value: val });
                }
            }
        }
        Self {
            inner: Arc::new(Mutex::new(data)),
            path: None,
        }
    }

    pub fn provider_names(&self) -> Vec<String> {
        self.inner
            .lock()
            .map(|g| g.providers.keys().cloned().collect())
            .unwrap_or_default()
    }

    pub fn get(&self, provider: &str) -> Option<AuthMethod> {
        self.inner.lock().ok()?.providers.get(provider).cloned()
    }

    pub fn set(&self, provider: &str, method: AuthMethod) {
        if let Ok(mut g) = self.inner.lock() {
            g.providers.insert(provider.to_string(), method);
            if let Some(path) = &self.path {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Ok(txt) = serde_json::to_string_pretty(&*g) {
                    let _ = std::fs::write(path, txt);
                }
            }
        }
    }

    pub fn remove(&self, provider: &str) {
        if let Ok(mut g) = self.inner.lock() {
            g.providers.remove(provider);
            if let Some(path) = &self.path {
                if let Ok(txt) = serde_json::to_string_pretty(&*g) {
                    let _ = std::fs::write(path, txt);
                }
            }
        }
    }
}
