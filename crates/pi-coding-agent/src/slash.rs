use std::collections::BTreeMap;

use crate::extensions::ExtensionCommandManifest;
use crate::prompts::PromptRegistry;

/// A slash command: built-in or template-derived.
#[derive(Debug, Clone)]
pub struct SlashCommand {
    pub name: String,
    pub description: String,
    pub kind: SlashKind,
}

#[derive(Debug, Clone)]
pub enum SlashKind {
    /// Built-in command — handled inline by the mode loop.
    Builtin,
    /// `/<name>` populated from a prompt template — the template body is
    /// rendered (with `{{arg}}` replaced by trailing arguments) and sent
    /// to the agent as a user prompt.
    Template { body: String },
    /// Command exported by a loaded extension. `extension_index` is the
    /// position of the owning [`LoadedExtension`] in `Startup::extensions`.
    Extension {
        extension_index: usize,
        command_name: String,
    },
}

#[derive(Default, Debug, Clone)]
pub struct SlashRegistry {
    inner: BTreeMap<String, SlashCommand>,
}

impl SlashRegistry {
    pub fn new() -> Self {
        let mut me = Self::default();
        me.register_builtins();
        me
    }

    fn register_builtins(&mut self) {
        for (name, desc) in [
            ("login", "Authenticate via OAuth"),
            ("logout", "Sign out from a provider"),
            ("model", "Switch model"),
            ("scoped-models", "Toggle the per-message model picker"),
            ("settings", "Edit settings.json"),
            ("resume", "Browse previous sessions"),
            ("tree", "Navigate the current session tree"),
            ("fork", "Branch from a user message"),
            ("clone", "Duplicate the active branch"),
            ("compact", "Summarise older messages"),
            ("export", "Export the current session as HTML"),
            ("share", "Upload session as a GitHub gist"),
            ("hotkeys", "Show all keyboard shortcuts"),
            ("help", "Show help"),
            ("quit", "Exit pi"),
            (
                "autoresearch",
                "Autonomous experiment loop (off | clear | export | <text>)",
            ),
        ] {
            self.inner.insert(
                name.into(),
                SlashCommand {
                    name: name.into(),
                    description: desc.into(),
                    kind: SlashKind::Builtin,
                },
            );
        }
    }

    /// Register slash commands exported by extensions.
    ///
    /// `items` is a slice of `(extension_index, manifest)` pairs where
    /// `extension_index` matches the position in `Startup::extensions`.
    /// Existing names (builtins, templates) are **not** overwritten so that
    /// built-in commands always take precedence.
    pub fn register_extension_commands(
        &mut self,
        items: &[(usize, &ExtensionCommandManifest)],
    ) {
        for (ext_idx, cmd) in items {
            if self.inner.contains_key(&cmd.name) {
                continue;
            }
            self.inner.insert(
                cmd.name.clone(),
                SlashCommand {
                    name: cmd.name.clone(),
                    description: cmd.description.clone(),
                    kind: SlashKind::Extension {
                        extension_index: *ext_idx,
                        command_name: cmd.name.clone(),
                    },
                },
            );
        }
    }

    pub fn register_templates(&mut self, prompts: &PromptRegistry) {
        for name in prompts.names() {
            if self.inner.contains_key(&name) {
                continue;
            }
            if let Some(t) = prompts.get(&name) {
                let desc = t.body.lines().next().unwrap_or("").to_string();
                self.inner.insert(
                    name.clone(),
                    SlashCommand {
                        name,
                        description: desc,
                        kind: SlashKind::Template {
                            body: t.body.clone(),
                        },
                    },
                );
            }
        }
    }

    pub fn get(&self, name: &str) -> Option<&SlashCommand> {
        self.inner.get(name)
    }

    pub fn names(&self) -> Vec<String> {
        self.inner.keys().cloned().collect()
    }
}

/// Try to interpret `input` as a slash command.
/// Returns (name, args) or None.
pub fn parse(input: &str) -> Option<(String, String)> {
    let trimmed = input.trim_start();
    let rest = trimmed.strip_prefix('/')?;
    let mut parts = rest.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("").to_string();
    let args = parts.next().unwrap_or("").to_string();
    if name.is_empty() {
        return None;
    }
    Some((name, args))
}

/// Render a template, replacing `{{args}}` and `{{ARGS}}` with the trailing
/// argument string. Mirrors upstream pi's interpolation.
pub fn render_template(body: &str, args: &str) -> String {
    body.replace("{{args}}", args).replace("{{ARGS}}", args)
}
