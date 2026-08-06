#!/usr/bin/env bash
# fleet-run.sh <agent-name> [args...] — fleet dispatch shim, two profiles.
#
# An RFD 0028 compiled agent is an immutable artifact: it cannot evolve
# at runtime. Evolution happens on the BUILD PLANE (somewhere with the
# pi-rs toolchain), and the RUN PLANE only ever swaps artifacts. This
# shim serves both, chosen via FLEET_MODE=rebuild|run (auto-detected
# when unset):
#
#   rebuild (dev / dogfood — halo supervising the pi-rs repo itself):
#     re-runs pi-build codegen from the (possibly halo-rewritten)
#     manifest and cargo-builds before exec. Deterministic codegen
#     means an unchanged manifest is a byte-identical tree → cargo
#     cache hit (~1s); a manifest the implementer edited last cycle
#     runs as a FRESH binary this cycle. Requires the pi-rs sources.
#
#   run (real deployment — host has binaries, no toolchain):
#     executes the pinned artifact from FLEET_BIN_DIR (default
#     <repo>/dist/agents-fleet). If the manifest is present on the
#     host, the artifact's baked-in manifest sha (`--version`) is
#     checked against it: a mismatch means the fleet has been evolved
#     but this host is running a stale artifact → exit 76 so halo
#     ALERTS instead of silently auditing with yesterday's brain.
#     Artifact delivery (CI build → release/registry → host) happens
#     outside this script.
#
# Both profiles append a lineage row to ~/.pi/fleet/<name>.lineage.jsonl
# so the agent (and the operator) can answer "when was I last evolved?".
#
# Contract with halo's dispatch loop (RFD 0028 §D): stdin carries the
# cycle prompt — only the final exec touches it; build/verify steps read
# /dev/null and write stdout to stderr so the JSONL spend stream stays
# clean. Exit codes: 64 usage, 66 missing manifest, 69 missing artifact,
# 75 rebuild failed, 76 stale artifact.
set -u

if [ $# -lt 1 ]; then
  echo "usage: fleet-run.sh <agent-name> [agent-args...]" >&2
  exit 64
fi
NAME="$1"; shift

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(dirname "$HERE")"
MANIFEST="$HERE/$NAME.toml"

MODE="${FLEET_MODE:-}"
if [ -z "$MODE" ]; then
  if [ -d "$REPO_ROOT/crates/pi-build" ] && command -v cargo >/dev/null 2>&1; then
    MODE=rebuild
  else
    MODE=run
  fi
fi

manifest_sha() { sha256sum "$MANIFEST" 2>/dev/null | cut -d' ' -f1; }

brain_path() { # cwd-relative path from the manifest, resolved vs REPO_ROOT
  local rel
  rel="$(grep -o 'system_prompt_file *= *"[^"]*"' "$MANIFEST" 2>/dev/null | head -1 | cut -d'"' -f2)"
  [ -n "$rel" ] && echo "$REPO_ROOT/$rel"
}

brain_sha() {
  local p; p="$(brain_path)"
  [ -n "$p" ] && [ -f "$p" ] && sha256sum "$p" | cut -d' ' -f1
}

lineage_append() { # $1=bin $2=mode
  local dir="$HOME/.pi/fleet" sha bsha ver last prev prev_b prev_ver core_evolved brain_evolved bumped
  mkdir -p "$dir" 2>/dev/null || return 0
  sha="$(manifest_sha || true)"
  bsha="$(brain_sha || true)"
  ver="$("$1" --version 2>/dev/null | head -1)"
  last="$(tail -1 "$dir/$NAME.lineage.jsonl" 2>/dev/null)"
  prev="$(echo "$last" | grep -o '"manifest_sha":"[a-f0-9]*"' | cut -d'"' -f4)"
  prev_b="$(echo "$last" | grep -o '"brain_sha":"[a-f0-9]*"' | cut -d'"' -f4)"
  prev_ver="$(echo "$last" | grep -o '"version_line":"[^"]*"' | cut -d'"' -f4)"
  core_evolved=false; [ -n "$prev" ] && [ -n "$sha" ] && [ "$prev" != "$sha" ] && core_evolved=true
  brain_evolved=false; [ -n "$prev_b" ] && [ -n "$bsha" ] && [ "$prev_b" != "$bsha" ] && brain_evolved=true
  bumped=true; [ "$core_evolved" = true ] && [ "$prev_ver" = "$ver" ] && bumped=false
  printf '{"ts":"%s","agent":"%s","mode":"%s","manifest_sha":"%s","brain_sha":"%s","version_line":"%s","evolved":%s,"brain_evolved":%s,"version_bumped":%s}\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$NAME" "$2" "${sha:-unknown}" "${bsha:-none}" "${ver:-unknown}" \
    "$core_evolved" "$brain_evolved" "$bumped" \
    >> "$dir/$NAME.lineage.jsonl"
  [ "$brain_evolved" = true ] && echo "fleet-run: $NAME brain evolved since last dispatch (runtime reload, no rebuild)" >&2
  if [ "$core_evolved" = true ]; then
    echo "fleet-run: $NAME core manifest changed since last dispatch (evolved)" >&2
    [ "$bumped" = false ] && echo "fleet-run: WARNING: $NAME core evolved without an [agent].version bump" >&2
  fi
}

if [ "$MODE" = run ]; then
  BIN="${FLEET_BIN_DIR:-$REPO_ROOT/dist/agents-fleet}/$NAME"
  [ -x "$BIN" ] || { echo "fleet-run: no artifact $BIN (deploy one from the build plane)" >&2; exit 69; }
  if [ -f "$MANIFEST" ]; then
    baked="$("$BIN" --version 2>/dev/null | grep -o 'manifest-sha256:[a-f0-9]*' | cut -d: -f2)"
    want="$(manifest_sha)"
    if [ -n "$baked" ] && [ -n "$want" ] && [ "$baked" != "$want" ]; then
      echo "fleet-run: STALE ARTIFACT: $NAME binary was built from sha ${baked:0:12}, manifest is ${want:0:12}" >&2
      exit 76
    fi
  fi
  lineage_append "$BIN" run
  exec "$BIN" "$@"
fi

# ── rebuild profile ──────────────────────────────────────────────────
[ -f "$MANIFEST" ] || { echo "fleet-run: no manifest $MANIFEST" >&2; exit 66; }
OUT_DIR="$REPO_ROOT/target/agents-fleet/$NAME-build"

# The binary lands under target/<forced-target>/release when a
# .cargo/config.toml in an ancestor dir (pi-rs forces musl) sets
# [build].target, else under target/release.
resolve_built_bin() {
  local c
  for c in "$OUT_DIR"/target/*/release/"$NAME" "$OUT_DIR/target/release/$NAME"; do
    [ -x "$c" ] && { echo "$c"; return 0; }
  done
  return 1
}
BIN="$(resolve_built_bin || true)"

PI_BUILD="${PI_BUILD_BIN:-}"
if [ -z "$PI_BUILD" ]; then
  for t in x86_64-unknown-linux-musl ""; do
    for p in release debug; do
      c="$REPO_ROOT/target/${t:+$t/}$p/pi-build"
      [ -x "$c" ] && PI_BUILD="$c" && break 2
    done
  done
fi
if [ -z "$PI_BUILD" ]; then
  (cd "$REPO_ROOT" && cargo build -p pi-build >/dev/null 2>&1) </dev/null \
    || { echo "fleet-run: cannot build pi-build" >&2; exit 75; }
  PI_BUILD="$(find "$REPO_ROOT/target" -name pi-build -type f -perm -u+x | head -1)"
  [ -n "$PI_BUILD" ] || { echo "fleet-run: pi-build not found after build" >&2; exit 75; }
fi

# Content-addressed skip: pi-build.lock records the sha256 of the
# manifest the crate was generated from. When it matches the current
# manifest, the binary exists, and pi-build itself hasn't been rebuilt
# since, the core is already up to date — skip codegen AND cargo (a
# thin-LTO relink costs minutes, and `--force` codegen would dirty
# mtimes and trigger one). Brain-file changes never come through here:
# the binary loads the brain at startup, no rebuild involved.
if [ -n "$BIN" ] \
  && grep -q "manifest_sha256  = \"$(manifest_sha)\"" "$OUT_DIR/pi-build.lock" 2>/dev/null \
  && [ "$BIN" -nt "$PI_BUILD" ]; then
  lineage_append "$BIN" rebuild
  exec "$BIN" "$@"
fi

"$PI_BUILD" "$MANIFEST" --out "$OUT_DIR" --force >/dev/null 2>&1 </dev/null \
  || { echo "fleet-run: pi-build codegen failed for $NAME" >&2; exit 75; }

# Until pi-sdk is on crates.io: pin it to this repo's sources, and opt
# the generated crate out of the enclosing cargo workspace. Idempotent
# (codegen just rewrote Cargo.toml, so these lines are always absent).
{
  printf '\n[patch.crates-io]\npi-sdk = { path = "%s/crates/pi-sdk" }\n' "$REPO_ROOT"
  printf '\n[workspace]\n'
} >> "$OUT_DIR/Cargo.toml"

(set -o pipefail; cd "$OUT_DIR" && cargo build --release 2>&1 | sed 's/^/[fleet-build] /' >&2) </dev/null
st=$?
BIN="$(resolve_built_bin || true)"
[ -n "$BIN" ] || st=1
if [ "$st" -ne 0 ]; then
  echo "fleet-run: cargo build failed for $NAME" >&2
  exit 75
fi

lineage_append "$BIN" rebuild
exec "$BIN" "$@"
