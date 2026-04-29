# RFD 0024 — Ratatui-based TUI rewrite

- **Status:** Discussion
- **Author:** Claude
- **Created:** 2024-01-01
- **Implemented:** <commit-hash, once shipped>

## Summary

Replace pi-rs's hand-rolled `pi-tui::DiffRenderer` with ratatui—a mature, proven terminal UI framework used by dozens of production tools (Helix, zoxide, bottom, etc.). The rewrite solves four concurrent bugs that the hand-rolled renderer has accumulated: mid-word wrap bugs, no markdown rendering, no syntax highlighting in code blocks, and missing autocomplete dropdown UI. Rather than patching each bug twice (once in `DiffRenderer`, again in `ratatui` when we migrate), we'll consolidate on ratatui once and for all, gaining robust text wrapping, markdown/syntax support, and theme switching for free.

## Background

The current architecture renders the transcript via two hand-rolled layers:
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

### Library stack

**Adopt ratatui + four supporting crates:**

- **ratatui** (v0.27+): Drop-in replacement for the raw terminal drawing. Handles cell-based layout, double-buffering, cursor positioning, theme/palette management. Proven in Helix, zoxide, bottom.
- **textwrap** (already in deps): Word wrapping with the `HyphenSplitter` — no mid-grapheme splits.
- **pulldown-cmark** (already in deps): Parse markdown (inline emphasis, code, fenced blocks). Convert AST to styled spans.
- **syntect** (already in deps): Bundle `default-syntaxes` TOML for offline syntax highlighting. Use `base16-ocean.dark` as the default theme (dark-friendly, good contrast).

**Rejected alternatives:**
- **termimad**: Renders markdown directly to terminal, but couples markdown parsing + rendering. We want to parse once, then render to ratatui's cell-based surface. Would require a shim layer anyway.
- **crossterm + raw escape codes** (status quo): Fragile, bug-prone, hard to test. This is what we're leaving behind.
- **colourfull** (syntax): Less complete than syntect; doesn't bundle syntaxes offline.
- **rustix-based terminals**: Requires platform code we don't need.

### New module structure

**`pi-tui`** (mostly internal refactoring, same public API):
- Keep public types `Frame`, `Line`, `Span` (unchanged API).
  - Internally, these become thin shims over ratatui's `Text`, `Line`, `Span`.
- Replace `DiffRenderer<W>` — the implementation uses ratatui's `Terminal`.
  - Public surface stays: `new(W)`, `render(&Frame)`, `resize(cols)`.
- Keep `Editor` (unchanged).
- Expand `Theme` to include theme name + palette caching so `/theme <name>` can be live.

**`pi-coding-agent::renderer`**:
- Keep `Transcript` and `Block` (unchanged).
- Replace `render()` — instead of returning a flat `Vec<Line>` of plain text + colours, parse the assistant text as markdown and emit styled spans.
  - For each `AssistantText` block, call `pulldown_cmark::parse()` and walk the event stream.
  - Inline emphasis → styled spans (bold, italic, code).
  - Fenced code blocks → syntax highlight each line, frame with language label.
- Replace `wrap_line()` — use `textwrap::wrap()`.

**New `pi-coding-agent` module: `markdown`**:
- Pure functions: `parse_and_render_markdown(text, theme, width)` → `Vec<(String, Option<Color>)>` (lines with spans).
- `render_code_block(lang, code, theme, width)` → syntax-highlighted lines with language label.

**New feature: autocomplete dropdown**:
- In `modes/interactive.rs`: when editor text matches `/[a-z]*`, render a dropdown widget showing the matching slash commands.
- Use ratatui's `Block` + `List` for the dropdown frame.

### Public API stability

The key promise: **`pi-tui::Frame`, `Line`, and `Span` remain unchanged.**

Internally:
```rust
pub struct Span {
    pub text: String,
    pub color: Option<Color>,  // crossterm::style::Color
}

pub struct Line {
    pub spans: Vec<Span>,
}

pub struct Frame {
    pub lines: Vec<Line>,
    pub cursor_at: Option<(u16, u16)>,
}

pub struct DiffRenderer<W: Write> {
    // ... ratatui::Terminal<Backend<W>> lives in here
}
```

`pi-coding-agent` doesn't change its public surface — it calls `renderer.render()` and gets a `Frame` back, as before. The renderer's job now includes markdown/syntax processing; it was always implicit.

### Migration order (so each step compiles and tests pass)

**Phase 0: RFD + scoping** ← you are here.

**Phase 1: Thin ratatui shim (diff renderer rewrite)**
1. Swap `DiffRenderer` implementation to use `ratatui::Terminal` underneath.
   - Keep the same methods: `new()`, `render(&Frame)`, `resize()`.
   - Internally convert `Frame::Line::Span` → ratatui's cell-based drawing.
   - Test: `pi --route auto -p "hello"` still works, looks the same.
2. Verify all existing tests pass (`cargo test --test interactive_*`).

**Phase 2: Markdown + syntax highlighting**
1. Add `markdown.rs` module with `parse_and_render_markdown()`.
2. Hook it into `renderer.rs::Transcript::render()` for `AssistantText` blocks.
3. Test markdown parsing (snapshot assertions on spans, no literal `**` or backticks).
4. Test syntax highlighting (fenced rust block renders with colours).
5. Integration test: `pi --route auto -p "explain monad with **bold** and \`code\`"` renders without literal markers.

**Phase 3: Feature parity**
1. **Autocomplete dropdown**: Extend `modes/interactive.rs` to render a dropdown widget when the editor text starts with `/`.
   - Test: `"/he" + Tab` completes to `/help`.
   - Test: dropdown appears and dismisses correctly.
2. **Powerline footer**: Refactor `footer.rs` to emit multiple coloured segments (model, cwd, git, usage, ctx) separated by ▶.
   - Test: `footer_powerline_segments.rs` asserts multiple distinct spans with colours.
3. **Live theme switching**: Extend `/theme` command to call `DiffRenderer::set_theme()`.
   - Test: `/theme dark` flips the palette and next frame renders with new colours.
4. **Code block visual framing**: Render fenced code blocks with a distinct background, language label on the fence line.
   - Test: `render_code_block.rs` asserts frame structure.

**Phase 4: Cleanup**
1. Delete old `wrap_line()`, unused colour padding helpers.
2. Run full suite: `cargo test --test interactive_*` + `cargo test pi-coding-agent`.
3. Manual test: `pi --route auto -p "explain monad..."` visibly shows markdown, syntax, footer.

### Non-goals for v1

- **Mouse support**: Deferred. Ratatui supports it; we're not wiring it up yet.
- **Sixel / iTerm2 inline images**: Deferred.
- **Per-language LSP formatting options** (RFD 0007): Separate effort, out of scope.
- **Autocomplete for arbitrary tools or shell commands**: v1 is slash-commands only.
- **Multi-pane layouts**: We stay single-pane. Ratatui's layout engine is available for future work.

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

- [ ] (none yet; update during implementation)
