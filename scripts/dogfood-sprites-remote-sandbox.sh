#!/usr/bin/env bash
# Sprites remote-sandbox end-to-end dogfood (RFD 0026 v2).
#
# Sister to scripts/dogfood-e2b-remote-sandbox.sh. Where E2B's base
# template blocks user FUSE mounts (no_new_privs=1 + /dev/fuse mode-600
# + sudo blocked — see RFD 0026 §6/§7), Sprites unblocks every
# constraint: NoNewPrivs=0, sudo works, /dev/fuse openable. v1 of the
# Sprites path here uses wromm's project sync at session-open (same
# semantics as the E2B v1 SmartSync path); the contextfs RW /work
# follow-up extends this.
#
# USAGE
#   bash scripts/dogfood-sprites-remote-sandbox.sh
#
# PREREQUISITES (all must be set or the script skips cleanly)
#   SPRITES_TOKEN          — Sprites API token (from
#                            https://sprites.dev → Dashboard → API Keys)
#   PI_SANDBOX_WORKER_BIN  — path to a musl-linked pi-sandbox-worker
#   wromm                  — `wromm` on PATH (or PI_WROMM_BIN)
#
# WHAT THIS DEMONSTRATES
#   1. Stages a tiny Rust source tree (one buggy .rs file) in a tempdir.
#   2. Runs `pi --sandbox-provider=sprites` with a self-contained brief:
#      "read src/math.rs, identify the bug, write a test in
#       tests/bug_test.rs that catches it".
#   3. Verifies (host-side) that:
#        a. The agent's bash tool calls landed inside the Sprite —
#           proven via the JSONL `hostname` ToolResult.
#        b. The new tests/bug_test.rs showed up in the host tempdir
#           (proving wromm's project sync flushed back).
#        c. pi exited 0.
#
# GUARANTEES
#   • Tears down the sprite + tempdir on exit (trap).
#   • Skips cleanly if SPRITES_TOKEN unset.

set -euo pipefail

# Default to the pi-rs musl-static build (NOT the npm `pi` on PATH,
# which is a different binary that doesn't know --sandbox-provider=sprites).
# Override with $PI_BIN if you've built somewhere else.
PI_BIN="${PI_BIN:-/home/nemesis/code/pi-rs/target/x86_64-unknown-linux-musl/release/pi}"
WORKER_BIN="${PI_SANDBOX_WORKER_BIN:-/home/nemesis/code/pi-rs/target/x86_64-unknown-linux-musl/release/pi-sandbox-worker}"

CYAN=$'\033[0;36m' GREEN=$'\033[0;32m' RED=$'\033[0;31m' RESET=$'\033[0m'
log()  { echo "${CYAN}[sprites-demo]${RESET} $*"; }
ok()   { echo "${GREEN}[sprites-demo ✓]${RESET} $*"; }
fail() { echo "${RED}[sprites-demo ✗]${RESET} $*"; }

# ── precheck ────────────────────────────────────────────────────────────────
if [[ -z "${SPRITES_TOKEN:-}" ]]; then
    log "SPRITES_TOKEN not set — SKIP."
    log "  set SPRITES_TOKEN to a Sprites API token to run this demo."
    exit 0
fi
[[ -x "$PI_BIN"      ]] || { fail "PI_BIN not executable: $PI_BIN"; exit 2; }
[[ -x "$WORKER_BIN"  ]] || { fail "PI_SANDBOX_WORKER_BIN not executable: $WORKER_BIN"; exit 2; }
command -v wromm >/dev/null || { fail "wromm not on PATH"; exit 2; }
export PI_SANDBOX_WORKER_BIN="$WORKER_BIN"

# ── stage a toy source tree ─────────────────────────────────────────────────
TMPDIR_DEMO="$(mktemp -d)"
cleanup() {
    log "tearing down (sprite cleanup happens via pi's session end hook)"
    if [[ "${PI_EXIT:-1}" -eq 0 && -f "$TMPDIR_DEMO/tests/bug_test.rs" ]]; then
        rm -rf "$TMPDIR_DEMO" || true
    else
        log "  KEEPING tempdir for debug: $TMPDIR_DEMO"
    fi
}
trap cleanup EXIT

cat > "$TMPDIR_DEMO/Cargo.toml" <<'TOML'
[package]
name = "sprites-dogfood"
version = "0.0.1"
edition = "2021"

[lib]
name = "sprites_dogfood"
path = "src/lib.rs"
TOML

mkdir -p "$TMPDIR_DEMO/src" "$TMPDIR_DEMO/tests"
cat > "$TMPDIR_DEMO/src/lib.rs" <<'RS'
pub mod math;
RS
cat > "$TMPDIR_DEMO/src/math.rs" <<'RS'
/// Buggy: subtracts unconditionally on unsigned types — underflows
/// when a < b.
pub fn abs_diff(a: u32, b: u32) -> u32 {
    a - b
}
RS

# wromm needs a wromm.json spec to provision a sandbox. Generate a
# minimal valid one — pi's SpritesProvider does this automatically too,
# but we keep the dogfood self-contained.
cat > "$TMPDIR_DEMO/wromm.json" <<'JSON'
{
  "name": "pi-sprites-dogfood",
  "runtimes": [],
  "system_packages": [],
  "services": [],
  "ports": [],
  "env": {},
  "source": {"type": "Manual"},
  "agent": null
}
JSON

cd "$TMPDIR_DEMO"
log "tempdir: $TMPDIR_DEMO"

# ── run pi ──────────────────────────────────────────────────────────────────
LOG_FILE="$TMPDIR_DEMO/pi-agent.log"

START_NS=$(date +%s%N)
PROMPT_FILE="$TMPDIR_DEMO/.brief.txt"
cat > "$PROMPT_FILE" <<'BRIEF'
You are inside a Sprites remote sandbox. The working directory contains a small Rust crate with a buggy function in src/math.rs.

Your task:
1. Read src/math.rs and identify the bug in abs_diff().
2. Run: hostname   (this confirms the bash tool runs inside the Sprite, not on the host -- output should NOT contain "kusanagi").
3. Write a test file at tests/bug_test.rs that catches the abs_diff overflow bug. Use the write tool (NOT bash echo) so the file is synced back to the host. The test should call abs_diff(3, 5) and assert the result equals 2 (the correct absolute difference).

Write ONLY the test file. Do NOT fix the bug. Stop after writing.
BRIEF

"$PI_BIN" \
    --sandbox-provider=sprites \
    --provider=anthropic \
    --model=claude-sonnet-4-6 \
    --auto-approve=yolo \
    --json \
    -p "$(cat "$PROMPT_FILE")" \
    > "$LOG_FILE" 2>&1
PI_EXIT=$?
END_NS=$(date +%s%N)
ELAPSED_MS=$(( (END_NS - START_NS) / 1000000 ))
log "pi exited code=$PI_EXIT after ${ELAPSED_MS}ms"

# ── verifications ───────────────────────────────────────────────────────────

# 1: bash tool ran inside the sprite (not on the host).
log "verification 1/3 — bash tool ran inside Sprite (not on host)"
HOSTNAME_CALL_ID=$(grep -oP '"id":"\K[^"]+' "$LOG_FILE" \
    | while read -r cid; do
        if grep -q "\"tool_use_id\":\"${cid}\"" "$LOG_FILE" 2>/dev/null \
           && grep -B1 "\"id\":\"${cid}\"" "$LOG_FILE" | grep -q '"name":"bash"' \
           && grep -A1 "\"id\":\"${cid}\"" "$LOG_FILE" | grep -q 'hostname'; then
            echo "$cid"; break
        fi
      done | head -1)
if [[ -n "$HOSTNAME_CALL_ID" ]]; then
    HOST_OUT=$(grep "\"tool_use_id\":\"$HOSTNAME_CALL_ID\"" "$LOG_FILE" \
        | head -1 | grep -oP '"model_output":"\K[^"]+' | head -c 80)
    if [[ "$HOST_OUT" != "kusanagi"* && -n "$HOST_OUT" ]]; then
        ok "  sprite hostname '$HOST_OUT' differs from host 'kusanagi'"
    else
        fail "  sprite hostname output unexpected: '$HOST_OUT'"
    fi
else
    fail "  could not locate a 'hostname' bash ToolResult in $LOG_FILE"
fi

# 2: tests/bug_test.rs flushed back to the host tempdir.
log "verification 2/3 — tests/bug_test.rs flushed back to host"
if [[ -f "$TMPDIR_DEMO/tests/bug_test.rs" ]] \
   && grep -q "abs_diff" "$TMPDIR_DEMO/tests/bug_test.rs" 2>/dev/null; then
    ok "  tests/bug_test.rs exists on host and references abs_diff"
else
    fail "  tests/bug_test.rs was NOT flushed back to host."
    fail "  see $LOG_FILE for the full session transcript"
fi

# 3: pi exited 0.
log "verification 3/3 — pi exit code"
if [[ "$PI_EXIT" -eq 0 ]]; then
    ok "  pi exited 0"
else
    fail "  pi exited $PI_EXIT"
fi

if [[ "$PI_EXIT" -eq 0 ]] \
   && [[ -f "$TMPDIR_DEMO/tests/bug_test.rs" ]] \
   && grep -q "abs_diff" "$TMPDIR_DEMO/tests/bug_test.rs"; then
    ok "DEMO PASSED — Sprites remote sandbox v1 verified"
    exit 0
else
    fail "DEMO FAILED — see $LOG_FILE"
    exit 1
fi
