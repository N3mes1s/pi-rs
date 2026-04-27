# RFDs (Requests for Discussion)

Lightweight design docs for non-trivial pi-rs work. One RFD per feature
or refactor that's larger than a single commit, ambiguous in scope, or
crosses crate boundaries.

## Workflow

1. Copy `0000-template.md` to `NNNN-short-slug.md` (next free number,
   four digits, lowercase-kebab slug).
2. Fill it in. Keep the **Background** and **Proposal** sections short
   enough that a reviewer can read the whole RFD in 5 minutes.
3. Open a PR with the RFD as the only change. Land the RFD first.
4. Implement against the RFD. If the RFD turns out wrong, amend it in
   a follow-up PR rather than letting it drift.
5. When the work ships, bump the RFD's `Status` field to `Implemented`
   and add a final commit hash.

## States

| Status        | Meaning                                          |
| ------------- | ------------------------------------------------ |
| `Draft`       | Author still iterating; not ready for review.    |
| `Discussion`  | PR open, soliciting feedback.                    |
| `Accepted`    | Direction agreed; implementation may start.      |
| `Implemented` | Shipped; RFD is now a historical record.         |
| `Rejected`    | Decided against. Keep the RFD; it's the receipt. |
| `Superseded`  | Replaced by a later RFD; cross-link both ways.   |

## Index

| RFD  | Title                                            | Status     |
| ---- | ------------------------------------------------ | ---------- |
| 0000 | Template                                         | n/a        |
| 0001 | LSP-on-write hook                                | Discussion |
| 0002 | Tier-5 follow-ups                                | Draft      |
