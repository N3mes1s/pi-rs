#!/usr/bin/env bash
# Sanity: full workspace must build + test (don't ship a fast-but-broken binary).
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --workspace --release --quiet 2>/dev/null
cargo test --workspace --quiet --no-fail-fast 2>&1 | grep -qE "test result: ok\." || exit 1
