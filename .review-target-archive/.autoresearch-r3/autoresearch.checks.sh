#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --workspace --release --quiet 2>/dev/null
cargo test --workspace --quiet --no-fail-fast 2>&1 | grep -qE "test result: ok\." || exit 1
