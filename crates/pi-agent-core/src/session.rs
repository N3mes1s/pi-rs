use chrono::Utc;
use pi_ai::{Message, ToolCall, ToolResult, Usage};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// One JSONL line in a session file. Sessions form a tree via `parent_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: i64,
    #[serde(flatten)]
    pub kind: SessionEntryKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionEntryKind {
    Meta {
        cwd: String,
        provider: String,
        model: String,
        title: Option<String>,
    },
    SystemPrompt {
        text: String,
    },
    User {
        message: Message,
    },
    Assistant {
        message: Message,
    },
    ToolCall {
        call: ToolCall,
    },
    ToolResult {
        result: ToolResult,
    },
    Usage {
        usage: Usage,
    },
    Compaction {
        summary: String,
        replaced_ids: Vec<String>,
    },
    /// Records that a context file (AGENTS.md / CLAUDE.md / @-ref) was
    /// loaded into this session's prompt. Powers the trajectory recorder
    /// and the AGENTS.md evolution oracle.
    ContextLoad {
        source: String,
        bytes: u64,
        tokens: Option<u64>,
    },
    /// Win/loss verdict appended at session end (or later, out-of-band by
    /// the evolve daemon). Consumed by the benchmark harness.
    Outcome {
        success: bool,
        source: OutcomeSource,
        score: Option<f32>,
        notes: Option<String>,
    },
    /// Identifies which AGENTS.md candidate was active for this session.
    /// Used to attribute outcomes back to specific evolution generations.
    EvolveMarker {
        agents_md_hash: String,
        generation: u32,
        lineage: Vec<String>,
    },
    /// Records the outcome of an RFD 0020 routing decision for a single
    /// assistant turn. Emitted by the runtime in `apply_routing` whenever
    /// the active `RouteMode` is non-Off. `budget_tokens` carries an
    /// optional TALE-EP `<budget>N</budget>` parsed from the prompt —
    /// telemetry-only on the `hard` route, never enforced.
    RoutingDecision {
        route_id: String,
        provider: String,
        model: String,
        thinking: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        budget_tokens: Option<u64>,
    },
    /// Records execution of one tool decision through a sandbox provider
    /// (RFD 0022). Emitted *before* the corresponding `ToolResult` so the
    /// per-call latency, exit status and provider attribution are
    /// observable end-to-end. Compact telemetry — the full ToolResult is
    /// captured separately.
    SandboxAction {
        provider: String,
        tool_name: String,
        duration_ms: u64,
        exit_status: i32,
        #[serde(default)]
        is_error: bool,
    },
    /// Per RFD 0027 §4.5 #10 (Hardening H6): synthetic-user message
    /// injected mid-stream by a `StreamInterceptor`'s `AbortAndInject`
    /// path (typically TTSR — Time-Travelling Streamed Rules).
    /// Distinguishes operator-typed input from runtime-injected
    /// reminders so auditors tailing the JSONL can tell forged
    /// context from real input.
    ///
    /// `source` identifies the interceptor (e.g. `"ttsr"`,
    /// `"safety-filter"`) so multiple interceptors in one runtime can
    /// be attributed individually.
    InterceptorInjection {
        reminder: String,
        source: String,
    },
}

/// How an [`SessionEntryKind::Outcome`] was derived. Replay-sourced
/// outcomes are tagged so the benchmark harness can exclude them from
/// future generations (preventing self-reinforcing loops).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeSource {
    /// User explicitly tagged the session (`:up` / `:down`).
    Explicit,
    /// Derived from git / test / lint / loop-detection signals.
    Heuristic,
    /// Smol-model judge scored the session.
    LlmJudge,
    /// Synthetic rollout produced during evolution benchmarking.
    Replay,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub path: PathBuf,
    pub cwd: String,
    pub provider: String,
    pub model: String,
    pub title: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
pub struct SessionTree {
    pub entries: Vec<SessionEntry>,
}

impl SessionTree {
    /// Returns the linear branch ending at `tip_id`.
    pub fn branch(&self, tip_id: &str) -> Vec<&SessionEntry> {
        let by_id: HashMap<&str, &SessionEntry> =
            self.entries.iter().map(|e| (e.id.as_str(), e)).collect();
        let mut chain = Vec::new();
        let mut cur = by_id.get(tip_id).copied();
        while let Some(e) = cur {
            chain.push(e);
            cur = e.parent_id.as_deref().and_then(|p| by_id.get(p).copied());
        }
        chain.reverse();
        chain
    }

    pub fn tips(&self) -> Vec<&SessionEntry> {
        let parents: std::collections::HashSet<&str> = self
            .entries
            .iter()
            .filter_map(|e| e.parent_id.as_deref())
            .collect();
        self.entries
            .iter()
            .filter(|e| !parents.contains(e.id.as_str()))
            .collect()
    }
}

/// In-memory + JSONL-backed session storage.
#[derive(Debug, Clone)]
pub struct SessionManager {
    state: Arc<Mutex<SessionState>>,
    base_dir: Option<PathBuf>,
    cwd: PathBuf,
}

#[derive(Debug, Default)]
struct SessionState {
    open: HashMap<String, OpenSession>,
}

#[derive(Debug)]
struct OpenSession {
    meta: SessionMeta,
    tree: SessionTree,
    last_id: Option<String>,
    file: Option<PathBuf>,
}

impl SessionManager {
    pub fn in_memory() -> Self {
        Self {
            state: Arc::new(Mutex::new(SessionState::default())),
            base_dir: None,
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        }
    }

    pub fn on_disk(base_dir: PathBuf, cwd: PathBuf) -> std::io::Result<Self> {
        std::fs::create_dir_all(&base_dir)?;
        Ok(Self {
            state: Arc::new(Mutex::new(SessionState::default())),
            base_dir: Some(base_dir),
            cwd,
        })
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Slug for the per-cwd session subdirectory.
    fn cwd_slug(&self) -> String {
        let s = self.cwd.display().to_string();
        s.replace(['/', '\\', ':'], "_")
    }

    pub fn create(&self, provider: &str, model: &str) -> std::io::Result<SessionMeta> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().timestamp_millis();
        let mut path = PathBuf::new();
        if let Some(base) = &self.base_dir {
            let dir = base.join(self.cwd_slug());
            std::fs::create_dir_all(&dir)?;
            path = dir.join(format!("{id}.jsonl"));
        }
        let meta = SessionMeta {
            id: id.clone(),
            path: path.clone(),
            cwd: self.cwd.display().to_string(),
            provider: provider.into(),
            model: model.into(),
            title: None,
            created_at: now,
            updated_at: now,
        };
        let entry = SessionEntry {
            id: Uuid::new_v4().to_string(),
            parent_id: None,
            timestamp: now,
            kind: SessionEntryKind::Meta {
                cwd: meta.cwd.clone(),
                provider: provider.into(),
                model: model.into(),
                title: None,
            },
        };
        let last = entry.id.clone();
        let open = OpenSession {
            meta: meta.clone(),
            tree: SessionTree {
                entries: vec![entry.clone()],
            },
            last_id: Some(last),
            file: if path.as_os_str().is_empty() {
                None
            } else {
                Some(path)
            },
        };
        if let Some(file) = &open.file {
            self.append_to_file(file, &entry)?;
        }
        if let Ok(mut s) = self.state.lock() {
            s.open.insert(id.clone(), open);
        }
        Ok(meta)
    }

    pub fn open_existing(&self, id_or_path: &str) -> std::io::Result<SessionMeta> {
        let path =
            if id_or_path.contains(std::path::MAIN_SEPARATOR) || id_or_path.ends_with(".jsonl") {
                PathBuf::from(id_or_path)
            } else if let Some(base) = &self.base_dir {
                base.join(self.cwd_slug())
                    .join(format!("{id_or_path}.jsonl"))
            } else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "no base dir",
                ));
            };
        let txt = std::fs::read_to_string(&path)?;
        let mut entries: Vec<SessionEntry> = Vec::new();
        for line in txt.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(e) = serde_json::from_str::<SessionEntry>(line) {
                entries.push(e);
            }
        }
        let meta_kind = entries.iter().find_map(|e| match &e.kind {
            SessionEntryKind::Meta {
                cwd,
                provider,
                model,
                title,
            } => Some((cwd.clone(), provider.clone(), model.clone(), title.clone())),
            _ => None,
        });
        let id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let last = entries.last().map(|e| e.id.clone());
        let (cwd, provider, model, title) = meta_kind.unwrap_or_else(|| {
            (
                self.cwd.display().to_string(),
                "anthropic".into(),
                "sonnet".into(),
                None,
            )
        });
        let meta = SessionMeta {
            id: id.clone(),
            path: path.clone(),
            cwd,
            provider,
            model,
            title,
            created_at: entries.first().map(|e| e.timestamp).unwrap_or(0),
            updated_at: entries.last().map(|e| e.timestamp).unwrap_or(0),
        };
        let open = OpenSession {
            meta: meta.clone(),
            tree: SessionTree { entries },
            last_id: last,
            file: Some(path),
        };
        if let Ok(mut s) = self.state.lock() {
            s.open.insert(id, open);
        }
        Ok(meta)
    }

    pub fn list(&self) -> Vec<SessionMeta> {
        let mut out = Vec::new();
        if let Some(base) = &self.base_dir {
            let dir = base.join(self.cwd_slug());
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for ent in rd.flatten() {
                    if let Ok(meta) = self.peek(&ent.path()) {
                        out.push(meta);
                    }
                }
            }
        }
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        out
    }

    fn peek(&self, path: &Path) -> std::io::Result<SessionMeta> {
        let txt = std::fs::read_to_string(path)?;
        let id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let mut created_at = 0;
        let mut updated_at = 0;
        let mut cwd = String::new();
        let mut provider = String::new();
        let mut model = String::new();
        let mut title = None;
        for line in txt.lines() {
            if let Ok(e) = serde_json::from_str::<SessionEntry>(line) {
                if created_at == 0 {
                    created_at = e.timestamp;
                }
                updated_at = e.timestamp;
                if let SessionEntryKind::Meta {
                    cwd: c,
                    provider: p,
                    model: m,
                    title: t,
                } = &e.kind
                {
                    cwd = c.clone();
                    provider = p.clone();
                    model = m.clone();
                    title = t.clone();
                }
            }
        }
        Ok(SessionMeta {
            id,
            path: path.to_path_buf(),
            cwd,
            provider,
            model,
            title,
            created_at,
            updated_at,
        })
    }

    pub fn most_recent(&self) -> Option<SessionMeta> {
        self.list().into_iter().next()
    }

    pub fn append(
        &self,
        session_id: &str,
        kind: SessionEntryKind,
    ) -> std::io::Result<SessionEntry> {
        let mut state = self.state.lock().map_err(io_lock)?;
        let open = state
            .open
            .get_mut(session_id)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "session not open"))?;
        let parent = open.last_id.clone();
        let entry = SessionEntry {
            id: Uuid::new_v4().to_string(),
            parent_id: parent,
            timestamp: Utc::now().timestamp_millis(),
            kind,
        };
        open.tree.entries.push(entry.clone());
        open.last_id = Some(entry.id.clone());
        open.meta.updated_at = entry.timestamp;
        if let Some(file) = open.file.clone() {
            self.append_to_file(&file, &entry)?;
        }
        Ok(entry)
    }

    /// Duplicate the active branch of `source_id` into a brand new session.
    /// Walks `current_branch(source_id)` and replays each entry verbatim
    /// (User / Assistant / ToolCall / ToolResult / Compaction) into the new
    /// session, preserving order. The new session's Meta uses the source
    /// session's provider+model.
    pub fn clone_branch(&self, source_id: &str) -> std::io::Result<SessionMeta> {
        let branch = self.current_branch(source_id);
        if branch.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "source session not open",
            ));
        }
        // Inherit provider+model from the source meta if available.
        let (provider, model) = self
            .meta(source_id)
            .map(|m| (m.provider, m.model))
            .unwrap_or_else(|| ("anthropic".into(), "sonnet".into()));
        let new_meta = self.create(&provider, &model)?;
        for e in branch {
            // Skip the Meta entry — `create` already wrote one for the new
            // session. Replay everything else.
            match e.kind {
                SessionEntryKind::Meta { .. } => continue,
                k => {
                    self.append(&new_meta.id, k)?;
                }
            }
        }
        Ok(new_meta)
    }

    pub fn fork(&self, session_id: &str, from_entry: &str) -> std::io::Result<()> {
        let mut state = self.state.lock().map_err(io_lock)?;
        let open = state
            .open
            .get_mut(session_id)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "session not open"))?;
        if !open.tree.entries.iter().any(|e| e.id == from_entry) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "fork target not found",
            ));
        }
        open.last_id = Some(from_entry.into());
        Ok(())
    }

    pub fn current_branch(&self, session_id: &str) -> Vec<SessionEntry> {
        let state = match self.state.lock() {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let Some(open) = state.open.get(session_id) else {
            return Vec::new();
        };
        let Some(tip) = &open.last_id else {
            return Vec::new();
        };
        open.tree.branch(tip).into_iter().cloned().collect()
    }

    pub fn tree(&self, session_id: &str) -> Option<SessionTree> {
        let state = self.state.lock().ok()?;
        let open = state.open.get(session_id)?;
        Some(open.tree.clone())
    }

    pub fn meta(&self, session_id: &str) -> Option<SessionMeta> {
        let state = self.state.lock().ok()?;
        Some(state.open.get(session_id)?.meta.clone())
    }

    fn append_to_file(&self, file: &Path, entry: &SessionEntry) -> std::io::Result<()> {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file)?;
        // Per RFD 0027 §4.5 #11 (Hardening H6): JSONL goes through
        // WireSerializer with default limits (1 MiB/field cap, ANSI
        // escape stripping, C1/bidi escape). Embedders that tail
        // session files no longer need to defend against
        // model-injected terminal-control sequences themselves.
        let line = WireSerializer::default().serialize(entry);
        f.write_all(line.as_bytes())?;
        f.write_all(b"\n")?;
        Ok(())
    }
}

fn io_lock<E>(_: E) -> std::io::Error {
    std::io::Error::other("session lock poisoned")
}

// ─── WireSerializer (Hardening §4.5 #11, RFD 0027 H6) ────────────

/// Per RFD 0027 §4.5 #11 (Hardening H6): JSONL serializer that
/// applies safety limits to every model-controlled string field
/// before emission.
///
/// **What it defends against:**
/// - Model emits an ANSI escape sequence (e.g. `\x1b]0;rm -rf /\x07`)
///   in a `text` field. Operators tailing the JSONL with `tail -f`
///   in a terminal would see their window title rewritten or worse.
///   `WireSerializer` strips ANSI escape sequences from every string.
/// - Model emits bidi-override characters (U+202A..U+202E,
///   U+2066..U+2069) that flip text rendering direction in the
///   operator's terminal — the trojan-source class of attack.
///   `WireSerializer` `\u`-escapes them.
/// - Model emits C1 control characters (U+0080..U+009F). Some
///   terminals interpret these as additional control sequences;
///   `WireSerializer` `\u`-escapes them.
/// - Model emits a megabyte-sized text block in a single field. Pre-H6
///   the JSONL row balloons proportionally and the operator's `jq`
///   pipeline OOMs. `WireSerializer` hard-truncates each text field
///   at `max_field_bytes` (default 1 MiB) with a `…[N bytes truncated
///   by pi-sdk WireSerializer]` marker.
///
/// **What it does NOT do:**
/// - Validate JSON shape — that's serde's job.
/// - Verify SessionEntryKind variant correctness — that's serde's
///   tag-based dispatch.
/// - Cryptographically sign or chain rows — see RFD 0027 Open
///   Question #9 (HMAC entry_seq, deferred to SDK 1.2).
#[derive(Debug, Clone)]
pub struct WireSerializer {
    /// Maximum bytes per string field. Defaults to 1 MiB; embedders
    /// can tighten via [`with_max_field_bytes`](Self::with_max_field_bytes).
    pub max_field_bytes: usize,
    /// Whether to strip ANSI escape sequences from string values.
    /// Default `true`. Setting to `false` is for callers that
    /// produce JSONL consumed only by machines, never operators.
    pub strip_ansi: bool,
    /// Whether to `\u`-escape bidi-override and C1 control chars.
    /// Default `true`.
    pub escape_bidi_and_c1: bool,
}

impl Default for WireSerializer {
    fn default() -> Self {
        Self {
            max_field_bytes: 1 << 20, // 1 MiB
            strip_ansi: true,
            escape_bidi_and_c1: true,
        }
    }
}

impl WireSerializer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_max_field_bytes(mut self, n: usize) -> Self {
        self.max_field_bytes = n;
        self
    }

    pub fn with_strip_ansi(mut self, on: bool) -> Self {
        self.strip_ansi = on;
        self
    }

    pub fn with_escape_bidi_and_c1(mut self, on: bool) -> Self {
        self.escape_bidi_and_c1 = on;
        self
    }

    /// Serialize a [`SessionEntry`] to its on-disk JSONL form (no
    /// trailing newline). Applies all configured safety limits.
    /// Never panics on serialization failure — falls back to a
    /// minimal `{"id":..,"kind":"meta","_serialize_error":"..."}`
    /// row so the file remains parseable.
    pub fn serialize(&self, entry: &SessionEntry) -> String {
        let mut value = match serde_json::to_value(entry) {
            Ok(v) => v,
            Err(e) => {
                return serde_json::to_string(&serde_json::json!({
                    "id": entry.id,
                    "kind": "meta",
                    "_serialize_error": e.to_string(),
                }))
                .unwrap_or_else(|_| String::from(r#"{"_fatal":"serialize"}"#));
            }
        };
        self.sanitize_value(&mut value);
        serde_json::to_string(&value)
            .unwrap_or_else(|_| String::from(r#"{"_fatal":"serialize"}"#))
    }

    fn sanitize_value(&self, v: &mut serde_json::Value) {
        match v {
            serde_json::Value::String(s) => {
                if self.strip_ansi {
                    *s = strip_ansi(s);
                }
                if self.escape_bidi_and_c1 {
                    *s = escape_bidi_and_c1(s);
                }
                if s.len() > self.max_field_bytes {
                    *s = truncate_with_marker(s, self.max_field_bytes);
                }
            }
            serde_json::Value::Array(arr) => {
                for item in arr.iter_mut() {
                    self.sanitize_value(item);
                }
            }
            serde_json::Value::Object(obj) => {
                for (_, item) in obj.iter_mut() {
                    self.sanitize_value(item);
                }
            }
            _ => {}
        }
    }
}

/// Strip ANSI escape sequences (CSI `\x1b[...`, OSC `\x1b]...`,
/// single `\x1b<final-byte>` forms). Greedy and tolerant — preserves
/// any remaining text intact.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Skip the next byte (intermediate / final / CSI marker).
            match chars.peek() {
                Some('[') => {
                    // CSI: ESC [ ... <final-byte 0x40..=0x7e>.
                    chars.next(); // consume '['
                    while let Some(&c2) = chars.peek() {
                        chars.next();
                        if (0x40..=0x7e).contains(&(c2 as u32)) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    // OSC: ESC ] ... ST (BEL or ESC \).
                    chars.next(); // consume ']'
                    while let Some(&c2) = chars.peek() {
                        chars.next();
                        if c2 == '\x07' {
                            break;
                        }
                        if c2 == '\x1b' {
                            if let Some(&c3) = chars.peek() {
                                if c3 == '\\' {
                                    chars.next();
                                }
                            }
                            break;
                        }
                    }
                }
                Some(_) => {
                    // Single-char escape: skip one final byte.
                    chars.next();
                }
                None => {} // dangling ESC at EOF: drop.
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Replace bidi-override (U+202A..U+202E, U+2066..U+2069) and C1
/// control (U+0080..U+009F) characters with their `\u{XXXX}` escapes.
fn escape_bidi_and_c1(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let cp = c as u32;
        let is_bidi = (0x202A..=0x202E).contains(&cp) || (0x2066..=0x2069).contains(&cp);
        let is_c1 = (0x0080..=0x009F).contains(&cp);
        if is_bidi || is_c1 {
            out.push_str(&format!("\\u{{{:04X}}}", cp));
        } else {
            out.push(c);
        }
    }
    out
}

/// Hard-truncate at the nearest char boundary at-or-before
/// `max_bytes`, append a marker.
fn truncate_with_marker(s: &str, max_bytes: usize) -> String {
    let truncated_at = s.len();
    let cut = s
        .char_indices()
        .take_while(|(i, _)| *i < max_bytes)
        .last()
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    let mut out = String::with_capacity(cut + 64);
    out.push_str(&s[..cut]);
    out.push_str(&format!(
        "…[{} bytes truncated by pi-sdk WireSerializer]",
        truncated_at - cut
    ));
    out
}

#[cfg(test)]
mod h6_wire_serializer_tests {
    use super::*;

    fn entry_with_text(s: &str) -> SessionEntry {
        SessionEntry {
            id: "id-1".into(),
            parent_id: None,
            timestamp: 0,
            kind: SessionEntryKind::SystemPrompt { text: s.into() },
        }
    }

    #[test]
    fn strips_ansi_csi_escape() {
        let s = "before\x1b[31mred\x1b[0mafter";
        let out = strip_ansi(s);
        assert_eq!(out, "beforeredafter");
    }

    #[test]
    fn strips_ansi_osc_set_window_title() {
        // Operator-terminal exfiltration vector: set window title.
        let s = "before\x1b]0;OWNED\x07after";
        let out = strip_ansi(s);
        assert_eq!(out, "beforeafter");
    }

    #[test]
    fn escapes_bidi_overrides() {
        let s = "before\u{202E}after";
        let out = escape_bidi_and_c1(s);
        assert!(out.contains("\\u{202E}"));
        assert!(!out.contains('\u{202E}'));
    }

    #[test]
    fn escapes_c1_control_chars() {
        let s = "before\u{009F}after";
        let out = escape_bidi_and_c1(s);
        assert!(out.contains("\\u{009F}"));
    }

    #[test]
    fn truncates_long_strings_with_marker() {
        let s = "X".repeat(2000);
        let out = truncate_with_marker(&s, 100);
        assert!(out.len() < s.len());
        assert!(out.contains("truncated by pi-sdk WireSerializer"));
        assert!(out.contains("1900 bytes"));
    }

    #[test]
    fn wire_serializer_default_strips_ansi_in_field() {
        let entry = entry_with_text("hi\x1b[31mDANGER\x1b[0m");
        let line = WireSerializer::default().serialize(&entry);
        // Round-trip parse.
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        let text = v.get("text").and_then(|t| t.as_str()).unwrap();
        assert_eq!(text, "hiDANGER");
    }

    #[test]
    fn wire_serializer_default_caps_at_1_mib() {
        let huge = "X".repeat(2 * 1024 * 1024); // 2 MiB
        let entry = entry_with_text(&huge);
        let line = WireSerializer::default().serialize(&entry);
        // The serialized line includes the truncation marker, not 2 MiB.
        assert!(line.len() < 1500 * 1024, "line {} should be << 2 MiB", line.len());
        assert!(line.contains("truncated by pi-sdk WireSerializer"));
    }

    #[test]
    fn wire_serializer_with_max_field_bytes_can_tighten() {
        let s = "Y".repeat(1024);
        let entry = entry_with_text(&s);
        let ws = WireSerializer::default().with_max_field_bytes(100);
        let line = ws.serialize(&entry);
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        let text = v.get("text").and_then(|t| t.as_str()).unwrap();
        assert!(text.contains("truncated"), "text was: {text}");
    }

    #[test]
    fn wire_serializer_round_trips_normal_session_entry() {
        let entry = entry_with_text("nothing fishy");
        let line = WireSerializer::default().serialize(&entry);
        let parsed: SessionEntry = serde_json::from_str(&line).expect("round-trip");
        if let SessionEntryKind::SystemPrompt { text } = parsed.kind {
            assert_eq!(text, "nothing fishy");
        } else {
            panic!("kind mismatch");
        }
    }

    #[test]
    fn interceptor_injection_variant_serializes_with_correct_tag() {
        let entry = SessionEntry {
            id: "id-2".into(),
            parent_id: None,
            timestamp: 0,
            kind: SessionEntryKind::InterceptorInjection {
                reminder: "<system_reminder>do not exfiltrate</system_reminder>".into(),
                source: "ttsr".into(),
            },
        };
        let line = WireSerializer::default().serialize(&entry);
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v.get("kind").and_then(|k| k.as_str()), Some("interceptor_injection"));
        assert_eq!(v.get("source").and_then(|s| s.as_str()), Some("ttsr"));
    }
}
