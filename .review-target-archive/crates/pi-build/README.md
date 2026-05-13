# pi-build

Compile a TOML manifest into a standalone Rust binary embedding
[`pi-sdk`](../pi-sdk). Implements [RFD 0028 — Compiled agents from
TOML manifest](../../rfd/0028-compiled-agents.md).

## Verbs

```text
pi-build validate <agent.toml>

  Parse + semantic-validate the manifest. Prints
    OK: <name> <version> (<provider>/<model>) — <N> tools allowlisted
  on success; an error pointing at the offending value otherwise.
  See RFD 0028 §A for the schema.

pi-build <agent.toml> [--out DIR] [--force] [--build] [--target T] [--release | --debug]

  Generate a Cargo project from <agent.toml>. Writes:
    <out>/
    ├── Cargo.toml              # caret-pinned pi-sdk + minimal tokio
    ├── src/main.rs             # generated; tokio::main current_thread
    └── pi-build.lock           # pi_build_version + manifest_sha256

  Default --out = <agent_name>-build/. With --build, runs `cargo build`
  in the output dir; with --target, forwards to cargo (operator must
  have the target installed via `rustup target add`).
```

## Exit codes (per RFD 0028 §Cross-cutting #5)

| Code | Meaning |
|---|---|
| 0  | Success. |
| 64 | EX_USAGE — bad CLI args. |
| 65 | EX_DATAERR — manifest parse / validation failed. |
| 66 | EX_NOINPUT — cannot read input file. |
| 73 | EX_CANTCREAT — I/O error writing the output dir. |
| 75 | EX_TEMPFAIL — cargo build failed (or cargo not on PATH). |

The generated agent itself uses a separate but compatible exit-code
contract: 0 on success, 1 on agent error, 2 on missing-auth, 3 on
budget exhaustion. See RFD 0028 §B.12.

## Cross-compile matrix

pi-build is opinion-free about which target the operator builds for —
`--target` forwards verbatim to cargo. The targets pi-rs CI exercises
end-to-end (so the pi-sdk transitive depgraph is known to compile):

| Target | Notes |
|---|---|
| `x86_64-unknown-linux-musl`  | static-linked Linux; canonical "drop in a container." |
| `x86_64-unknown-linux-gnu`   | dynamic-linked Linux; matches the host pi-rs builds on. |
| `aarch64-unknown-linux-musl` | ARM Linux (e.g., Graviton). |
| `aarch64-apple-darwin`       | macOS arm64 (Apple Silicon). |
| `x86_64-apple-darwin`        | macOS x86_64. |
| `x86_64-pc-windows-msvc`     | Windows; CI smoke only — no halo support yet. |

Other tier-1/tier-2 Rust targets should work; file an issue if not.

## Hidden-flag policy (§C.2)

`pi-build --build` invokes cargo with **only** `--manifest-path
<out>/Cargo.toml` beyond what the operator passed. NO `RUSTFLAGS`
injection, NO `-C` overrides, NO `--frozen`/`--locked`. Profile
tuning (`lto = "thin"`, `strip = true`, `codegen-units = 1`) lives in
the generated `Cargo.toml` where the operator can audit + override it.

## Reproducibility

Same `(manifest, pi-build version)` → byte-identical output. Pi-build
emits no timestamps, no random IDs, no env reads beyond `PATH` (for
the optional cargo subprocess).

`pi-build.lock` carries `pi_build_version` + `manifest_sha256`. The
v1 lock is sufficient for a future `pi-build verify <binary>` verb
(deferred to v2 per RFD 0028 §C.6). It is NOT sufficient for full
reconstruction — the manifest source itself, the rustc toolchain
version, and the resolved pi-sdk dependency graph (the generated
`Cargo.lock`) are all part of "the same binary, anywhere" and must
be retained alongside.

## Out of scope (v1)

- Bit-identical reproducibility across pi-build minors.
- `pi-build verify <binary>` (deferred to v2; the lock-file shape is
  already in place).
- `pi-build migrate <agent.toml>` (premature for a one-version schema).
- Code signing (Sigstore / cosign) — operator's tooling.
- Container images — operator wraps the binary in their own Dockerfile.
- Package-manager distribution (apt, brew, AUR) — operator's choice.
- Toolchain bundling — operator's `cargo` + `rustup` are the build
  authority.

## Custom tools, microvm sandbox, MCP

All deferred to v2. RFD 0028 §A.9.

## RFD reference

[RFD 0028 — Compiled agents from TOML manifest](../../rfd/0028-compiled-agents.md):

- §A — Manifest schema
- §B — Codegen + runtime
- §C — Distribution (this crate)
- §D — Halo integration (separate workstream in `pi-coding-agent`)
