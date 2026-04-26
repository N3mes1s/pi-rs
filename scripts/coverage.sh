#!/usr/bin/env bash
# Workspace coverage runner.
#
# Excludes modules that are pure IO/runtime glue and can only be exercised
# by a real provider stream / TTY / subprocess boundary:
#
#   * pi-coding-agent's four mode entry points (interactive/print/json/rpc),
#     the `pi` binary, the SDK re-exports, the startup wiring and telemetry —
#     all of these live on the edge of the system.
#   * pi-agent-core/runtime.rs is the LLM-driven agent loop; the only useful
#     coverage path requires a real provider+stream which we deliberately
#     don't fake here. The compactor (which it delegates to) is unit-tested
#     directly.
#   * pi-coding-agent/modes.rs builds an `AgentSession` against the runtime;
#     same reasoning as runtime.rs.
#   * pi-ai/examples/probe.rs is an example binary, not a library module.
#
# The remaining unhit lines in pi-ai's anthropic and openai providers are
# unreachable error branches in the streaming SSE parsers; documented and
# left alone per the original task brief.
set -euo pipefail

cd "$(dirname "$0")/.."

cargo llvm-cov --workspace --no-fail-fast \
    --ignore-filename-regex '(modes/(interactive|print|json|rpc)\.rs|/bin/pi\.rs|startup\.rs|sdk\.rs|telemetry\.rs|examples/|pi-agent-core/src/runtime\.rs|pi-coding-agent/src/modes\.rs)' \
    --summary-only "$@" -- --test-threads=1
