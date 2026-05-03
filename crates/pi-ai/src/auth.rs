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
///
/// Per RFD 0027 §4.5 #8 (Hardening H5):
/// - On-disk persistence uses `0o600` perms + atomic temp + rename
///   on write so a partial write or umask-leak cannot expose
///   credentials to other users on the host.
/// - [`from_env`](Self::from_env) is **deprecated** at H5: it slurps
///   17 env vars unconditionally, a CWE-526 magnet for embedders that
///   inherit a parent process environment they don't fully trust. Use
///   [`from_env_explicit`](Self::from_env_explicit) and name the keys
///   you trust.
/// - [`scoped`](Self::scoped) creates a per-tenant view that filters
///   out other providers — multi-tenant embedders use this to deny
///   cross-tenant credential bleed in shared-runtime setups.
/// - [`sealed`](Self::sealed) makes [`set`](Self::set) panic after
///   construction, so an embedder that wants immutable creds
///   post-init can enforce it at runtime.
#[derive(Debug, Default, Clone)]
pub struct AuthStorage {
    inner: Arc<Mutex<AuthData>>,
    path: Option<PathBuf>,
    /// Allow-list of provider names this view exposes. `None` = all.
    /// Set by [`scoped`](Self::scoped). When `Some`, `get` and
    /// `provider_names` filter; `set`/`remove` reject providers
    /// outside the list.
    scope: Option<Arc<Vec<String>>>,
    /// When `true`, [`set`](Self::set) and [`remove`](Self::remove)
    /// panic. Set by [`sealed`](Self::sealed).
    sealed: bool,
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
            scope: None,
            sealed: false,
        })
    }

    pub const ENV_KEYS: &'static [(&'static str, &'static str)] = &[
        ("anthropic", "ANTHROPIC_API_KEY"),
        ("openai", "OPENAI_API_KEY"),
        ("fireworks", "FIREWORKS_API_KEY"),
        ("google", "GOOGLE_API_KEY"),
        ("bedrock", "AWS_BEDROCK_TOKEN"),
        ("azure-openai", "AZURE_OPENAI_API_KEY"),
        ("cerebras", "CEREBRAS_API_KEY"),
        ("groq", "GROQ_API_KEY"),
        ("xai", "XAI_API_KEY"),
        ("openrouter", "OPENROUTER_API_KEY"),
        ("deepseek", "DEEPSEEK_API_KEY"),
        ("mistral", "MISTRAL_API_KEY"),
        ("zai", "ZAI_API_KEY"),
        ("huggingface", "HF_TOKEN"),
        ("ollama", "OLLAMA_API_KEY"),
        ("kimi", "MOONSHOT_API_KEY"),
        ("minimax", "MINIMAX_API_KEY"),
    ];

    /// Slurp every env var in [`ENV_KEYS`](Self::ENV_KEYS).
    ///
    /// **DEPRECATED** at H5 (RFD 0027 §4.5 #8). Embedders should use
    /// [`from_env_explicit`](Self::from_env_explicit) and name the
    /// provider/env-var pairs they trust. `from_env()` is a CWE-526
    /// magnet for embedders that inherit a parent process environment
    /// they do not fully control.
    ///
    /// Construction emits a `tracing::warn!` so embedders running with
    /// a tracing subscriber see the deprecation in their logs. The
    /// function continues to work for back-compat through the SDK 0.x
    /// window; removal is in SDK 1.0+4 MINOR per the deprecation
    /// policy.
    #[deprecated(
        since = "0.1.0",
        note = "use `AuthStorage::from_env_explicit(&[(provider, env_key), ...])` instead — RFD 0027 §4.5 #8"
    )]
    pub fn from_env() -> Self {
        tracing::warn!(
            "AuthStorage::from_env() called — slurping {} env vars unconditionally; \
             prefer from_env_explicit(allowlist) for production (RFD 0027 §4.5 #8)",
            Self::ENV_KEYS.len()
        );
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
            scope: None,
            sealed: false,
        }
    }

    /// Per RFD 0027 §4.5 #8 (Hardening H5): opt-in env scanning.
    /// Embedder explicitly names the provider → env-var pairs that
    /// should be looked up. Missing or empty env vars are silently
    /// skipped (no error); duplicate `provider` entries are
    /// last-write-wins.
    ///
    /// Returns an `AuthStorage` ready for use; never persists to disk
    /// (path = None). To persist, follow with `.with_path(path)` once
    /// that helper is added — for SDK 0.1 the in-memory path is the
    /// only blessed option.
    ///
    /// Returns `Ok` even if no env vars matched — embedders may want
    /// to start empty and `set` later.
    ///
    /// **Asymmetry note vs. deprecated `from_env()`:** non-UTF8 env
    /// vars surface here as `Err(VarError::NotUnicode(_))` — the
    /// caller decides how to handle. The deprecated `from_env()`
    /// silently skips non-UTF8 vars (uses `if let Ok(...)`). Embedders
    /// migrating from `from_env()` may discover non-UTF8 env vars on
    /// hosts where they previously went unnoticed.
    pub fn from_env_explicit(
        allowlist: &[(&str, &str)],
    ) -> Result<Self, std::env::VarError> {
        let mut data = AuthData::default();
        for (provider, env) in allowlist {
            match std::env::var(env) {
                Ok(val) if !val.is_empty() => {
                    data.providers.insert(
                        (*provider).to_string(),
                        AuthMethod::ApiKey { value: val },
                    );
                }
                // Missing or empty: skip silently (caller may set later).
                Ok(_) | Err(std::env::VarError::NotPresent) => {}
                // Non-UTF8 env var: bubble up so the caller sees it.
                Err(e @ std::env::VarError::NotUnicode(_)) => return Err(e),
            }
        }
        Ok(Self {
            inner: Arc::new(Mutex::new(data)),
            path: None,
            scope: None,
            sealed: false,
        })
    }

    /// Return a view of this storage that exposes only credentials for
    /// the providers in `allow`. `set`/`remove` for providers outside
    /// the scope panic — embedders writing scoped views in a
    /// multi-tenant runtime get an immediate failure if the call site
    /// is wrong.
    ///
    /// The returned `AuthStorage` shares the underlying credential
    /// store via `Arc<Mutex<...>>`; modifying credentials through the
    /// scoped view is visible to the parent and vice versa. The scope
    /// is purely a read/write policy applied on top.
    ///
    /// **Composition semantics:** `s.scoped(["a"]).scoped(["b"])`
    /// **replaces** the scope — it does not intersect. The second
    /// `scoped` discards the first's `scope` field and installs `["b"]`
    /// as the new policy, so providers in `"a"` that aren't in `"b"`
    /// are no longer visible. Embedders needing intersection should
    /// compute it caller-side and pass the result as a single
    /// `scoped(...)` call.
    pub fn scoped<I, S>(&self, allow: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let allow: Vec<String> = allow.into_iter().map(Into::into).collect();
        Self {
            inner: Arc::clone(&self.inner),
            path: self.path.clone(),
            scope: Some(Arc::new(allow)),
            sealed: self.sealed,
        }
    }

    /// Per RFD 0027 §4.5 #8 (Hardening H5): produce a sealed view.
    /// All subsequent `set`/`remove` calls panic. Use when the
    /// embedder wants to enforce "credentials are immutable after
    /// init" — useful in long-running services where a runtime
    /// regression that mutates auth would be surprising.
    ///
    /// **Caveat — view-local, not store-wide:** `sealed()` is a
    /// per-view policy applied on top of the shared `Arc<Mutex<...>>`
    /// credential store. Other (non-sealed) clones of the same
    /// underlying store can still mutate it; a sealed view will see
    /// those mutations on the next `get()`. Embedders requiring a
    /// true seal must drop all non-sealed clones first (or never
    /// hand them out in the first place — wrap construction in a
    /// builder that returns only the sealed view).
    pub fn sealed(self) -> Self {
        Self { sealed: true, ..self }
    }

    pub fn provider_names(&self) -> Vec<String> {
        let names: Vec<String> = self
            .inner
            .lock()
            .map(|g| g.providers.keys().cloned().collect())
            .unwrap_or_default();
        if let Some(scope) = &self.scope {
            names.into_iter().filter(|n| scope.contains(n)).collect()
        } else {
            names
        }
    }

    pub fn get(&self, provider: &str) -> Option<AuthMethod> {
        if let Some(scope) = &self.scope {
            if !scope.iter().any(|p| p == provider) {
                return None;
            }
        }
        self.inner.lock().ok()?.providers.get(provider).cloned()
    }

    /// Set credential for a provider. Per RFD 0027 §4.5 #8 (Hardening
    /// H5):
    /// - If this is a [`sealed()`](Self::sealed) view: panic.
    /// - If this is a [`scoped()`](Self::scoped) view and `provider`
    ///   is outside the scope: panic.
    /// - On-disk persistence (when `path` is set) writes via
    ///   `OpenOptions::mode(0o600).create_new(true)` to a temp file,
    ///   then atomic-renames into place. Partial writes never expose
    ///   credentials in a half-written state, and the file's
    ///   permissions never go through the umask-default 0o644.
    pub fn set(&self, provider: &str, method: AuthMethod) {
        if self.sealed {
            panic!(
                "AuthStorage::set called on a sealed view (provider={provider}); \
                 sealed storage rejects all mutations after construction (RFD 0027 §4.5 #8)"
            );
        }
        if let Some(scope) = &self.scope {
            if !scope.iter().any(|p| p == provider) {
                panic!(
                    "AuthStorage::set called for provider `{provider}` \
                     outside scope `{:?}` (RFD 0027 §4.5 #8)",
                    scope
                );
            }
        }
        if let Ok(mut g) = self.inner.lock() {
            g.providers.insert(provider.to_string(), method);
            if let Some(path) = &self.path {
                if let Err(e) = atomic_write_secure(path, &*g) {
                    tracing::warn!(
                        path = %path.display(),
                        err = %e,
                        "AuthStorage: persisted write failed; in-memory state remains current"
                    );
                }
            }
        }
    }

    /// Remove credential for a provider. Same scoped/sealed rules as
    /// [`set`](Self::set).
    pub fn remove(&self, provider: &str) {
        if self.sealed {
            panic!(
                "AuthStorage::remove called on a sealed view (provider={provider}); \
                 sealed storage rejects all mutations after construction (RFD 0027 §4.5 #8)"
            );
        }
        if let Some(scope) = &self.scope {
            if !scope.iter().any(|p| p == provider) {
                panic!(
                    "AuthStorage::remove called for provider `{provider}` \
                     outside scope `{:?}` (RFD 0027 §4.5 #8)",
                    scope
                );
            }
        }
        if let Ok(mut g) = self.inner.lock() {
            g.providers.remove(provider);
            if let Some(path) = &self.path {
                if let Err(e) = atomic_write_secure(path, &*g) {
                    tracing::warn!(
                        path = %path.display(),
                        err = %e,
                        "AuthStorage: persisted remove-write failed"
                    );
                }
            }
        }
    }
}

/// Write `data` to `path` with mode `0o600` via a temp file + atomic
/// rename. Per RFD 0027 §4.5 #8 (Hardening H5).
///
/// On Windows (which has no POSIX modes) the mode flag is ignored;
/// the atomic-rename invariant still applies. Embedders running on
/// Windows + multi-user systems should layer NTFS ACLs on top of
/// the parent directory.
fn atomic_write_secure(path: &std::path::Path, data: &AuthData) -> std::io::Result<()> {
    use std::io::Write as _;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let txt = serde_json::to_string_pretty(data)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

    // Temp file lives next to the target so rename(2) is atomic
    // on the same filesystem. Suffix is unique-per-process-and-call:
    // pid + nanos + monotonic AtomicU64 counter. The counter prevents
    // collisions on hosts with low-resolution clocks where two
    // parallel `set()` calls could otherwise produce identical
    // (pid, nanos) pairs and the second `create_new` would fail
    // silently (per code-review finding #4, pass-3).
    use std::sync::atomic::{AtomicU64, Ordering};
    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);
    let tmp = path.with_extension(format!(
        "tmp.{}.{}.{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));

    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(txt.as_bytes())?;
        f.sync_all()?;
    }

    // Atomic rename. On POSIX this is rename(2); the destination is
    // replaced if it exists (matching the pre-H5 std::fs::write
    // behaviour, but without the partial-write window).
    let rename_res = std::fs::rename(&tmp, path);
    if rename_res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    rename_res
}

#[cfg(test)]
mod h5_tests {
    use super::*;

    #[test]
    fn from_env_explicit_with_no_matches_returns_empty_storage() {
        // Use a name unlikely to exist in CI environments.
        let s = AuthStorage::from_env_explicit(&[("anthropic", "ZZZ_DEFINITELY_NOT_SET_ABC123")])
            .expect("missing env should not error");
        assert!(s.provider_names().is_empty());
    }

    #[test]
    fn from_env_explicit_picks_up_named_var() {
        let env_name = "PI_SDK_H5_TEST_VAR";
        // Safety: this test mutates process env, but on a name no
        // other test is supposed to touch.
        std::env::set_var(env_name, "stub-value");
        let s = AuthStorage::from_env_explicit(&[("anthropic", env_name)])
            .expect("present env should not error");
        std::env::remove_var(env_name);
        let m = s.get("anthropic").expect("anthropic creds present");
        assert!(matches!(m, AuthMethod::ApiKey { value } if value == "stub-value"));
    }

    #[test]
    fn scoped_view_filters_get() {
        let s = AuthStorage::in_memory();
        s.set("anthropic", AuthMethod::ApiKey { value: "a".into() });
        s.set("openai", AuthMethod::ApiKey { value: "o".into() });
        let scoped = s.scoped(["anthropic"]);
        assert!(scoped.get("anthropic").is_some());
        assert!(scoped.get("openai").is_none(), "openai must be hidden by scope");
        let names = scoped.provider_names();
        assert_eq!(names, vec!["anthropic".to_string()]);
    }

    #[test]
    #[should_panic(expected = "outside scope")]
    fn scoped_view_panics_on_set_outside_scope() {
        let s = AuthStorage::in_memory().scoped(["anthropic"]);
        // openai is outside the scope → panic.
        s.set("openai", AuthMethod::ApiKey { value: "x".into() });
    }

    #[test]
    #[should_panic(expected = "sealed view")]
    fn sealed_view_panics_on_set() {
        let s = AuthStorage::in_memory().sealed();
        s.set("anthropic", AuthMethod::ApiKey { value: "x".into() });
    }

    #[test]
    #[should_panic(expected = "sealed view")]
    fn sealed_view_panics_on_remove() {
        let s = AuthStorage::in_memory().sealed();
        s.remove("anthropic");
    }

    #[test]
    fn atomic_write_creates_file_with_0o600_perms() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("auth.json");
        let s = AuthStorage::open(path.clone()).expect("open");
        s.set("anthropic", AuthMethod::ApiKey { value: "x".into() });
        assert!(path.exists(), "set should have persisted");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "auth file must be 0o600, got 0o{mode:o}");
        }
    }

    #[test]
    fn atomic_write_round_trips_through_open() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("auth.json");
        {
            let s = AuthStorage::open(path.clone()).expect("open");
            s.set("anthropic", AuthMethod::ApiKey { value: "secret123".into() });
        }
        // Re-open and verify the credential round-tripped.
        let s2 = AuthStorage::open(path).expect("re-open");
        let m = s2.get("anthropic").expect("creds present");
        assert!(matches!(m, AuthMethod::ApiKey { value } if value == "secret123"));
    }
}
