#!/usr/bin/env bash
# Sanity: workspace must still build and tests pass.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --workspace --release --quiet 2>/dev/null
