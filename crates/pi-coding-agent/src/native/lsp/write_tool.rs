//! `write` tool wrapper that hooks LSP format-on-write and
//! diagnostics-on-write side-effects (RFD 0001).
//!
//! This wraps [`pi_tools::write::WriteTool`] verbatim — same
//! `spec().name == "write"` so registering this *after* the inner tool
//! transparently overrides it. The hook is best-effort: if anything in
//! the LSP path fails (server not on PATH, language disabled, edits
//! out-of-bounds, …) we leave the file as the inner write left it and
//! never flip `is_error`.
//!
//! Flow on a successful inner write:
//!
//! 1. Resolve the path the same way the inner tool did
//!    (`pi_tools::resolve_path`) and dispatch to a language. Unknown
//!    extensions / disabled languages → return inner result unchanged.
//! 2. Lazy-init an [`super::engine::LspEngine`] (mirrors `LspTool`'s
//!    `OnceCell` pattern) so the first write spawns the server and
//!    later writes reuse it.
//! 3. If `format_on_write`: pull `textDocument/formatting`, sort
//!    edits descending by `(line, character)`, apply UTF-8-aware,
//!    write back to disk, and add `display.formatted = true`.
//! 4. If `diagnostics_on_write`: pull `textDocument/diagnostic`, accept
//!    either a raw `Diagnostic[]` or the LSP 3.17 `{kind, items}`
//!    shape, and attach `display.diagnostics`.

use async_trait::async_trait;
use pi_ai::{ToolResult, ToolSpec};
use pi_tools::{resolve_path, Tool, ToolContext, ToolError};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::OnceCell;

use super::config::LspConfig;
use super::engine::LspEngine;

pub struct LspWriteTool {
    inner: pi_tools::write::WriteTool,
    config: LspConfig,
    engine: OnceCell<Arc<LspEngine>>,
}

impl LspWriteTool {
    pub fn new(cfg: LspConfig) -> Self {
        Self {
            inner: pi_tools::write::WriteTool,
            config: cfg,
            engine: OnceCell::new(),
        }
    }

    async fn engine_for(&self, ctx: &ToolContext) -> &Arc<LspEngine> {
        self.engine
            .get_or_init(|| async {
                Arc::new(LspEngine::new(self.config.clone(), ctx.cwd.clone()))
            })
            .await
    }
}

#[async_trait]
impl Tool for LspWriteTool {
    fn spec(&self) -> ToolSpec {
        self.inner.spec()
    }

    fn read_only(&self) -> bool {
        false
    }

    async fn invoke(
        &self,
        ctx: &ToolContext,
        call_id: &str,
        input: Value,
    ) -> Result<ToolResult, ToolError> {
        // 1. Inner write first. If that errors out (or returns
        //    `is_error: true`), surface it verbatim — no hook fires.
        let mut result = self.inner.invoke(ctx, call_id, input.clone()).await?;
        if result.is_error || !self.config.enabled {
            return Ok(result);
        }

        // 2. Resolve the path & language. Skip silently for unknown /
        //    disabled languages; the inner result is the truth.
        let Some(path_str) = input.get("path").and_then(|v| v.as_str()) else {
            return Ok(result);
        };
        let path = resolve_path(ctx, path_str);
        let Some(language) = LspEngine::language_for(&path) else {
            return Ok(result);
        };
        if !self.config.is_language_enabled(language) {
            return Ok(result);
        }

        // 3. Lazy engine. Cloned out of the OnceCell so we don't hold a
        //    borrow across the awaits below.
        let engine = self.engine_for(ctx).await.clone();

        // Mutable handle on the display object so we can splice in
        // hook-side fields. If `display` is somehow not an object we
        // bail — preserves the inner result.
        let display = match result.display.as_mut().and_then(|v| v.as_object_mut()) {
            Some(obj) => obj,
            None => return Ok(result),
        };

        // 4. format_on_write. Errors are swallowed; the file on disk is
        //    whatever inner.invoke just wrote.
        if self.config.format_on_write {
            if let Ok(reply) = engine.formatting(&path).await {
                if let Some(edits) = reply.as_array() {
                    if !edits.is_empty() {
                        if let Ok(original) = tokio::fs::read_to_string(&path).await {
                            if let Some(formatted) = apply_text_edits(&original, edits) {
                                if tokio::fs::write(&path, &formatted).await.is_ok() {
                                    display.insert("formatted".into(), json!(true));
                                    display
                                        .insert("bytes".into(), json!(formatted.len()));
                                }
                            }
                        }
                    }
                }
            }
        }

        // 5. diagnostics_on_write. Accept both the LSP 3.17 pull-mode
        //    `{ kind, items }` shape and a bare `Diagnostic[]`.
        if self.config.diagnostics_on_write {
            if let Ok(reply) = engine.diagnostics(&path).await {
                let items = if reply.is_array() {
                    Some(reply)
                } else if let Some(items) = reply.get("items") {
                    Some(items.clone())
                } else {
                    None
                };
                if let Some(items) = items {
                    display.insert("diagnostics".into(), items);
                }
            }
        }

        Ok(result)
    }
}

/// Apply an LSP `TextEdit[]` to `text`. Returns the rewritten string,
/// or `None` if any edit's range is out of bounds (the file should not
/// be rewritten in that case — fail closed).
///
/// Sort order: descending by `(start.line, start.character)` so each
/// later edit's offsets stay valid as we rewrite earlier (in-file)
/// regions. LSP semantics: lines are split on `\n` (a trailing newline
/// produces an empty final line), and `character` is a 0-indexed count
/// of UTF-16 code units in the spec — but most servers and most files
/// are ASCII-or-BMP, and the rest of the engine uses Unicode-scalar
/// counts; we follow that convention here. `position == EOF`
/// (`line == line_count, character == 0`) and one-past-end on a line
/// are both legal.
pub(crate) fn apply_text_edits(text: &str, edits: &[Value]) -> Option<String> {
    if edits.is_empty() {
        return Some(text.to_string());
    }

    // Line-start byte offsets, plus a sentinel for EOF. `lines.len()`
    // is `line_count + 1` so `lines[line_count]` is `text.len()`.
    let mut lines: Vec<usize> = Vec::with_capacity(text.len() / 32 + 2);
    lines.push(0);
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            lines.push(i + 1);
        }
    }
    // Treat one-past-last-newline as a synthetic empty trailing line
    // (LSP's EOF position). If the file already ends in `\n`, that line
    // already exists at `lines.last()`; otherwise we add a synthetic
    // one-past-end entry.
    if *lines.last().unwrap() != text.len() {
        // The last partial line ends at text.len(); EOF position is
        // line == lines.len() (the next index), char == 0.
        // We don't push it; we'll handle EOF as a special case below.
    }
    let line_count = if text.is_empty() {
        1
    } else if text.ends_with('\n') {
        lines.len() // includes the synthetic empty final line
    } else {
        lines.len()
    };

    // (line, character) → byte offset. None on out-of-bounds.
    let to_byte = |line: usize, character: usize| -> Option<usize> {
        // EOF position: line == line_count, char == 0.
        if line == line_count && character == 0 {
            return Some(text.len());
        }
        if line >= line_count {
            return None;
        }
        let line_start = lines[line];
        // Find the end of this line (exclusive of '\n').
        let line_end = if line + 1 < lines.len() {
            // -1 strips the '\n' byte that began the next line.
            lines[line + 1] - 1
        } else {
            text.len()
        };
        let line_slice = &text[line_start..line_end];
        // Advance `character` Unicode scalars; one-past-end is legal.
        let mut chars = line_slice.char_indices();
        let mut taken = 0usize;
        while taken < character {
            match chars.next() {
                Some(_) => taken += 1,
                None => {
                    // Allow exactly character == scalar_count (one
                    // past end of line) but not beyond.
                    if taken == character {
                        return Some(line_end);
                    }
                    return None;
                }
            }
        }
        // taken == character. Where are we?
        match chars.next() {
            Some((off, _)) => Some(line_start + off),
            None => Some(line_end),
        }
    };

    // Pre-resolve all (start, end, new_text) into byte offsets so we
    // can validate before mutating. Bail on the first malformed edit.
    struct Resolved<'a> {
        start: usize,
        end: usize,
        new_text: &'a str,
    }
    let mut resolved: Vec<Resolved<'_>> = Vec::with_capacity(edits.len());
    for edit in edits {
        let range = edit.get("range")?;
        let s = range.get("start")?;
        let e = range.get("end")?;
        let sl = s.get("line")?.as_u64()? as usize;
        let sc = s.get("character")?.as_u64()? as usize;
        let el = e.get("line")?.as_u64()? as usize;
        let ec = e.get("character")?.as_u64()? as usize;
        let new_text = edit.get("newText").and_then(|v| v.as_str()).unwrap_or("");
        let start = to_byte(sl, sc)?;
        let end = to_byte(el, ec)?;
        if start > end {
            return None;
        }
        resolved.push(Resolved { start, end, new_text });
    }

    // Sort descending by start (ties: descending by end too — doesn't
    // matter much, but keeps ordering deterministic).
    resolved.sort_by(|a, b| b.start.cmp(&a.start).then(b.end.cmp(&a.end)));

    let mut out = text.to_string();
    let mut last_start = usize::MAX;
    for r in &resolved {
        // Reject overlapping edits — LSP says the result is undefined
        // and we'd rather fail closed.
        if r.end > last_start {
            return None;
        }
        out.replace_range(r.start..r.end, r.new_text);
        last_start = r.start;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn edit(sl: u64, sc: u64, el: u64, ec: u64, new_text: &str) -> Value {
        json!({
            "range": {
                "start": { "line": sl, "character": sc },
                "end":   { "line": el, "character": ec },
            },
            "newText": new_text,
        })
    }

    #[test]
    fn empty_edits_is_a_noop() {
        let original = "hello\nworld\n";
        let out = apply_text_edits(original, &[]).unwrap();
        assert_eq!(out, original);
    }

    #[test]
    fn single_line_replace_swaps_the_chosen_run() {
        // Replace "world" with "rust" on line 1.
        let original = "hello\nworld\n";
        let edits = vec![edit(1, 0, 1, 5, "rust")];
        let out = apply_text_edits(original, &edits).unwrap();
        assert_eq!(out, "hello\nrust\n");
    }

    #[test]
    fn two_edits_apply_in_descending_order() {
        // Even though we list them ascending, the function must sort
        // descending by start so the first edit's offsets stay valid.
        let original = "aaa\nbbb\nccc\n";
        let edits = vec![
            edit(0, 0, 0, 3, "AAA"), // line 0
            edit(2, 0, 2, 3, "CCC"), // line 2
        ];
        let out = apply_text_edits(original, &edits).unwrap();
        assert_eq!(out, "AAA\nbbb\nCCC\n");
    }

    #[test]
    fn out_of_bounds_line_returns_none() {
        let original = "hello\n";
        // Line 99 doesn't exist.
        let edits = vec![edit(99, 0, 99, 0, "boom")];
        assert!(apply_text_edits(original, &edits).is_none());
    }

    #[test]
    fn multibyte_character_offsets_are_unicode_scalar_counts() {
        // "héllo" — `é` is 2 bytes in UTF-8 but 1 scalar. Replacing
        // chars 1..2 should rewrite just the `é`.
        let original = "héllo\n";
        let edits = vec![edit(0, 1, 0, 2, "E")];
        let out = apply_text_edits(original, &edits).unwrap();
        assert_eq!(out, "Hllo\n".replace('H', "hE"));
        // …i.e. "hEllo\n":
        assert_eq!(out, "hEllo\n");
    }

    #[test]
    fn eof_position_is_valid_for_appending() {
        // Append "!" at the very end of a no-trailing-newline file.
        let original = "abc";
        // Line 0 has 3 scalars; one-past-end on line 0 is char 3.
        let edits = vec![edit(0, 3, 0, 3, "!")];
        let out = apply_text_edits(original, &edits).unwrap();
        assert_eq!(out, "abc!");
    }

    #[test]
    fn overlapping_edits_are_rejected() {
        let original = "abcdef";
        let edits = vec![edit(0, 0, 0, 4, "X"), edit(0, 2, 0, 6, "Y")];
        assert!(apply_text_edits(original, &edits).is_none());
    }

    #[test]
    fn spec_forwards_to_inner_write_tool_name() {
        let tool = LspWriteTool::new(LspConfig::default());
        let spec = tool.spec();
        assert_eq!(spec.name, "write");
        assert!(!tool.read_only());
    }

    #[tokio::test]
    async fn disabled_config_passes_through_unchanged() {
        // Master off → wrapper must not touch the inner result.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hello.rs");
        let tool = LspWriteTool::new(LspConfig::default());
        let ctx = ToolContext {
            cwd: dir.path().to_path_buf(),
            ..ToolContext::default()
        };
        let res = tool
            .invoke(
                &ctx,
                "c1",
                json!({
                    "path": path.to_string_lossy(),
                    "content": "fn main() {}\n",
                }),
            )
            .await
            .unwrap();
        assert!(!res.is_error);
        let display = res.display.unwrap();
        assert!(display.get("formatted").is_none());
        assert!(display.get("diagnostics").is_none());
        // File was still written by inner tool.
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "fn main() {}\n");
    }
}
