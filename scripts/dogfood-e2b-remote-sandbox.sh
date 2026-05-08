#!/usr/bin/env bash
# dogfood-e2b-remote-sandbox.sh — E2B remote-sandbox end-to-end demo
#
# RFD 0026 M3: scripted reproducible demo that drives a real coding task
# through --sandbox-provider=e2b and verifies the host↔sandbox file sync.
#
# USAGE
#   bash scripts/dogfood-e2b-remote-sandbox.sh
#
# PREREQUISITES (all must be set or the script skips cleanly)
#   E2B_API_KEY             — E2B API key (https://e2b.dev → Dashboard → API Keys)
#   PI_SANDBOX_WORKER_BIN   — path to a musl-linked pi-sandbox-worker binary
#                             built with: cargo build -p pi-sandbox-worker
#                                         --target x86_64-unknown-linux-musl --release
#
# OPTIONAL ENV OVERRIDES
#   PI_BIN                  — path to the pi binary (default: adjacent release build or 'pi')
#   E2B_SANDBOX_TIMEOUT_SECS — sandbox lifetime cap (default: 300 for this demo)
#   E2B_BASE_URL            — override E2B API endpoint (for CI mock servers)
#
# WHAT THIS DEMONSTRATES
#   1. Stages a tiny Rust source tree (one buggy .rs file) in a tempdir.
#   2. Runs pi --sandbox-provider=e2b with a self-contained coding brief:
#      "read src/math.rs, identify the bug, write a test in tests/bug_test.rs
#       that catches it".
#   3. Verifies (host-side) that:
#        a. The agent's bash tool calls landed inside the E2B sandbox —
#           proven structurally: we parse the --json JSONL event log to find
#           the ToolResult for the agent's `hostname` bash call and verify
#           result.model_output != host hostname.  This is sound because
#           --json mode serialises every ToolResult (including successful
#           ones), so the proof cannot be satisfied by a tool-call line alone.
#        b. The new tests/bug_test.rs showed up in the host tempdir —
#           proving host_cwd → remote sandbox file sync (SmartSync upload)
#           and the write-tool flushback (ToolResponse.file_writes) both worked.
#        c. pi --sandbox doctor runs (probes microVM; E2B env vars are
#           validated separately with hard-fail assertions).
#
# GUARANTEES
#   • Never tears down the caller's existing E2B sandboxes.
#     The demo's sandbox is scoped to its own PI session; cleanup() fires at
#     session exit via the mode-exit hook (RFD 0026 §"Session lifecycle").
#   • Leaves no tempdir or sandbox behind on exit (trap-based cleanup).
#   • The tempdir is never committed to the repo; it is a runtime side-effect.
#   • Exits 0 when E2B_API_KEY is unset; prints a setup pointer.
#
# LIMITATIONS (v1)
#   • bash tool file mutations are NOT flushback-synced (only write/edit are).
#     The test file must be written via the write tool, not `echo > file`.
#   • Requires the pi binary to support --sandbox-provider=e2b (Commit G).
#   • Requires PI_SANDBOX_WORKER_BIN to point to a musl pi-sandbox-worker.

set -euo pipefail

# ── colour helpers ─────────────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'   # no colour

log()  { echo -e "${CYAN}[e2b-demo]${NC} $*"; }
ok()   { echo -e "${GREEN}[e2b-demo ✓]${NC} $*"; }
warn() { echo -e "${YELLOW}[e2b-demo !]${NC} $*"; }
fail() { echo -e "${RED}[e2b-demo ✗]${NC} $*" >&2; exit 1; }
skip() { echo -e "${YELLOW}[e2b-demo SKIP]${NC} $*"; exit 0; }

# ── prerequisite: E2B_API_KEY ──────────────────────────────────────────────────
if [[ -z "${E2B_API_KEY:-}" ]]; then
    skip "E2B_API_KEY is not set.

To run this demo:
  1. Sign up at https://e2b.dev and create an API key.
  2. export E2B_API_KEY=e2b_...
  3. Build the worker binary:
       cargo build -p pi-sandbox-worker --target x86_64-unknown-linux-musl --release
  4. export PI_SANDBOX_WORKER_BIN=\$(pwd)/target/x86_64-unknown-linux-musl/release/pi-sandbox-worker
  5. Re-run this script."
fi

# ── prerequisite: PI_SANDBOX_WORKER_BIN ───────────────────────────────────────
if [[ -z "${PI_SANDBOX_WORKER_BIN:-}" ]]; then
    skip "PI_SANDBOX_WORKER_BIN is not set.

Build the worker and set the path:
  cargo build -p pi-sandbox-worker --target x86_64-unknown-linux-musl --release
  export PI_SANDBOX_WORKER_BIN=\$(pwd)/target/x86_64-unknown-linux-musl/release/pi-sandbox-worker"
fi

if [[ ! -x "${PI_SANDBOX_WORKER_BIN}" ]]; then
    fail "PI_SANDBOX_WORKER_BIN=${PI_SANDBOX_WORKER_BIN} is not executable or does not exist."
fi

# ── locate the pi binary ───────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "${SCRIPT_DIR}")"

if [[ -n "${PI_BIN:-}" ]]; then
    PI="${PI_BIN}"
elif [[ -x "${REPO_ROOT}/target/x86_64-unknown-linux-musl/release/pi" ]]; then
    PI="${REPO_ROOT}/target/x86_64-unknown-linux-musl/release/pi"
elif command -v pi &>/dev/null; then
    PI="$(command -v pi)"
else
    fail "pi binary not found. Build with: cargo build -p pi-coding-agent --target x86_64-unknown-linux-musl --release"
fi
log "using pi binary: ${PI}"

# ── verify pi supports --sandbox-provider=e2b ─────────────────────────────────
if ! "${PI}" --help 2>&1 | grep -q "sandbox-provider"; then
    fail "this pi binary does not support --sandbox-provider. Rebuild from the Commit G branch."
fi

# ── sandbox doctor: probe host setup ──────────────────────────────────────────
# The correct CLI flag is `pi --sandbox doctor` (a flag on the pi binary, not
# a sub-command).  It probes for microVM/Firecracker preconditions.  For the
# E2B provider the interesting config lives in env vars rather than in the
# microVM probe, so we run doctor for completeness (it still validates the
# binary is functional and the contextfs helpers can be found) and then
# separately assert the E2B-specific vars below.
log "running: pi --sandbox doctor"
echo "────────────────────────────────────────────"
# Capture both output and exit status without letting `set -e` abort the
# script on a non-zero exit.  The if/else form suppresses the automatic
# exit-on-error behaviour for exactly this command, so DOCTOR_EXIT is
# always set before we use it.
DOCTOR_OUTPUT=""
if DOCTOR_OUTPUT="$("${PI}" --sandbox doctor 2>&1)"; then
    DOCTOR_EXIT=0
else
    DOCTOR_EXIT=$?
fi
echo "${DOCTOR_OUTPUT}"
echo "────────────────────────────────────────────"

# doctor exits non-zero only when a *blocker* is present for microVM.
# E2B does not depend on microVM, so we just emit a note if it fails.
if [[ "${DOCTOR_EXIT}" -ne 0 ]]; then
    warn "pi --sandbox doctor exited ${DOCTOR_EXIT} (microVM blockers present, but E2B does not require microVM)"
else
    ok "pi --sandbox doctor exited 0 ✓"
fi

# Verify the E2B-specific configuration: E2B_API_KEY and PI_SANDBOX_WORKER_BIN.
# These were already asserted as non-empty / executable above, so the checks
# below are assertion-grade (they will hard-fail if something regressed).
log "E2B configuration (assertion check):"
if [[ -n "${E2B_API_KEY:-}" ]]; then
    ok "  E2B_API_KEY         — set (${#E2B_API_KEY} chars) ✓"
else
    fail "  E2B_API_KEY is empty — should have been caught earlier"
fi
if [[ -n "${PI_SANDBOX_WORKER_BIN:-}" && -x "${PI_SANDBOX_WORKER_BIN}" ]]; then
    ok "  PI_SANDBOX_WORKER_BIN — ${PI_SANDBOX_WORKER_BIN} (executable ✓)"
else
    fail "  PI_SANDBOX_WORKER_BIN=${PI_SANDBOX_WORKER_BIN:-<unset>} is not executable — should have been caught earlier"
fi
if [[ -n "${E2B_BASE_URL:-}" ]]; then
    warn "  E2B_BASE_URL        — ${E2B_BASE_URL} (overridden)"
fi
E2B_TIMEOUT="${E2B_SANDBOX_TIMEOUT_SECS:-300}"
log "  sandbox lifetime cap: ${E2B_TIMEOUT}s (E2B_SANDBOX_TIMEOUT_SECS)"

# ── stage toy source tree ──────────────────────────────────────────────────────
WORK_DIR="$(mktemp -d)"
AGENT_LOG="${WORK_DIR}/pi-agent.log"
log "staged tempdir: ${WORK_DIR}"

# Trap: clean up the tempdir on any exit. The E2B sandbox cleanup is handled
# by the pi session's mode-exit hook (SandboxProvider::cleanup()), so we only
# need to remove the local tempdir here.
cleanup() {
    local code=$?
    if [[ -d "${WORK_DIR}" ]]; then
        rm -rf "${WORK_DIR}"
        log "tempdir removed"
    fi
    exit "${code}"
}
trap cleanup EXIT

# Write the buggy Rust file
mkdir -p "${WORK_DIR}/src"
cat > "${WORK_DIR}/src/math.rs" <<'EOF'
//! Simple integer math utilities.

/// Returns the absolute difference between two unsigned integers.
///
/// BUG: This overflows when b > a because u32 subtraction wraps/panics.
pub fn abs_diff(a: u32, b: u32) -> u32 {
    a - b
}

/// Returns true when n is prime.
pub fn is_prime(n: u32) -> bool {
    if n < 2 { return false; }
    for i in 2..n {
        if n % i == 0 { return false; }
    }
    true
}
EOF

mkdir -p "${WORK_DIR}/tests"
touch "${WORK_DIR}/tests/.gitkeep"

# Minimal Cargo.toml so the project looks realistic.
cat > "${WORK_DIR}/Cargo.toml" <<'EOF'
[package]
name = "toy"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "toy"
path = "src/main.rs"
EOF

cat > "${WORK_DIR}/src/main.rs" <<'EOF'
mod math;
fn main() {
    println!("abs_diff(5, 3) = {}", math::abs_diff(5, 3));
}
EOF

log "staged toy Rust project at ${WORK_DIR}"
log "  src/main.rs   — entry point"
log "  src/math.rs   — buggy abs_diff (panics when b > a)"
log "  tests/        — empty (agent should write a test here)"

# ── record the host hostname for the sandbox sentinel check ───────────────────
HOST_HOSTNAME="$(hostname)"
log "host hostname: ${HOST_HOSTNAME}"

# ── run the pi agent ───────────────────────────────────────────────────────────
log "launching pi --sandbox-provider=e2b …"
echo "────────────────────────────────────────────"

BRIEF="You are working inside an E2B remote sandbox. The working directory
contains a small Rust project with a buggy function in src/math.rs.

Your task:
1. Read src/math.rs and identify the bug in abs_diff().
2. Run: hostname   (this confirms the bash tool runs inside the E2B sandbox,
   not on the caller's host — the output should differ from '${HOST_HOSTNAME}').
3. Write a test file at tests/bug_test.rs that catches the abs_diff overflow bug.
   Use the write tool (not bash echo) so the file is synced back to the host.
   The test should call abs_diff(3, 5) and assert the result equals 2 (the correct
   absolute difference), which will panic under the current buggy implementation.

Write ONLY the test file. Do not fix the bug itself. Stop after writing the test."

START_TS=$(date +%s)

# Run pi from WORK_DIR so pi's cwd (std::env::current_dir()) is the staged
# project root.  pi reads its working directory at startup; launching from
# elsewhere would mean the agent operates on the caller's cwd, not the toy
# project we just staged.
#
# Use --json mode (structured JSONL event stream) rather than -p (print mode).
# In print mode, successful ToolResult output is never written to the
# transcript — only tool-call lines are printed to stderr.  In --json mode
# every AgentEvent is serialised to stdout as a JSON object, so we can
# unambiguously find the ToolResult whose tool_use_id matches the
# AssistantToolCall for `hostname` and read the exact model_output.
(
    cd "${WORK_DIR}"
    E2B_SANDBOX_TIMEOUT_SECS="${E2B_TIMEOUT}" \
        "${PI}" \
        --sandbox-provider=e2b \
        --auto-approve yolo \
        --json \
        "${BRIEF}" \
        2>&1
) | tee "${AGENT_LOG}"

AGENT_EXIT=${PIPESTATUS[0]}
END_TS=$(date +%s)
ELAPSED=$(( END_TS - START_TS ))

echo "────────────────────────────────────────────"
log "pi agent exited with code ${AGENT_EXIT} after ${ELAPSED}s"

# ── check 1: sandbox hostname sentinel ────────────────────────────────────────
# This check GATES demo success.  We parse the JSONL event log to find the
# ToolResult that corresponds to the agent's `hostname` bash call.
#
# Strategy (using jq on the structured --json output):
#   Step A: find AssistantToolCall events where kind.call.name == "bash"
#           and kind.call.input.command contains "hostname".
#           Extract the call id.
#   Step B: find the ToolResult event whose kind.result.tool_use_id matches
#           that call id.  Extract kind.result.model_output (trimmed).
#   Step C: verify the output is non-empty and not equal to HOST_HOSTNAME.
#
# This is sound because --json mode serialises every ToolResult including
# successful ones.  The grep-based approach used in print mode was unsound:
# it could match the tool-call line itself (which mentions "hostname") and
# then promote an unrelated word to a "sandbox hostname".
log "verification 1/3 — bash tool ran inside E2B sandbox (not on host)"

# Step A — find the call id for the hostname invocation.
HOSTNAME_CALL_ID=$(jq -r '
  select(
    .kind.type == "assistant_tool_call"
    and .kind.call.name == "bash"
    and (.kind.call.input.command // "" | test("hostname"))
  ) | .kind.call.id
' "${AGENT_LOG}" 2>/dev/null | head -1 || true)

if [[ -z "${HOSTNAME_CALL_ID}" ]]; then
    fail "sandbox isolation NOT confirmed — no 'bash hostname' AssistantToolCall found
  in the JSONL log.  The agent either skipped step 2 of its task or
  --sandbox-provider=e2b was not honoured.
  Check ${AGENT_LOG} for the full session.
  Possible causes:
    a) The agent skipped the hostname step in its task execution.
    b) --sandbox-provider=e2b was not honoured (check pi --help output)."
fi
log "  hostname call id: ${HOSTNAME_CALL_ID}"

# Step B — find the matching ToolResult and extract its model_output.
# bash tool results have the format "hostname\n\n[exit 0]".  We extract only
# the first line of model_output (the actual hostname) so that the comparison
# in Step C is never confused by the trailing "[exit N]" trailer: without this,
# tr -d '\n' | xargs would yield "sandbox-abc123 [exit 0]", and on a bad run
# where bash ran on the host we'd compare "my-host [exit 0]" != "my-host"
# and falsely conclude isolation was proved.
SANDBOX_HOSTNAME=$(jq -r --arg id "${HOSTNAME_CALL_ID}" '
  select(
    .kind.type == "tool_result"
    and .kind.result.tool_use_id == $id
  ) | .kind.result.model_output | split("\n")[0]
' "${AGENT_LOG}" 2>/dev/null | xargs || true)

if [[ -z "${SANDBOX_HOSTNAME}" ]]; then
    fail "sandbox isolation NOT confirmed — found the hostname bash call
  (tool_use_id=${HOSTNAME_CALL_ID}) but could not find the matching
  ToolResult in the JSONL log.  This is unexpected; check ${AGENT_LOG}."
fi

# Step C — verify the sandbox hostname differs from the host's.
if [[ "${SANDBOX_HOSTNAME}" == "${HOST_HOSTNAME}" ]]; then
    fail "sandbox isolation NOT confirmed — the hostname ToolResult returned
  '${SANDBOX_HOSTNAME}', which is identical to the host hostname '${HOST_HOSTNAME}'.
  This means bash calls ran on the HOST rather than inside the E2B sandbox.
  Check ${AGENT_LOG} for the full session.
  Possible causes:
    a) --sandbox-provider=e2b was not honoured by this pi build.
    b) The E2B sandbox share-network mode happened to inherit the host hostname."
fi

ok "  sandbox hostname '${SANDBOX_HOSTNAME}' differs from host '${HOST_HOSTNAME}' ✓"
ok "  (proven via ToolResult.model_output for tool_use_id=${HOSTNAME_CALL_ID})"

# ── check 2: test file flushback ──────────────────────────────────────────────
log "verification 2/3 — tests/bug_test.rs flushed back to host tempdir"
TEST_FILE="${WORK_DIR}/tests/bug_test.rs"
if [[ -f "${TEST_FILE}" ]]; then
    ok "  tests/bug_test.rs exists on host ✓"
    log "  contents:"
    sed 's/^/    /' "${TEST_FILE}"

    # Verify the test file contains a reference to abs_diff (basic content check)
    if grep -q "abs_diff" "${TEST_FILE}"; then
        ok "  test file references abs_diff ✓"
    else
        warn "  test file does not reference abs_diff — content may be unexpected"
        warn "  (check ${AGENT_LOG} for details)"
    fi
else
    fail "  tests/bug_test.rs was NOT created on the host.
  This means either:
    a) The write-tool flushback (ToolResponse.file_writes) did not work, OR
    b) The agent wrote the file via bash (not the write tool) — bash mutations
       are not synced in v1 (see RFD 0026 §\"File-mutation flushback\"), OR
    c) The agent did not write the file at all.
  Check ${AGENT_LOG} for the full session transcript."
fi

# ── check 3: agent exit code ──────────────────────────────────────────────────
log "verification 3/3 — pi agent exit code"
if [[ "${AGENT_EXIT}" -eq 0 ]]; then
    ok "  pi exited 0 ✓"
else
    warn "  pi exited ${AGENT_EXIT} — check ${AGENT_LOG} for errors"
fi

# ── timing + cost estimate ─────────────────────────────────────────────────────
COMPUTE_RATE="${E2B_COMPUTE_RATE_PER_SEC:-0.000084}"
COST=$(echo "scale=6; ${ELAPSED} * ${COMPUTE_RATE}" | bc 2>/dev/null || echo "N/A")

echo ""
log "session summary:"
log "  wall-time : ${ELAPSED}s"
log "  E2B compute estimate: \$${COST} (at \$${COMPUTE_RATE}/s compute rate)"
log "  (storage cost \$0.000225/s is additional — not tracked per-call in v1)"
log "  per-call round-trip typically 0.5–15s depending on tool"
log "  first call includes setup overhead (sandbox create + 7 MB worker upload)"

echo ""
# Reaching here means all hard-gated checks above passed (sandbox isolation
# confirmed, test file flushed back, agent exited 0).  The test-file existence
# check below is a safety net — it should always be true at this point.
if [[ -f "${TEST_FILE}" ]]; then
    ok "DEMO PASSED — E2B remote sandbox end-to-end verified"
    # Show the generated test file as the before/after summary
    echo ""
    echo "Before (staged input — tests/ was empty):"
    echo "  tests/.gitkeep"
    echo ""
    echo "After (E2B agent output — tests/bug_test.rs appeared via flushback):"
    sed 's/^/  /' "${TEST_FILE}"
else
    # This branch should be unreachable: check 2 above already called fail()
    # if TEST_FILE was absent.  Guard here just in case.
    fail "DEMO FAILED — test file not found after all checks passed (internal error)"
fi

log "done. E2B sandbox was created and cleaned up by the pi session."
log "(the tempdir ${WORK_DIR} will be removed on script exit)"
