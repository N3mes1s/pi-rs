# pi-sdk compatibility matrix

> **Generated from `compatibility.toml` by `scripts/gen-compatibility-matrix.sh`.
> Do NOT hand-edit — re-run the script after editing the TOML.**

Per RFD 0027 §3, every `pi-sdk` MINOR ships with the underlying
crate versions it pins. Embedders pinning `pi-sdk = "0.1"` get the
versions in the row labeled `0.1.x`; embedders depending on
`pi-ai` directly should ensure their own pin is compatible with
the matrix entry.

| pi-sdk | pi-tool-types | pi-ai | pi-tools-core | pi-sandbox | pi-agent-core | Notes |
|--------|---------------|-------|---------------|------------|---------------|-------|
| 0.1.0 | 0.1 | 0.1 | 0.1 | 0.1 | 0.1 | Initial 0.x. Façade + 7 hardening commits + 6 examples + quick_start convenience. |

`⚠ SEC` flag = MINOR release shipped a security-CVE breaking change
under the §3 escape hatch. Read the changelog before upgrading.
