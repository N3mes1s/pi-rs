#!/usr/bin/env bash
# Per RFD 0027 §3 + Commit G: regenerate
# `crates/pi-sdk/COMPATIBILITY.md` from `crates/pi-sdk/compatibility.toml`.
#
# CI runs this on every release; the markdown is committed alongside
# the TOML so embedders can read the matrix without running cargo.
#
# Usage:
#   bash scripts/gen-compatibility-matrix.sh
#
# Dependencies: bash, awk. No yq / no python — keep the deploy
# surface tight.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TOML="${REPO_ROOT}/crates/pi-sdk/compatibility.toml"
OUT="${REPO_ROOT}/crates/pi-sdk/COMPATIBILITY.md"

if [[ ! -f "${TOML}" ]]; then
  echo "ERROR: compatibility.toml not found at ${TOML}" >&2
  exit 2
fi

cat > "${OUT}" <<'HEADER'
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
HEADER

# Stream the TOML rows in order, emit one markdown row each.
# Schema is fixed; we don't need a real TOML parser.
#
# Per code-review pass-6 finding #7: anchor each field regex with
# `[ \t]*=` so a future field whose name starts with an existing
# field's name (e.g. hypothetical `pi_sdk_canary` colliding with
# `pi_sdk`, or `pi_tools_net` with `pi_tools_core`) doesn't get
# silently overwritten by the wrong line.
awk '
  /^\[\[release\]\]/                      { in_row = 1; sec = 0; next }
  in_row && /^pi_sdk[ \t]*=/              { gsub(/[ "]/, "", $3); pi_sdk        = $3 }
  in_row && /^pi_tool_types[ \t]*=/       { gsub(/[ "]/, "", $3); pi_tool_types = $3 }
  in_row && /^pi_ai[ \t]*=/               { gsub(/[ "]/, "", $3); pi_ai         = $3 }
  in_row && /^pi_tools_core[ \t]*=/       { gsub(/[ "]/, "", $3); pi_tools_core = $3 }
  in_row && /^pi_sandbox[ \t]*=/          { gsub(/[ "]/, "", $3); pi_sandbox    = $3 }
  in_row && /^pi_agent_core[ \t]*=/       { gsub(/[ "]/, "", $3); pi_agent_core = $3 }
  in_row && /^notes/        {
    sub(/^[^=]*= */, "")
    gsub(/(^"|"$)/, "")
    notes = $0
  }
  in_row && /^security/ && /true/ { sec = 1 }
  in_row && /^$/ {
    flag = sec ? " ⚠ SEC" : ""
    printf "| %s%s | %s | %s | %s | %s | %s | %s |\n", pi_sdk, flag, pi_tool_types, pi_ai, pi_tools_core, pi_sandbox, pi_agent_core, notes
    in_row = 0
    pi_sdk = ""; pi_tool_types = ""; pi_ai = ""; pi_tools_core = ""
    pi_sandbox = ""; pi_agent_core = ""; notes = ""
  }
  END {
    if (in_row && pi_sdk != "") {
      flag = sec ? " ⚠ SEC" : ""
      printf "| %s%s | %s | %s | %s | %s | %s | %s |\n", pi_sdk, flag, pi_tool_types, pi_ai, pi_tools_core, pi_sandbox, pi_agent_core, notes
    }
  }
' "${TOML}" >> "${OUT}"

cat >> "${OUT}" <<'FOOTER'

`⚠ SEC` flag = MINOR release shipped a security-CVE breaking change
under the §3 escape hatch. Read the changelog before upgrading.
FOOTER

echo "Generated: ${OUT}"
