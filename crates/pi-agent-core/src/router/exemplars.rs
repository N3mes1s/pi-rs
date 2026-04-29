//! Bundled route exemplars + override-directory merge logic.
//!
//! Three text files at `data/routes/{fast,default,hard}.txt` ship as
//! `include_str!` blobs; each holds ~100 hand-paraphrased exemplars
//! covering one route. At runtime `EmbeddingRouter::bundled()` calls
//! `resolve_router_dir` to find a user override directory — the
//! priority is `PI_ROUTER_DIR` env var → `<cwd>/.pi/router/` →
//! `~/.pi/agent/router/`. If a directory is found, `load_routes_from_dir`
//! reads `{fast,default,hard}.txt` from it and merges with the
//! bundled defaults: blank/`#` lines ignored, `-line` subtracts a
//! bundled exemplar, anything else appends (deduped). Override-merge
//! semantics are pinned by `tests/router_overrides.rs`.

use crate::router::RouteEntry;
use std::path::{Path, PathBuf};

/// Bundled route example text files — included at compile time so the binary
/// has sane defaults even without any `~/.pi/agent/router/` directory.
static BUNDLED_FAST_TXT: &str = include_str!("../../data/routes/fast.txt");
static BUNDLED_DEFAULT_TXT: &str = include_str!("../../data/routes/default.txt");
static BUNDLED_HARD_TXT: &str = include_str!("../../data/routes/hard.txt");

/// Parse a route text file (comment lines starting with `#` and blank lines
/// are ignored; `-line` prefix removes a bundled example from the merged set).
fn parse_route_file(text: &str) -> (Vec<String>, Vec<String>) {
    let mut additions = Vec::new();
    let mut subtractions = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('-') {
            subtractions.push(rest.trim().to_string());
        } else {
            additions.push(trimmed.to_string());
        }
    }
    (additions, subtractions)
}

/// Build the bundled example list for one route id.
fn bundled_examples_for(route_id: &str) -> Vec<String> {
    let text = match route_id {
        "fast" => BUNDLED_FAST_TXT,
        "default" => BUNDLED_DEFAULT_TXT,
        "hard" => BUNDLED_HARD_TXT,
        _ => return Vec::new(),
    };
    let (additions, _) = parse_route_file(text);
    additions
}

pub(super) fn default_routes() -> Vec<RouteEntry> {
    vec![
        RouteEntry {
            id: "fast".into(),
            examples: bundled_examples_for("fast"),
            threshold: 0.0,
            provider: "anthropic".into(),
            model: "claude-haiku-4-5-20251001".into(),
            thinking: "off".into(),
        },
        RouteEntry {
            id: "default".into(),
            examples: bundled_examples_for("default"),
            threshold: 0.0,
            provider: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
            thinking: "medium".into(),
        },
        RouteEntry {
            id: "hard".into(),
            examples: bundled_examples_for("hard"),
            threshold: 0.0,
            provider: "openai".into(),
            model: "gpt-5.4".into(),
            thinking: "xhigh".into(),
        },
    ]
}

/// Resolve the router override directory.  Priority order:
///   1. `PI_ROUTER_DIR` env var
///   2. `<project>/.pi/router/` (if the current directory has a `.pi/router/` dir)
///   3. `~/.pi/agent/router/`
///
/// Returns `None` if none of the above directories exist AND `create_if_missing`
/// is false.  When `create_if_missing` is true (first-run materialisation path),
/// the function creates `~/.pi/agent/router/` and writes the bundled defaults
/// there, then returns the path.
pub fn resolve_router_dir(create_if_missing: bool) -> Option<PathBuf> {
    // 1. Explicit env override.
    if let Ok(p) = std::env::var("PI_ROUTER_DIR") {
        let path = PathBuf::from(p);
        if path.exists() || create_if_missing {
            return Some(path);
        }
    }

    // 2. Per-project `.pi/router/` (cwd walk up to root).
    if let Ok(cwd) = std::env::current_dir() {
        let candidate = cwd.join(".pi").join("router");
        if candidate.exists() {
            return Some(candidate);
        }
    }

    // 3. User-global `~/.pi/agent/router/`.
    let global = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".pi")
        .join("agent")
        .join("router");
    if global.exists() {
        return Some(global);
    }

    if create_if_missing {
        if std::fs::create_dir_all(&global).is_ok() {
            materialise_bundled_routes(&global);
        }
        return Some(global);
    }

    None
}

/// Write the bundled route examples to `dir/{fast,default,hard}.txt` so the
/// user has something to edit.  Existing files are never overwritten.
fn materialise_bundled_routes(dir: &Path) {
    for (name, content) in [
        ("fast.txt", BUNDLED_FAST_TXT),
        ("default.txt", BUNDLED_DEFAULT_TXT),
        ("hard.txt", BUNDLED_HARD_TXT),
    ] {
        let path = dir.join(name);
        if !path.exists() {
            let _ = std::fs::write(path, content);
        }
    }
}

/// Load routes from an override directory, merging with the bundled defaults.
/// Files that don't exist are silently skipped (falls through to bundled only).
/// The merge is additive: user lines extend the bundled set.  A `-line` prefix
/// subtracts the matching example from the bundled set (same convention as
/// `pi --policy` deny-regex).
pub(super) fn load_routes_from_dir(dir: &Path) -> Vec<RouteEntry> {
    default_routes()
        .into_iter()
        .map(|mut route| {
            let file = dir.join(format!("{}.txt", route.id));
            if let Ok(text) = std::fs::read_to_string(&file) {
                let (additions, subtractions) = parse_route_file(&text);
                // Remove subtracted examples.
                route.examples.retain(|e| !subtractions.contains(e));
                // Append new examples (avoid exact duplicates).
                for add in additions {
                    if !route.examples.contains(&add) {
                        route.examples.push(add);
                    }
                }
            }
            route
        })
        .collect()
}
