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
            file: if path.as_os_str().is_empty() { None } else { Some(path) },
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
        let path = if id_or_path.contains(std::path::MAIN_SEPARATOR) || id_or_path.ends_with(".jsonl") {
            PathBuf::from(id_or_path)
        } else if let Some(base) = &self.base_dir {
            base.join(self.cwd_slug()).join(format!("{id_or_path}.jsonl"))
        } else {
            return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no base dir"));
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
        let id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
        let last = entries.last().map(|e| e.id.clone());
        let (cwd, provider, model, title) = meta_kind
            .unwrap_or_else(|| (self.cwd.display().to_string(), "anthropic".into(), "sonnet".into(), None));
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
        let id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
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

    pub fn append(&self, session_id: &str, kind: SessionEntryKind) -> std::io::Result<SessionEntry> {
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

    pub fn fork(&self, session_id: &str, from_entry: &str) -> std::io::Result<()> {
        let mut state = self.state.lock().map_err(io_lock)?;
        let open = state
            .open
            .get_mut(session_id)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "session not open"))?;
        if !open.tree.entries.iter().any(|e| e.id == from_entry) {
            return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "fork target not found"));
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
        let line = serde_json::to_string(entry).unwrap_or_default();
        f.write_all(line.as_bytes())?;
        f.write_all(b"\n")?;
        Ok(())
    }
}

fn io_lock<E>(_: E) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, "session lock poisoned")
}
