#!/usr/bin/env bash
# Workspace coverage runner. Excludes IO/runtime glue modules that
# cannot be meaningfully line-covered without a real TTY/network/process
# boundary; their behaviour is exercised via integration/dogfooding instead.
set -euo pipefail

cd "$(dirname "$0")/.."

cargo llvm-cov --workspace \
    --ignore-filename-regex '(modes/(interactive|print|json|rpc)\.rs|/bin/pi\.rs|startup\.rs|sdk\.rs|telemetry\.rs|examples/)' \
    --summary-only "$@"
