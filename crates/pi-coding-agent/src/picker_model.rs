//! oh-my-pi-style `/model` picker backbone.
//!
//! This module is the *pure* state machine: the TUI feeds it tabs,
//! query, role-set context-menu transitions; it returns ranked items.
//! The rendering side (overlay frame composition, key handling) lives
//! in `modes::interactive`.
//!
//! Two tabs:
//!
//! * **All** — every (provider/model) the registry knows about.
//! * **Canonical** — only models that carry an `alias` (e.g. `sonnet`,
//!   `gpt-4o`, …). Smaller, curated list.
//!
//! Each entry carries a *role badge* — a one-letter marker indicating
//! which `Settings.roles.*` slot currently points at it
//! (`d`efault · `s`mol · `S`low · `p`lan · `c`ommit). When no role
//! claims the model the badge is empty.
//!
//! Pressing Enter on a row opens a context menu listing the same five
//! roles plus a "temporary" option (apply only for the next message).

use crate::picker::{PickItem, Picker};
use pi_agent_core::settings::{ModelRoles, Role};
use pi_ai::ModelRegistry;

/// Tabs offered by the `/model` picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelTab {
    All,
    Canonical,
}

impl ModelTab {
    pub const ALL: &'static [ModelTab] = &[ModelTab::All, ModelTab::Canonical];

    pub fn label(self) -> &'static str {
        match self {
            ModelTab::All => "ALL",
            ModelTab::Canonical => "CANONICAL",
        }
    }

    pub fn next(self) -> ModelTab {
        match self {
            ModelTab::All => ModelTab::Canonical,
            ModelTab::Canonical => ModelTab::All,
        }
    }
}

/// Roles that can be assigned via the context menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextMenuChoice {
    Default,
    Smol,
    Slow,
    Plan,
    Commit,
    /// One-shot: apply for the next message, then revert. Maps to the
    /// existing `scoped_models` flow.
    Temporary,
}

impl ContextMenuChoice {
    pub const ALL: &'static [ContextMenuChoice] = &[
        ContextMenuChoice::Default,
        ContextMenuChoice::Smol,
        ContextMenuChoice::Slow,
        ContextMenuChoice::Plan,
        ContextMenuChoice::Commit,
        ContextMenuChoice::Temporary,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ContextMenuChoice::Default => "default",
            ContextMenuChoice::Smol => "smol",
            ContextMenuChoice::Slow => "slow",
            ContextMenuChoice::Plan => "plan",
            ContextMenuChoice::Commit => "commit",
            ContextMenuChoice::Temporary => "temporary",
        }
    }

    /// The `Role` that this choice writes into. `None` for `Temporary`
    /// (handled out-of-band).
    pub fn as_role(self) -> Option<Role> {
        match self {
            ContextMenuChoice::Default => Some(Role::Default),
            ContextMenuChoice::Smol => Some(Role::Smol),
            ContextMenuChoice::Slow => Some(Role::Slow),
            ContextMenuChoice::Plan => Some(Role::Plan),
            ContextMenuChoice::Commit => Some(Role::Commit),
            ContextMenuChoice::Temporary => None,
        }
    }
}

/// A single row in the `/model` picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRow {
    pub provider: String,
    pub model: String,
    pub alias: Option<String>,
    /// Sorted list of role markers ('d'/'s'/'S'/'p'/'c') for whichever
    /// `Settings.roles.*` slots currently point at this model.
    pub badges: Vec<char>,
}

impl ModelRow {
    /// Picker label: `provider/model [alias] (badges)`.
    pub fn label(&self) -> String {
        let mut s = format!("{}/{}", self.provider, self.model);
        if let Some(a) = &self.alias {
            s.push_str(&format!(" [{}]", a));
        }
        if !self.badges.is_empty() {
            let badge_str: String = self.badges.iter().collect();
            s.push_str(&format!(" ({})", badge_str));
        }
        s
    }

    pub fn full_id(&self) -> String {
        format!("{}/{}", self.provider, self.model)
    }
}

/// Pull rows from the [`ModelRegistry`], optionally filtered to the
/// canonical (aliased) subset, with role badges baked in from the
/// supplied [`ModelRoles`].
pub fn collect_rows(registry: &ModelRegistry, roles: &ModelRoles, tab: ModelTab) -> Vec<ModelRow> {
    let mut out: Vec<ModelRow> = Vec::new();
    for p in registry.providers() {
        for m in &p.models {
            if matches!(tab, ModelTab::Canonical) && m.alias.is_none() {
                continue;
            }
            let full = format!("{}/{}", p.name, m.id);
            let badges = badges_for(roles, &full, &m.id, m.alias.as_deref());
            out.push(ModelRow {
                provider: p.name.clone(),
                model: m.id.clone(),
                alias: m.alias.clone(),
                badges,
            });
        }
    }
    out
}

/// Compute the role badges for a single model. We check each role
/// slot's stored value for an exact match against `full_id` (preferred),
/// the bare `model_id`, or the `alias`.
fn badges_for(
    roles: &ModelRoles,
    full_id: &str,
    model_id: &str,
    alias: Option<&str>,
) -> Vec<char> {
    let mut out = Vec::new();
    let candidates: [&Option<String>; 5] = [
        &roles.default,
        &roles.smol,
        &roles.slow,
        &roles.plan,
        &roles.commit,
    ];
    let markers: [char; 5] = ['d', 's', 'S', 'p', 'c'];
    for (slot, marker) in candidates.iter().zip(markers.iter()) {
        let Some(v) = slot.as_ref() else { continue };
        if matches_any(v, full_id, model_id, alias) {
            out.push(*marker);
        }
    }
    out
}

fn matches_any(v: &str, full_id: &str, model_id: &str, alias: Option<&str>) -> bool {
    v == full_id || v == model_id || alias.is_some_and(|a| v == a)
}

/// Convenience: build a [`Picker`] populated from a `ModelTab`. The
/// stored value is the full provider/model id so the existing
/// `/model <value>` slash code path reuses without change.
pub fn picker_for(
    registry: &ModelRegistry,
    roles: &ModelRoles,
    tab: ModelTab,
) -> Picker<String> {
    let rows = collect_rows(registry, roles, tab);
    let items: Vec<PickItem<String>> = rows
        .into_iter()
        .map(|r| PickItem {
            label: r.label(),
            value: r.full_id(),
        })
        .collect();
    Picker::new(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_agent_core::settings::ModelRoles;
    use pi_ai::AuthStorage;

    fn registry() -> ModelRegistry {
        ModelRegistry::new(AuthStorage::in_memory())
    }

    #[test]
    fn tabs_cycle() {
        assert_eq!(ModelTab::All.next(), ModelTab::Canonical);
        assert_eq!(ModelTab::Canonical.next(), ModelTab::All);
        assert_eq!(ModelTab::All.label(), "ALL");
        assert_eq!(ModelTab::Canonical.label(), "CANONICAL");
    }

    #[test]
    fn collect_rows_all_tab_includes_every_model() {
        let reg = registry();
        let total: usize = reg.providers().map(|p| p.models.len()).sum();
        let rows = collect_rows(&reg, &ModelRoles::default(), ModelTab::All);
        assert_eq!(rows.len(), total);
    }

    #[test]
    fn collect_rows_canonical_only_includes_aliased_models() {
        let reg = registry();
        let rows = collect_rows(&reg, &ModelRoles::default(), ModelTab::Canonical);
        // Every row must have an alias.
        for r in &rows {
            assert!(r.alias.is_some(), "{} missing alias", r.full_id());
        }
        // Canonical is a non-empty subset of All.
        let all = collect_rows(&reg, &ModelRoles::default(), ModelTab::All);
        assert!(!rows.is_empty());
        assert!(rows.len() <= all.len());
    }

    #[test]
    fn badges_show_up_for_role_assignments_via_full_id_alias_or_model_id() {
        let reg = registry();
        // Pick the first model with an alias so we can probe all three
        // matching paths.
        let target = reg
            .providers()
            .flat_map(|p| {
                p.models.iter().map(move |m| {
                    (
                        p.name.clone(),
                        m.id.clone(),
                        m.alias.clone(),
                    )
                })
            })
            .find(|(_, _, alias)| alias.is_some())
            .expect("at least one canonical model in defaults");
        let (provider, model, alias) = target;
        let alias = alias.unwrap();

        // default -> full id, smol -> alias, plan -> bare model id
        let mut roles = ModelRoles::default();
        roles.default = Some(format!("{}/{}", provider, model));
        roles.smol = Some(alias.clone());
        roles.plan = Some(model.clone());

        let rows = collect_rows(&reg, &roles, ModelTab::All);
        let row = rows
            .iter()
            .find(|r| r.provider == provider && r.model == model)
            .expect("row present");
        assert!(row.badges.contains(&'d'), "expected default badge");
        assert!(row.badges.contains(&'s'), "expected smol badge");
        assert!(row.badges.contains(&'p'), "expected plan badge");
        assert!(!row.badges.contains(&'S'));
        assert!(!row.badges.contains(&'c'));
    }

    #[test]
    fn label_format_includes_alias_and_badges() {
        let row = ModelRow {
            provider: "openai".into(),
            model: "gpt-4o".into(),
            alias: Some("gpt-4o".into()),
            badges: vec!['d', 'p'],
        };
        let label = row.label();
        assert!(label.contains("openai/gpt-4o"));
        assert!(label.contains("[gpt-4o]"));
        assert!(label.contains("(dp)"));
    }

    #[test]
    fn label_omits_badges_when_empty() {
        let row = ModelRow {
            provider: "anthropic".into(),
            model: "claude-3".into(),
            alias: None,
            badges: vec![],
        };
        let label = row.label();
        assert!(!label.contains("("));
        assert!(!label.contains("["));
    }

    #[test]
    fn context_menu_choices_cover_five_roles_plus_temporary() {
        assert_eq!(ContextMenuChoice::ALL.len(), 6);
        let labels: Vec<&str> = ContextMenuChoice::ALL.iter().map(|c| c.label()).collect();
        for want in &["default", "smol", "slow", "plan", "commit", "temporary"] {
            assert!(labels.contains(want), "missing {want}");
        }
        assert!(ContextMenuChoice::Temporary.as_role().is_none());
        assert_eq!(ContextMenuChoice::Default.as_role(), Some(Role::Default));
    }

    #[test]
    fn picker_for_uses_full_id_as_value() {
        let reg = registry();
        let p = picker_for(&reg, &ModelRoles::default(), ModelTab::Canonical);
        assert!(p.items_len() > 0);
        let ranked = p.ranked();
        // Value matches "provider/model".
        for (_, item) in ranked {
            assert!(item.value.contains('/'), "value not full id: {}", item.value);
        }
    }
}
