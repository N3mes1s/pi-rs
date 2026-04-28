# RFD 0007 — Per-language LSP formatting options

- **Status:** Implemented
- **Author:** pi-rs maintainers
- **Created:** 2026-04-27
- **Implemented:** c008b44

## Summary

Add a `format_options` field to `LspLanguageSettings` (and the runtime
mirror `LanguageConfig`) so users can override the four `FormattingOptions`
constants the engine sends with `textDocument/formatting`: `tab_size`,
`insert_spaces`, `trim_trailing_whitespace`, `insert_final_newline`,
`trim_final_newlines`. Defaults stay exactly as they are today
(`tab_size=4`, `insert_spaces=true`, all three trim/newline flags
`true`) so this is purely additive.

This is one of the items deferred from RFD 0001's "Out of scope" list
("`tabSize` etc. could come from `LspConfig` per-language overrides
later") and called out explicitly in RFD 0002 P1 0007.

## Background

Today `LspEngine::formatting()` sends a hardcoded `FormattingOptions`
block (see `crates/pi-coding-agent/src/native/lsp/engine.rs` around
the `formatting` method that was added by RFD 0001). That's fine for
Rust + rustfmt, but Python via `pylsp`, JS/TS via `tsserver`, Go via
`gopls` all expect different defaults. A two-line per-language
override unblocks every one of them without touching the transport.

References (LSP 3.17):
* `FormattingOptions` shape: spec §3.17.13.
* All five option keys are documented as optional except `tabSize`
  and `insertSpaces`, which are `required`.

## Proposal

### 1. Settings: serde mirror in `pi-agent-core`

`crates/pi-agent-core/src/settings.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct LspLanguageSettings {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub command: Option<Vec<String>>,
    /// Override the `FormattingOptions` block sent with
    /// `textDocument/formatting`. Missing fields inherit the
    /// hardcoded engine defaults (tab_size=4, insert_spaces=true,
    /// trim/newline=true). RFD 0007.
    #[serde(default, skip_serializing_if = "FormattingOptions::is_empty")]
    pub format_options: FormattingOptions,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct FormattingOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_size: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insert_spaces: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trim_trailing_whitespace: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insert_final_newline: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trim_final_newlines: Option<bool>,
}

impl FormattingOptions {
    pub fn is_empty(&self) -> bool {
        self.tab_size.is_none()
            && self.insert_spaces.is_none()
            && self.trim_trailing_whitespace.is_none()
            && self.insert_final_newline.is_none()
            && self.trim_final_newlines.is_none()
    }
}
```

### 2. Runtime mirror in `pi-coding-agent`

`crates/pi-coding-agent/src/native/lsp/config.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct LanguageConfig {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub command: Option<Vec<String>>,
    /// Per-language formatting overrides. RFD 0007.
    #[serde(default)]
    pub format_options: FormattingOptions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct FormattingOptions {
    pub tab_size:                 Option<u32>,
    pub insert_spaces:             Option<bool>,
    pub trim_trailing_whitespace:  Option<bool>,
    pub insert_final_newline:      Option<bool>,
    pub trim_final_newlines:       Option<bool>,
}
```

The existing `From<&LspSettings>` impl in `config.rs` is extended to
copy `format_options` field-by-field (use `.clone()` since the source
is by-ref and the target owns).

### 3. Engine: read overrides in `formatting()`

```rust
// engine.rs::formatting()
pub async fn formatting(&self, file: &Path) -> Result<Value, EngineError> {
    let client = self.prepare(file).await?;
    let language = Self::language_for(file)
        .map(|s| s.to_string())
        .unwrap_or_else(|| "plaintext".to_string());
    let options = self.formatting_options_for(&language);
    let params = json!({
        "textDocument": { "uri": Self::file_uri(file) },
        "options": options,
    });
    Ok(client.send_request("textDocument/formatting", params).await?)
}

/// Pure: build a `FormattingOptions` JSON object using the
/// per-language override falling back to the engine defaults.
/// Called from `formatting()` and tested directly.
fn formatting_options_for(&self, language: &str) -> Value {
    let cfg = self.config
        .languages.get(language)
        .map(|l| &l.format_options);
    let tab_size      = cfg.and_then(|f| f.tab_size).unwrap_or(4);
    let insert_spaces = cfg.and_then(|f| f.insert_spaces).unwrap_or(true);
    let trim_trail    = cfg.and_then(|f| f.trim_trailing_whitespace).unwrap_or(true);
    let insert_final  = cfg.and_then(|f| f.insert_final_newline).unwrap_or(true);
    let trim_final    = cfg.and_then(|f| f.trim_final_newlines).unwrap_or(true);
    json!({
        "tabSize":                 tab_size,
        "insertSpaces":            insert_spaces,
        "trimTrailingWhitespace":  trim_trail,
        "insertFinalNewline":      insert_final,
        "trimFinalNewlines":       trim_final,
    })
}
```

The wire keys stay camelCase (LSP spec); the Rust fields stay
snake_case via serde rename-on-the-fly inside the json! macro — no
`#[serde(rename = …)]` needed because the field names never round-trip
through JSON.

## Test plan

1. **`config.rs` unit test**: deserialise a `LanguageConfig` JSON with
   `format_options: { "tab_size": 2, "insert_spaces": false }`,
   assert the runtime struct round-trips.
2. **`config.rs` mirror test**: build a `LspSettings` with a
   `format_options` block, convert via `From<&LspSettings>`, assert
   equality.
3. **`engine.rs::formatting_options_for` unit test**: with no
   per-language config, all five wire keys come back at their
   hardcoded defaults. With a partial override
   (`tab_size=2`, `insert_final_newline=false`), the two overridden
   keys appear; the other three stay at their defaults.
4. **`settings_save.rs`-style serde round trip** (in
   `pi-agent-core/tests/settings_save.rs`): write a settings.json
   containing a `format_options` block, reload, assert equality.

No integration test against rust-analyzer needed — the existing
`tests/lsp_write_tool_real_rust_analyzer.rs` already exercises the
default-values path; this RFD is about *overrides*, which is a unit
concern.

## Out of scope

- **Per-file overrides** (e.g. read `.editorconfig`). Tracked
  separately; needs a parser, not a config schema change.
- **Range formatting** (`textDocument/rangeFormatting`). Not on the
  pi-rs feature list at all yet.
- **Format-on-paste / format-on-type**. RFD 0002 P1 0005.
- **Surfacing rustfmt's `Cargo.toml` toolchain pin** — that goes
  into the engine command override, not formatting options.

## Open questions

- **Should the engine inherit a project-level
  `.editorconfig`-derived default if no `format_options` block is
  set?** Lean no for v1 (config layering is hard); document the
  manual `lsp.languages.<lang>.format_options` path.
- **Should we expose the same overrides for `format-on-type` /
  range-formatting once those land?** Yes — the same `FormattingOptions`
  struct is the right place. (Decided. Listed for the record.)
