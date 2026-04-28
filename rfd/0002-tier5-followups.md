# RFD 0002 — Tier-5 follow-ups

- **Status:** Draft
- **Author:** pi-rs maintainers
- **Created:** 2026-04-27
- **Implemented:** &lt;n/a — tracking RFD&gt;

## Summary

Catalogue of work explicitly deferred from H1–H5 plus the obvious next
steps. This RFD does not propose a design for any single item; it
exists so we have one canonical place to point at when scoping the
next dogfood cycle. Each line should graduate to its own RFD when work
starts.

## Background

After RFD 0001 lands, the LSP integration is feature-complete for the
"agent writes a file, gets formatted code + fresh diagnostics" loop.
Several adjacent improvements were carved out of the original
`dogfood-tier4-task.md` to keep that RFD honest. This RFD enumerates
them.

## Proposal — items by priority

### P0 (next cycle)

- **0003 — `textDocument/willSave` flow.** Replace fire-and-forget
  format-on-write with the synchronous `willSaveWaitUntil` request.
  Trade-off: lets us cancel the write if formatting fails; introduces
  a timeout cliff. Worth doing if we hit a "formatter ate my file"
  bug.
- **0004 — TUI diagnostic surfacing.** Render `display.diagnostics`
  inline in the transcript instead of leaving it for the model to
  rephrase. Probably a small panel on the right.

### P1 (when a user asks)

- **0005 — Format-on-type.** Honour `textDocument/onTypeFormatting`
  inside the `edit` tool. Lower value than format-on-write because
  the agent rarely streams partial edits.
- **0006 — Code-action menu wiring.** Surface `codeAction` results as
  picker entries. The engine op already exists (`code_actions`); we
  need a UI affordance.
- **0007 — Per-language formatting options.** Move tabSize /
  insertSpaces / `trim*` flags into `LspConfig.languages.<lang>` so a
  user can override per project.

### P2 (defer until evidence)

- **0008 — Multi-server-per-language fallback.** Today one server per
  language; if rust-analyzer crashes there's no second chance until
  `reload`. Could keep a backup process on standby.
- **0009 — Workspace symbol search.** Adds `workspace/symbol` as a
  12th LSP op. Useful for "find that function across the whole repo"
  but the agent can already do that via `grep`.
- **0010 — LSP server lifecycle telemetry.** Track spawn count,
  request counts, error counts per server. Feeds the `status` op.

## Out of scope

Not on this list (= not currently planned):

- Replacing rust-analyzer with a vendored binary for hermetic CI.
- LSP completion (the agent doesn't type into a buffer).
- DAP / debugger integration.

## Open questions

- **Cadence.** How many of these per cycle? Tier-1..4 averaged ~2
  RFDs each. P0 alone is 2 RFDs; P1 is 3. Pick a budget.
- **Authoring.** Until someone explicitly takes one, the next-up RFD
  is unowned. Should we require a name in the `Author` field at
  `Discussion` state?
