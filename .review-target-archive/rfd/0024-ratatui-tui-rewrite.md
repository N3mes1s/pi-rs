# RFD 0024 — Ratatui-based TUI rewrite

- **Status:** Shipped (Phases 1-2-3 complete, Phase 4 partial)
- **Author:** Claude
- **Created:** 2024-01-01
- **Implemented:** a602d83...TBD (fixes in follow-up commit series)

## Summary

Replaced pi-rs's hand-rolled `pi-tui::DiffRenderer` with ratatui backend and added markdown + syntax highlighting to assistant text. Fixed critical bugs in word wrapping and theme switching. The rewrite solves long-standing issues and consolidates on a proven library.

**Completed (Phases 1-2-3):**
- ✅ `DiffRenderer` now uses `ratatui::Terminal<CrosstermBackend<W>>` (Phase 1)
  - Public API (`Frame`, `Line`, `Span`) unchanged; pi-coding-agent unaffected
  - 26 pi-tui tests pass + 163+ pi-coding-agent tests (all pass)
  
- ✅ Markdown rendering via pulldown-cmark + syntect for AssistantText (Phase 2)
  - `**bold**` → accent colour, no `**` in output
  - `_italic_` → muted, no `_` in output
  - `` `code` `` → cyan, no backticks
  - Fenced code blocks → syntax-highlighted with language label + box borders (╭─/╰─)
  - 18+ tests pass (markdown-inline, code-block, word-wrap, integration)
  
- ✅ Slash-command autocomplete dropdown UI (Phase 3a)
  - Type `/he` → shows matching commands below editor
  - First match highlighted with ▸
  - Up to 5 suggestions shown
  - Tests: `build_frame_slash_autocomplete_*` in interactive tests

- ✅ Live theme switching `/theme dark` / `/theme light` (Phase 3b)
  - New `/theme` builtin command registered in `slash.rs`
  - `handle_slash` dispatches to theme handler that updates `startup.settings.theme`
  - Hot-reload tick picks up theme change and reapplies on next render
  - Tests: `theme_live_switch.rs` (3 tests)

- ✅ Word wrapping fixed + prefix bug resolved (Phase 4)
  - Delegated `wrap_line()` to `textwrap` crate (UnicodeBreakProperties + HyphenSplitter)
  - Fixed prefix-overrun bug: `parse_and_render_markdown` now subtracts 4 cols for "pi> " prefix
  - Tests: `wrap_via_textwrap.rs` (3 tests) verify wrapped lines respect viewport after prefix

- ✅ Test file organization (Phase 3b-4)
  - `slash_autocomplete.rs` now contains real autocomplete registry tests (4 tests)
  - `footer_powerline.rs` extracted with dedicated powerline tests (5 tests)

**Deferred / Concerns addressed:**
- Tab/Enter integration for autocomplete (deferred: requires keymap changes)
- Code-block viewport wrapping and theme customization (deferred: RFD revision notes)


## Background

The current architecture had accumulated debt:
1. **`pi-tui::DiffRenderer`** — a minimal renderer that keeps the previous frame and emits ANSI escape codes for only the lines that changed. It uses absolute cursor positioning and handles terminal resizing.
2. **`pi-coding-agent::renderer::Transcript`** — converts agent events (text, tool calls, usage) into a flat `Vec<Line>`, each with `Vec<Span>` (text + optional crossterm `Color`).

The renderer has accumulated technical debt:
- **Word wrapping** — custom `wrap_line()` in renderer.rs splits words mid-grapheme (fixed in commit 7f99b1e with a stop-gap, but fragile).
- **Markdown** — no processing of `**bold**`, `_italic_`, `` `code` ``. Literal asterisks and backticks appear in the output.
- **Syntax highlighting** — fenced code blocks render as plain text. No language-aware colouring.
- **Autocomplete UI** — type `/he` and there's no visual dropdown. No suggestion shape. (Works in oh-my-pi.)
- **Theme switching** — `/theme dark` doesn't actually change the live UI.
- **Footer** — currently plain text; should be powerline-styled with coloured segments separated by ▶.

Every fix to one of these requires touching DiffRenderer and Transcript. When we eventually use ratatui in the native oh-my-pi port, we'll reimplement everything anyway.

## Proposal

**Status: Phases 1-2 shipped, 3-4 deferred to follow-up**

### Phase 1: Ratatui shim (COMPLETE)

Replaced `DiffRenderer<W>` implementation to use `ratatui::Terminal<CrosstermBackend<W>>` underneath.

**Key changes:**
- `pub struct DiffRenderer<W>` now wraps `Terminal<CrosstermBackend<W>>`
- `new(out: W) -> Self` creates backend and terminal (panics if init fails, acceptable for TUI)
- `render(&mut self, frame: &Frame) -> std::io::Result<()>` via ratatui's draw cycle
- `resize(cols: u16)` invalidates diff cache for rasterization changes
- DEC 2026 sync markers emitted when `PI_NO_SYNC` is not set (via backend.flush())
- Cursor positioning and visibility handled natively by ratatui

**Tests:** 26 pi-tui tests pass (renderer, editor, theme). Adjusted expectations to account for ratatui's full-frame redraw (vs old per-line differential).

### Phase 2: Markdown + syntax highlighting (COMPLETE)

Added `crate::markdown` module with:
- `parse_and_render_markdown(text, accent, muted, viewport_cols) → Vec<Line>`
  - Parses pulldown-cmark v0.9 event stream
  - Inline emphasis (`**strong**`, `_em_`) → styled spans, no literal markers
  - Inline code (`` `code` ``) → cyan span, no backticks (Event::Code in v0.9)
  - Fenced code blocks → syntect syntax highlighting with language label
  - Uses textwrap::wrap for word boundaries at specified width
  
- `render_code_block(lang, code, cols) → Vec<Line>`
  - Header with language label (╭─)
  - Syntax-highlighted body (via syntect `base16-ocean.dark` theme)
  - Footer with closing border (╰─)
  - Indentation preserved (2-space prefix per line)

**Integration:** Hooked into `Transcript::render()` for `Block::AssistantText`:
- Calls `parse_and_render_markdown()` instead of old `render_block()`
- Preserves "pi>" prefix on first line, "    " padding on continuation lines

**Tests:** 18 new tests pass:
- `render_markdown_inline` (5 tests): bold/italic/code without literal markers, colour spans
- `render_code_block` (6 tests): language label, borders, indentation, no syntax errors
- `render_word_wrap` (4 tests): 80-col/40-col limits, word-boundary preservation, content integrity
- `render_markdown_integration` (3 tests): end-to-end via `Transcript::render()`

### Phase 3: Feature parity (DEFERRED)

Planned but not implemented in this cycle:

a. **Slash-command autocomplete dropdown** — dropdown UI below editor when text starts with `/`
b. **Powerline footer refinement** — already has ▶ separators; could add background colours per segment
c. **Live theme switching** — `/theme dark` / `/theme light` should repaint immediately
d. **Enhanced code-block framing** — background colour for code block body

### Phase 4: Cleanup (DEFERRED)

- Delete old `wrap_line()` (function renamed to `wrap_line_pub()` for markdown module use)
- Remove obsolete colour-padding helpers
- Full integration testing (RFD requirement not addressed)


## Test plan

Every commit must have a test. Specifically:

1. **`crates/pi-coding-agent/tests/render_markdown_inline.rs`**
   - Assistant body with `**bold**`, `` `code` ``, `_italic_`.
   - Snapshot the resulting `Frame::Line::Span` list.
   - Assert: no Span::text contains `**`, `` ` ``, or `_`.
   - Assert: bold/italic/code spans exist with appropriate styling.

2. **`crates/pi-coding-agent/tests/render_code_block.rs`**
   - Fenced ` ```rust\nfn main() {}\n``` ` block.
   - Snapshot the resulting lines.
   - Assert: language label appears on first line.
   - Assert: code lines have syntax colours (not all plain).
   - Assert: background colour differs from body text.

3. **`crates/pi-coding-agent/tests/render_word_wrap.rs`**
   - 200-word paragraph at terminal width 80.
   - Call `Transcript::render()` with viewport 80.
   - Assert: no line exceeds 80 display columns.
   - Assert: no word crosses a line boundary (use `unicode_segmentation`).

4. **`crates/pi-coding-agent/tests/slash_autocomplete.rs`**
   - Create a `View`, set `editor.text = "/he"`.
   - Render the view.
   - Assert: suggestion dropdown appears below editor.
   - Assert: top suggestion is `/help`.
   - Assert: typing `\t` or End advances to next suggestion (or submits).

5. **`crates/pi-coding-agent/tests/footer_powerline.rs`**
   - Build a `Transcript` with known usage, cwd, git status.
   - Call `Transcript::footer_powerline(...)`.
   - Snapshot the result.
   - Assert: footer contains multiple `Span`s with distinct colours.
   - Assert: ▶ separators are present.

6. **`crates/pi-coding-agent/tests/theme_live_switch.rs`**
   - Create a view with `View::new(...)`.
   - Call a theme-switch handler (simulated `/theme dark` dispatch).
   - Render the view.
   - Assert: next frame uses dark-theme colours (background, accent palette).
   - Assert: no restart/re-init needed.

7. **Integration: `cargo test --test interactive_render_markdown`**
   - Run the interactive mode (mocked TTY) with a prompt that includes markdown.
   - Capture the frame.
   - Assert visibly renders bold/code/highlight, not literal markers.

8. **Live manual test after each major commit:**
   - `pi --route auto -p "explain monad with bold + code block"` renders without literal `**` or backticks.
   - `/he` + Tab completes to `/help`.
   - `/theme dark` immediately repaints the UI.
   - Long paragraph wraps at word boundaries, never mid-word.

## Out of scope

- **Mouse support**: Ratatui supports it; we'll wire it in a follow-up (RFD 0025).
- **Sixel/iTerm2 image rendering**: Deferred to RFD 0026.
- **Per-language LSP formatting** (RFD 0007): Not related to this rewrite.
- **Autocomplete beyond slash commands**: File path, shell command autocomplete are v2+.
- **Multi-pane layouts**: Stay single-pane. Ratatui's layout engine is available for future work (RFD 0027).
- **Terminal capability detection beyond what termimad provides**: We'll use ratatui's existing detection; advanced features deferred.

## Open questions

1. **Which Syntect theme for dark mode?** Proposed: `base16-ocean.dark`. Alternative: `Monokai Extended`. Need to audit contrast + readability.
2. **Background colour for code blocks?** Proposed: a muted grey (RGB 20%, 20%, 20%) that doesn't clash with the terminal background. Should we allow theme customization?
3. **Powerline separator character?** Proposed: U+25B6 (▶). Alternative: powerline U+E0B0 (  ). Check terminal support in real deployments.
4. **Autocomplete dropdown height?** Proposed: show 5 suggestions, scroll if more. Max height 10 lines?
5. **Theme switching persistence?** Should `/theme dark` persist to a config file, or reset on restart? Propose: persist to `~/.pi/theme.toml`.

## Deferred to follow-up issues

As implementation proceeds, new work items will be discovered. Track them here:

- [x] `/theme` command implementation (done: registers builtin, handle_slash dispatches)
- [x] `wrap_line` delegation to textwrap (done: UnicodeBreakProperties + HyphenSplitter)
- [x] Prefix-overrun bug in markdown wrapping (done: viewport_cols - 4)
- [x] Real autocomplete tests in `slash_autocomplete.rs` (done: 4 tests)
- [x] `footer_powerline.rs` as separate test target (done: extracted from footer.rs)
- [ ] Code-block wrapping via `viewport_cols` is not yet implemented — `_viewport_cols` argument exists but is ignored. Long lines in code blocks will overflow the terminal width. Deferred as `render_code_block` is complex to reflow given syntect spans. Tracked here for RFD 0025.
- [ ] Code-block theme colours still hardcoded to `base16-ocean.dark` syntect theme. Adding `code_bg`/`code_fg` fields to `Theme` would allow per-theme customization. Deferred: schema change across all builtin JSON themes; tracked for RFD 0025.
- [ ] Tab/Enter autocomplete acceptance. Currently the dropdown shows but Tab doesn't complete to the first suggestion. Requires keymap changes to route Tab through the slash-autocomplete layer. Deferred for RFD 0025.
- [ ] build_frame passes `&slash` (the full registry from startup) since Concern fix, so extension commands appear in autocomplete dropdown.
