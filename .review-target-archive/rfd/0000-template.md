# RFD NNNN — &lt;title&gt;

- **Status:** Draft
- **Author:** &lt;name&gt;
- **Created:** YYYY-MM-DD
- **Implemented:** &lt;commit-hash, once shipped&gt;

## Summary

One paragraph. What this RFD changes, in plain English. A reader who
only reads this paragraph should walk away knowing the *what* and the
*why* — not the *how*.

## Background

What's the current state? What problem are we solving? Link to prior
RFDs, issues, commits if relevant. Keep it tight — one or two
paragraphs is usually enough.

## Proposal

The actual design. Subsections as needed: data structures, wire
formats, API changes, migration paths. Show concrete code shapes (Rust
types, JSON examples) rather than English prose where you can.

If multiple options were considered, list the alternatives and explain
why this one wins. Reviewers should not have to ask "what about X?".

## Test plan

How will we know the implementation is correct? Bullet list of the
tests that must exist before this RFD is `Implemented`. Be specific —
"unit tests" is not a plan; "round-trip an LspSettings through TOML
serde and assert equality" is.

## Out of scope

Anything you considered but explicitly chose to defer. Each line should
either point at a follow-up RFD number or describe what would need to
change to bring it back into scope.

## Open questions

Things you genuinely don't know yet. Reviewers should answer these
during discussion. If a question is still open at merge time, lift it
into a follow-up issue and link it here.
