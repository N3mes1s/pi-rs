#!/usr/bin/env bash
# fleet-run.sh <agent-name> [args...] — rebuild-if-changed dispatch shim.
#
# This is the piece that makes the compiled agents *evolvable*: the
# binaries themselves are immutable (RFD 0028), so evolution happens at
# build time. halo dispatches THIS script instead of a raw binary path;
# on every dispatch it re-runs pi-build codegen from the (possibly
# halo-rewritten) manifest and `cargo build`s the result. Deterministic
# codegen means an unchanged manifest is a byte-identical tree and the
# build is a cache hit — the common case costs ~1s. A manifest the halo
# implementer edited last cycle produces a fresh binary here, THEN runs.
#
# Contract with halo's dispatch loop (RFD 0028 §D):
#   - stdin carries the cycle prompt — only the final exec touches it;
#     every build step reads /dev/null.
#   - stdout is the agent's JSONL event stream — build-step stdout is
#     routed to stderr so spend attribution never sees build noise.
#   - exit 75 = rebuild failed (EX_TEMPFAIL, matching pi-build's own
#     cargo-failure code); map it to "alert" in halo.toml.
set -u

if [ $# -lt 1 ]; then
  echo "usage: fleet-run.sh <agent-name> [agent-args...]" >&2
  exit 64
fi
NAME="$1"; shift

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(dirname "$HERE")"
MANIFEST="$HERE/$NAME.toml"
OUT_DIR="$REPO_ROOT/target/agents-fleet/$NAME-build"
BIN="$OUT_DIR/target/release/$NAME"

[ -f "$MANIFEST" ] || { echo "fleet-run: no manifest $MANIFEST" >&2; exit 66; }

# Locate (or build) pi-build. Honor an operator override.
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

# Regenerate from the manifest. --force is safe: codegen is
# deterministic, so an unchanged manifest rewrites identical bytes.
"$PI_BUILD" "$MANIFEST" --out "$OUT_DIR" --force >/dev/null 2>&1 </dev/null \
  || { echo "fleet-run: pi-build codegen failed for $NAME" >&2; exit 75; }

# Until pi-sdk is on crates.io: pin it to this repo's sources, and opt
# the generated crate out of the enclosing cargo workspace. Idempotent
# (codegen just rewrote Cargo.toml, so these are always missing here).
{
  printf '\n[patch.crates-io]\npi-sdk = { path = "%s/crates/pi-sdk" }\n' "$REPO_ROOT"
  printf '\n[workspace]\n'
} >> "$OUT_DIR/Cargo.toml"

(set -o pipefail; cd "$OUT_DIR" && cargo build --release 2>&1 | sed 's/^/[fleet-build] /' >&2) </dev/null
st=$?
[ -x "$BIN" ] || st=1
if [ "$st" -ne 0 ]; then
  echo "fleet-run: cargo build failed for $NAME" >&2
  exit 75
fi

exec "$BIN" "$@"
