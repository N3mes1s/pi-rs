//! In-process language-server registry (D1.engine).
//!
//! The transport ([`super::transport::LspClient`]) handles a single
//! server connection. The engine is one level up: it owns a
//! `language → Arc<LspClient>` map, lazily spawns servers on first use,
//! and exposes a small surface (`diagnostics`, `definition`, `hover`,
//! …) that the agent-facing tool ([`super::tool`]) calls into.
//!
//! Design notes:
//!
//! * One server per language, shared across files. The cost of spawning
//!   `rust-analyzer` (multiple seconds + indexing) makes this the only
//!   sensible policy. Locking is per-language, so two requests against
//!   different languages are concurrent.
//! * `LspClient` is wrapped in `Arc` (no inner mutex) — the transport
//!   already serialises stdin writes internally and the dispatch table
//!   is concurrent. We hold an outer lock only around *spawning* (so
//!   two callers don't both try to launch rust-analyzer).
//! * Catalogue + config drive which command we spawn for a language;
//!   per-language overrides in [`super::config::LspConfig`] win.
//! * The engine never panics on a server that fails to start (e.g.
//!   `rust-analyzer` not on PATH); it returns [`EngineError::Spawn`]
//!   so the tool can degrade to a clean error message.
//!
//! The engine intentionally does *not* implement the full LSP request
//! catalogue here — we provide the smallest set the tool needs and
//! leave room to grow. Each method below assembles the LSP params from
//! a `(file, line?, col?)` triple, calls
//! [`LspClient::send_request`], and returns the raw `serde_json::Value`
//! so the tool can hand it back as structured JSON.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};
use thiserror::Error;
use tokio::sync::Mutex;

use super::catalogue::{language_for_extension, DEFAULT_CATALOGUE};
use super::config::LspConfig;
use super::transport::{LspClient, TransportError};

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("no language server registered for extension `.{0}`")]
    UnknownLanguage(String),
    #[error("language `{0}` is disabled by config")]
    Disabled(String),
    #[error("spawning language server for `{language}` failed: {source}")]
    Spawn {
        language: String,
        #[source]
        source: TransportError,
    },
    #[error("transport error: {0}")]
    Transport(#[from] TransportError),
    #[error("file path is not absolute: {0}")]
    RelativePath(PathBuf),
}

/// One running server entry. The `opened` set tracks which files we
/// have notified the server about with `textDocument/didOpen`; rust-
/// analyzer (and most servers) refuse to answer per-document requests
/// for files they have not been told about, so the engine sends an
/// idempotent didOpen on first contact with each path.
struct ServerEntry {
    client: Arc<LspClient>,
    opened: Mutex<HashSet<PathBuf>>,
}

/// Lazy registry of LspClient connections, keyed by language id.
pub struct LspEngine {
    config: LspConfig,
    servers: Mutex<HashMap<String, Arc<ServerEntry>>>,
    /// Where files live; used as `rootUri` for `initialize`. The engine
    /// is created once per process and pinned to a workspace root.
    root: PathBuf,
}

impl LspEngine {
    pub fn new(config: LspConfig, root: PathBuf) -> Self {
        Self {
            config,
            servers: Mutex::new(HashMap::new()),
            root,
        }
    }

    /// Resolve a path's file extension to a language id, honouring the
    /// default catalogue (config overrides apply later, when we look
    /// up the spawn command).
    pub fn language_for(path: &Path) -> Option<&'static str> {
        let ext = path.extension().and_then(|e| e.to_str())?;
        language_for_extension(ext).map(|e| e.language)
    }

    /// Return the currently-running languages. Used to back the
    /// `status` op without round-tripping any actual request.
    pub async fn running_languages(&self) -> Vec<String> {
        self.servers.lock().await.keys().cloned().collect()
    }

    /// Drop a running server, forcing the next request to respawn.
    /// Backs the `reload` op.
    pub async fn reload(&self, language: &str) -> bool {
        self.servers.lock().await.remove(language).is_some()
    }

    /// Get-or-spawn the server for `language`. Honours `LspConfig`:
    /// returns `Disabled` if the master switch is off and the language
    /// has no per-language opt-in; respects per-language `command`
    /// overrides too.
    async fn ensure(&self, language: &str) -> Result<Arc<ServerEntry>, EngineError> {
        if !self.config.is_language_enabled(language) {
            return Err(EngineError::Disabled(language.into()));
        }
        // Fast path: already running.
        {
            let map = self.servers.lock().await;
            if let Some(entry) = map.get(language) {
                return Ok(entry.clone());
            }
        }
        // Spawn under the lock to avoid double-launch.
        let mut map = self.servers.lock().await;
        if let Some(entry) = map.get(language) {
            return Ok(entry.clone());
        }
        // Determine argv: per-language override wins, else catalogue.
        let owned: Vec<String>;
        let argv: Vec<&str> = if let Some(o) = self.config.command_override(language) {
            owned = o.iter().cloned().collect();
            owned.iter().map(|s| s.as_str()).collect()
        } else {
            let entry = DEFAULT_CATALOGUE
                .iter()
                .find(|e| e.language == language)
                .ok_or_else(|| EngineError::UnknownLanguage(language.into()))?;
            entry.command.iter().copied().collect()
        };
        let client = LspClient::spawn(&argv).await.map_err(|source| EngineError::Spawn {
            language: language.into(),
            source,
        })?;
        // The spec demands an `initialize` round-trip before any other
        // request. `rootUri` is constructed from the engine's pinned
        // workspace root.
        let root_uri = format!("file://{}", self.root.display());
        client.initialize(&root_uri).await.map_err(|source| EngineError::Spawn {
            language: language.into(),
            source,
        })?;
        let entry = Arc::new(ServerEntry {
            client: Arc::new(client),
            opened: Mutex::new(HashSet::new()),
        });
        map.insert(language.into(), entry.clone());
        Ok(entry)
    }

    /// Resolve the server entry for `file`. Used by request methods
    /// that also need to consult / update the entry's `opened` set.
    async fn entry_for(&self, file: &Path) -> Result<Arc<ServerEntry>, EngineError> {
        if !file.is_absolute() {
            return Err(EngineError::RelativePath(file.to_path_buf()));
        }
        let ext = file
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default();
        let language = language_for_extension(ext)
            .map(|e| e.language)
            .ok_or_else(|| EngineError::UnknownLanguage(ext.into()))?;
        self.ensure(language).await
    }

    /// Idempotently send `textDocument/didOpen` for `file`. Reads the
    /// file's contents on first contact, then never re-reads (server
    /// keeps tracking the document for the life of the process). Files
    /// that fail to read are silently skipped — the request will fail
    /// downstream with a clearer message from the server.
    async fn ensure_opened(
        &self,
        entry: &ServerEntry,
        file: &Path,
    ) -> Result<(), EngineError> {
        {
            let opened = entry.opened.lock().await;
            if opened.contains(file) {
                return Ok(());
            }
        }
        let Ok(text) = std::fs::read_to_string(file) else {
            return Ok(());
        };
        let language = language_for_extension(
            file.extension().and_then(|e| e.to_str()).unwrap_or_default(),
        )
        .map(|e| e.language)
        .unwrap_or("plaintext");
        let params = json!({
            "textDocument": {
                "uri": Self::file_uri(file),
                "languageId": language,
                "version": 1,
                "text": text,
            }
        });
        entry
            .client
            .send_notification("textDocument/didOpen", params)
            .await?;
        entry.opened.lock().await.insert(file.to_path_buf());
        Ok(())
    }

    /// Resolve the entry, ensure didOpen, then return the bare client
    /// for the request. This is the standard prelude for any
    /// `textDocument/*` request below.
    async fn prepare(&self, file: &Path) -> Result<Arc<LspClient>, EngineError> {
        let entry = self.entry_for(file).await?;
        self.ensure_opened(&entry, file).await?;
        Ok(entry.client.clone())
    }

    fn file_uri(file: &Path) -> String {
        format!("file://{}", file.display())
    }

    pub async fn diagnostics(&self, file: &Path) -> Result<Value, EngineError> {
        let client = self.prepare(file).await?;
        // textDocument/diagnostic (LSP 3.17 pull diagnostics). Servers
        // that only support push-mode will reject; the tool layer can
        // map that to an empty array.
        let params = json!({
            "textDocument": { "uri": Self::file_uri(file) },
        });
        Ok(client.send_request("textDocument/diagnostic", params).await?)
    }

    /// `textDocument/formatting` — full-document formatting (LSP 3.17
    /// §3.17.13). Returns `TextEdit[]` (or `null`) verbatim. Sends
    /// conventional defaults for `FormattingOptions`; per-file overrides
    /// are out of scope (see RFD 0002 P1 0007).
    pub async fn formatting(&self, file: &Path) -> Result<Value, EngineError> {
        let client = self.prepare(file).await?;
        let params = json!({
            "textDocument": { "uri": Self::file_uri(file) },
            "options": {
                "tabSize": 4,
                "insertSpaces": true,
                "trimTrailingWhitespace": true,
                "insertFinalNewline": true,
                "trimFinalNewlines": true,
            },
        });
        Ok(client.send_request("textDocument/formatting", params).await?)
    }

    pub async fn definition(
        &self,
        file: &Path,
        line: u32,
        col: u32,
    ) -> Result<Value, EngineError> {
        let client = self.prepare(file).await?;
        let params = json!({
            "textDocument": { "uri": Self::file_uri(file) },
            "position": { "line": line, "character": col },
        });
        Ok(client.send_request("textDocument/definition", params).await?)
    }

    pub async fn hover(&self, file: &Path, line: u32, col: u32) -> Result<Value, EngineError> {
        let client = self.prepare(file).await?;
        let params = json!({
            "textDocument": { "uri": Self::file_uri(file) },
            "position": { "line": line, "character": col },
        });
        Ok(client.send_request("textDocument/hover", params).await?)
    }

    pub async fn references(
        &self,
        file: &Path,
        line: u32,
        col: u32,
    ) -> Result<Value, EngineError> {
        let client = self.prepare(file).await?;
        let params = json!({
            "textDocument": { "uri": Self::file_uri(file) },
            "position": { "line": line, "character": col },
            "context": { "includeDeclaration": true },
        });
        Ok(client.send_request("textDocument/references", params).await?)
    }

    pub async fn symbols(&self, file: &Path) -> Result<Value, EngineError> {
        let client = self.prepare(file).await?;
        let params = json!({ "textDocument": { "uri": Self::file_uri(file) }});
        Ok(client.send_request("textDocument/documentSymbol", params).await?)
    }

    /// `textDocument/typeDefinition` — same wire shape as `definition`.
    /// The reply is typically `Location | Location[] | LocationLink[] |
    /// null` (LSP 3.17 §3.17.7) which we surface verbatim as JSON.
    pub async fn type_definition(
        &self,
        file: &Path,
        line: u32,
        col: u32,
    ) -> Result<Value, EngineError> {
        let client = self.prepare(file).await?;
        let params = json!({
            "textDocument": { "uri": Self::file_uri(file) },
            "position": { "line": line, "character": col },
        });
        Ok(client
            .send_request("textDocument/typeDefinition", params)
            .await?)
    }

    /// `textDocument/implementation` — locate concrete impls of the
    /// symbol under the cursor. Same shape as `definition`.
    pub async fn implementation(
        &self,
        file: &Path,
        line: u32,
        col: u32,
    ) -> Result<Value, EngineError> {
        let client = self.prepare(file).await?;
        let params = json!({
            "textDocument": { "uri": Self::file_uri(file) },
            "position": { "line": line, "character": col },
        });
        Ok(client
            .send_request("textDocument/implementation", params)
            .await?)
    }

    /// `textDocument/rename` — return the workspace edit. We don't
    /// apply it here; the caller (agent) decides whether to fan the
    /// edits out to file tools.
    pub async fn rename(
        &self,
        file: &Path,
        line: u32,
        col: u32,
        new_name: &str,
    ) -> Result<Value, EngineError> {
        let client = self.prepare(file).await?;
        let params = json!({
            "textDocument": { "uri": Self::file_uri(file) },
            "position": { "line": line, "character": col },
            "newName": new_name,
        });
        Ok(client.send_request("textDocument/rename", params).await?)
    }

    /// `textDocument/codeAction` for `range` (a single zero-length
    /// range at `(line, col)` if you only have a position). We pass an
    /// empty `only` filter, which means *all* `CodeActionKind`s. We
    /// also leave `diagnostics` empty — pure on-cursor refactors. The
    /// caller is responsible for any subsequent `codeAction/resolve`
    /// round-trip.
    pub async fn code_actions(
        &self,
        file: &Path,
        start_line: u32,
        start_col: u32,
        end_line: u32,
        end_col: u32,
    ) -> Result<Value, EngineError> {
        let client = self.prepare(file).await?;
        let params = json!({
            "textDocument": { "uri": Self::file_uri(file) },
            "range": {
                "start": { "line": start_line, "character": start_col },
                "end":   { "line": end_line,   "character": end_col   },
            },
            "context": {
                "diagnostics": [],
                // Empty `only` ⇒ server returns every kind it supports.
            },
        });
        Ok(client
            .send_request("textDocument/codeAction", params)
            .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_enabled() -> LspConfig {
        let mut c = LspConfig::default();
        c.enabled = true;
        c
    }

    #[test]
    fn language_for_dispatches_off_extension() {
        let p = PathBuf::from("/tmp/foo.rs");
        assert_eq!(LspEngine::language_for(&p), Some("rust"));
        let p = PathBuf::from("/tmp/foo.tsx");
        assert_eq!(LspEngine::language_for(&p), Some("typescript"));
        let p = PathBuf::from("/tmp/foo.unknown");
        assert!(LspEngine::language_for(&p).is_none());
    }

    #[tokio::test]
    async fn relative_paths_are_rejected_before_spawn() {
        let engine = LspEngine::new(cfg_enabled(), PathBuf::from("/tmp"));
        let err = engine
            .definition(Path::new("foo.rs"), 1, 1)
            .await
            .unwrap_err();
        assert!(matches!(err, EngineError::RelativePath(_)));
    }

    #[tokio::test]
    async fn unknown_extension_is_rejected_before_spawn() {
        let engine = LspEngine::new(cfg_enabled(), PathBuf::from("/tmp"));
        let err = engine
            .definition(Path::new("/tmp/foo.zzzzz"), 1, 1)
            .await
            .unwrap_err();
        assert!(matches!(err, EngineError::UnknownLanguage(_)));
    }

    #[tokio::test]
    async fn disabled_language_blocks_spawn() {
        let mut cfg = LspConfig::default(); // master = off
        cfg.enabled = false;
        let engine = LspEngine::new(cfg, PathBuf::from("/tmp"));
        let err = engine
            .definition(Path::new("/tmp/x.rs"), 1, 1)
            .await
            .unwrap_err();
        assert!(matches!(err, EngineError::Disabled(_)));
    }

    #[tokio::test]
    async fn running_languages_starts_empty() {
        let engine = LspEngine::new(cfg_enabled(), PathBuf::from("/tmp"));
        assert!(engine.running_languages().await.is_empty());
        assert!(!engine.reload("rust").await);
    }

    /// Engines spawned with a per-language *command override* pointing at
    /// our fake python server should succeed end-to-end. Routes us
    /// through `LspConfig::command_override`, the lazy spawn dance, and
    /// the initialize handshake — without depending on any real LSP
    /// binary being installed.
    #[tokio::test]
    async fn command_override_lets_engine_use_fake_server_for_rust() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fake_lsp_server.py");
        let mut cfg = cfg_enabled();
        cfg.languages.insert(
            "rust".into(),
            super::super::config::LanguageConfig {
                enabled: Some(true),
                command: Some(vec![
                    "python3".into(),
                    path.to_string_lossy().into_owned(),
                ]),
            },
        );
        let engine = LspEngine::new(cfg, PathBuf::from("/tmp"));
        // diagnostics/definition will get a `Method not found` error
        // from the fake server, surfaced as TransportError::Rpc — that's
        // fine, what we're proving is the spawn + handshake landed.
        let err = engine
            .diagnostics(Path::new("/tmp/x.rs"))
            .await
            .unwrap_err();
        match err {
            EngineError::Transport(TransportError::Rpc { code, .. }) => {
                assert_eq!(code, -32601, "fake server returns Method not found");
            }
            other => panic!("expected Rpc(-32601), got {other:?}"),
        }
        // And it should be in the running set now.
        assert_eq!(engine.running_languages().await, vec!["rust".to_string()]);
        assert!(engine.reload("rust").await);
        assert!(engine.running_languages().await.is_empty());
    }
}
