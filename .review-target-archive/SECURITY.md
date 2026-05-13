# Security policy

## Reporting a vulnerability

**DO NOT open public GitHub issues or pull requests for security vulnerabilities.**

Use [GitHub Security Advisories](https://github.com/n3mes1s/playground/security/advisories/new) to report privately. Maintainers receive notification immediately and coordinate a fix + disclosure timeline with the reporter.

If GitHub Security Advisories is unavailable to you, email the maintainers via the address listed in the [pi-rs repository's project metadata](https://github.com/n3mes1s/playground). Maintainers acknowledge security reports within 7 days.

## Disclosure timeline

Per RFD 0027 §3 (security exception):

- **Day 0** — embargoed report received via GitHub Security Advisories.
- **Day 0–7** — maintainers triage + reproduce + assign a CVE number if the vulnerability is in the public surface.
- **Day 7–30** — patch development. The reporter is invited to review the fix.
- **Day 30** — public advisory + patched release published. RUSTSEC entry submitted to https://github.com/rustsec/advisory-db.
- **Day 30+** — disclosure window per the RFD's "30-day pre-notification" policy is satisfied.

A High+ severity CVE in a stable trait or POD type may force a breaking change inside a MINOR release per RFD 0027 §3 (security exception). Embedders subscribed to GitHub Security Advisories receive 30-day pre-notification of forced breaking changes.

## Supported versions

| Version | Status | Security backports |
|---------|--------|--------------------|
| `pi-sdk 0.1.x` | Pre-1.0 (current) | Yes — until pi-sdk 1.0 ships. |
| `pi-sdk 0.2+` | Pre-1.0 (future) | Latest pre-1.0 line only. |
| `pi-sdk 1.x`  | Stable (planned) | 6 months past the next MAJOR's ship date. |

Pre-1.0 means breaking changes can land in any 0.x → 0.x+1 release. Embedders pinning `pi-sdk = "0.1"` get 0.1.x backports until 1.0; embedders pinning `pi-sdk = "0.1.0"` (exact) get backports only on 0.1.0.

## Hardening contract

Per RFD 0027 §4.5, the SDK ships with the following hardening invariants:

- `catch_unwind` boundary around custom `Tool::invoke` — a panicking tool is converted to an error, not a worker-thread crash.
- Stream-event validation — `Finish::ToolUse` requires ≥1 tool call; saturating-add token counters; per-session cumulative-token budget cap; per-turn tool-invocation cap.
- `ToolGate` carries `GateContext { session_id, turn_index, parent_session, recursion_depth }` so naïve "approve once" gates can't be bypassed cross-session.
- `ToolRegistry::register` returns `Result<(), DuplicateName>` — silent last-write-wins is rejected.
- `bash` tool: `cwd` argument is canonicalize-jailed against `ctx.cwd`; `timeout_ms` clamped at 600 s; per-tool input size cap (64 KiB).
- `AuthStorage` on-disk persistence: `0o600` perms + atomic temp + rename on write. `from_env_explicit` for opt-in env scanning; `scoped(allowlist)` for per-tenant restriction; `sealed()` for post-init immutability.
- `WireSerializer` on JSONL: 1 MiB per-field cap, ANSI escape stripping, C1 + bidi-override `\u`-escape.

Each invariant is regression-tested. See `crates/pi-sdk-canary` for the compatibility canary that verifies the hardening contract stays intact across MINOR releases.

## RUSTSEC advisory namespace

Reserved: `RUSTSEC-2026-PI-SDK-NNNN` (number assigned at disclosure). Embedders consuming `pi-sdk` should run `cargo audit` against their resolved lockfile every release.

## What this policy does not cover

- Vulnerabilities in transitive dependencies that pi-sdk itself does not pin (e.g. a CVE in `tokio` that affects all downstream Rust users). Subscribe to the upstream project's advisories.
- Vulnerabilities in the embedder's own custom `Tool` / `Provider` / `SandboxProvider` impls — those are the embedder's responsibility.
- Misuse: e.g. running `pi-sdk` with `ToolRegistry::with_unsafe_extras()` + `LocalProcessProvider` in production. The SDK's safe defaults exist; opting out is opting out.

See RFD 0027 §Threat-model for the explicit defends-against / does-not-defend-against split.
