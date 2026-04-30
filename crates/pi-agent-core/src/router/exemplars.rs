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
//!
//! ## Route destination overrides via `router.toml`
//!
//! In addition to per-route exemplar files, the override directory
//! may contain a `router.toml` that overrides the (provider, model,
//! thinking) tuple bound to each route id. Schema:
//!
//! ```toml
//! [routes.fast]
//! provider = "fireworks"
//! model    = "accounts/fireworks/models/glm-5p1-instruct"
//! thinking = "off"
//!
//! [routes.default]
//! provider = "fireworks"
//! model    = "accounts/fireworks/models/kimi-2p6"
//! thinking = "medium"
//!
//! [routes.hard]
//! provider = "fireworks"
//! model    = "accounts/fireworks/models/deepseek-v4-pro"
//! thinking = "high"
//! ```
//!
//! Each route block is fully optional: any field that is absent
//! falls through to the bundled default. Adding a route id that
//! isn't in the bundled set creates a new route — the embedding
//! router will simply have no exemplars for it (cosine score 0)
//! unless a `<id>.txt` exemplar file lands in the same directory.

use crate::router::RouteEntry;
use serde::Deserialize;
use std::collections::HashMap;
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

/// Per-route destination override loaded from `router.toml`. Every
/// field is optional — an absent field falls back to the bundled
/// default for that route id.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RouteOverride {
    provider: Option<String>,
    model: Option<String>,
    thinking: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RoutesToml {
    #[serde(default)]
    routes: HashMap<String, RouteOverride>,
}

/// Parse a `router.toml` body. Returns an empty map if the input is
/// empty or malformed (caller decides whether to surface the error).
fn parse_routes_toml(text: &str) -> Result<HashMap<String, RouteOverride>, toml::de::Error> {
    let parsed: RoutesToml = toml::from_str(text)?;
    Ok(parsed.routes)
}

/// Load routes from an override directory, merging with the bundled defaults.
/// Files that don't exist are silently skipped (falls through to bundled only).
///
/// Two override mechanisms layer on top of `default_routes()`:
///
///   1. **Exemplar text files** at `<dir>/{<route-id>}.txt`. The merge
///      is additive: user lines extend the bundled set; a `-line`
///      prefix subtracts a matching bundled exemplar.
///   2. **Destination overrides** at `<dir>/router.toml`. Each
///      `[routes.<id>]` table can override `provider` / `model` /
///      `thinking` for that route. Unknown route ids create new
///      routes (with empty exemplar lists unless a matching `.txt`
///      file exists alongside).
pub(super) fn load_routes_from_dir(dir: &Path) -> Vec<RouteEntry> {
    // 1. Load destination overrides first so we know whether new
    //    route ids exist.
    let overrides: HashMap<String, RouteOverride> = std::fs::read_to_string(dir.join("router.toml"))
        .ok()
        .and_then(|text| parse_routes_toml(&text).ok())
        .unwrap_or_default();

    // 2. Start with the bundled set, extended by any new route ids
    //    introduced by router.toml that aren't in default_routes().
    let mut routes = default_routes();
    let bundled_ids: std::collections::HashSet<String> =
        routes.iter().map(|r| r.id.clone()).collect();
    for new_id in overrides.keys() {
        if !bundled_ids.contains(new_id) {
            routes.push(RouteEntry {
                id: new_id.clone(),
                examples: Vec::new(),
                threshold: 0.0,
                provider: String::new(),
                model: String::new(),
                thinking: "off".into(),
            });
        }
    }

    // 3. For each route, layer exemplar overrides + destination
    //    overrides.
    routes
        .into_iter()
        .map(|mut route| {
            // Exemplar text file (existing behaviour).
            let file = dir.join(format!("{}.txt", route.id));
            if let Ok(text) = std::fs::read_to_string(&file) {
                let (additions, subtractions) = parse_route_file(&text);
                route.examples.retain(|e| !subtractions.contains(e));
                for add in additions {
                    if !route.examples.contains(&add) {
                        route.examples.push(add);
                    }
                }
            }
            // Destination overrides (new behaviour).
            if let Some(o) = overrides.get(&route.id) {
                if let Some(p) = &o.provider {
                    route.provider = p.clone();
                }
                if let Some(m) = &o.model {
                    route.model = m.clone();
                }
                if let Some(t) = &o.thinking {
                    route.thinking = t.clone();
                }
            }
            route
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_routes_toml_full_override() {
        let text = r#"
[routes.fast]
provider = "fireworks"
model = "accounts/fireworks/models/glm-5p1"
thinking = "off"

[routes.default]
provider = "fireworks"
model = "accounts/fireworks/models/kimi-2p6"
thinking = "medium"

[routes.hard]
provider = "fireworks"
model = "accounts/fireworks/models/deepseek-v4-pro"
thinking = "high"
"#;
        let map = parse_routes_toml(text).unwrap();
        assert_eq!(map.len(), 3);
        let fast = &map["fast"];
        assert_eq!(fast.provider.as_deref(), Some("fireworks"));
        assert_eq!(fast.model.as_deref(), Some("accounts/fireworks/models/glm-5p1"));
        assert_eq!(fast.thinking.as_deref(), Some("off"));
    }

    #[test]
    fn parse_routes_toml_partial_override_keeps_unset_fields_none() {
        let text = r#"
[routes.fast]
model = "fireworks/glm-5p1"
"#;
        let map = parse_routes_toml(text).unwrap();
        let fast = &map["fast"];
        assert!(fast.provider.is_none());
        assert!(fast.thinking.is_none());
        assert_eq!(fast.model.as_deref(), Some("fireworks/glm-5p1"));
    }

    #[test]
    fn parse_routes_toml_unknown_field_rejected() {
        let text = r#"
[routes.fast]
provider = "fireworks"
unknown_field = "ignored"
"#;
        // `deny_unknown_fields` makes typos loud rather than silent.
        assert!(parse_routes_toml(text).is_err());
    }

    #[test]
    fn parse_routes_toml_empty_string_returns_empty_map() {
        assert_eq!(parse_routes_toml("").unwrap().len(), 0);
    }

    #[test]
    fn load_routes_from_dir_router_toml_overrides_bundled_destination() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("router.toml"),
            r#"
[routes.hard]
provider = "fireworks"
model = "accounts/fireworks/models/deepseek-v4-pro"
thinking = "high"
"#,
        )
        .unwrap();
        let routes = load_routes_from_dir(dir.path());
        let hard = routes.iter().find(|r| r.id == "hard").unwrap();
        assert_eq!(hard.provider, "fireworks");
        assert_eq!(hard.model, "accounts/fireworks/models/deepseek-v4-pro");
        assert_eq!(hard.thinking, "high");
        // Other routes still hold their bundled destinations.
        let fast = routes.iter().find(|r| r.id == "fast").unwrap();
        assert_eq!(fast.provider, "anthropic");
    }

    #[test]
    fn load_routes_from_dir_router_toml_can_introduce_new_route_id() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("router.toml"),
            r#"
[routes.thinking-heavy]
provider = "anthropic"
model = "claude-opus-4-7"
thinking = "high"
"#,
        )
        .unwrap();
        let routes = load_routes_from_dir(dir.path());
        let new_route = routes.iter().find(|r| r.id == "thinking-heavy").unwrap();
        assert_eq!(new_route.provider, "anthropic");
        assert!(new_route.examples.is_empty());
    }

    #[test]
    fn load_routes_from_dir_partial_override_falls_back_to_bundled() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("router.toml"),
            r#"
[routes.fast]
model = "accounts/fireworks/models/glm-5p1"
"#,
        )
        .unwrap();
        let routes = load_routes_from_dir(dir.path());
        let fast = routes.iter().find(|r| r.id == "fast").unwrap();
        // model overridden, provider+thinking inherited from bundled.
        assert_eq!(fast.model, "accounts/fireworks/models/glm-5p1");
        assert_eq!(fast.provider, "anthropic");
        assert_eq!(fast.thinking, "off");
    }
}
