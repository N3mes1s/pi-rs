#!/usr/bin/env bash
# Benchmark pi-rs cold-start. Builds release if needed, then averages
# 200 invocations of `pi --list` (a no-network startup path).
set -euo pipefail
cd "$(dirname "$0")/.."

# Build silently. Fail loud.
cargo build --release -p pi-coding-agent 2>/dev/null

start=$(date +%s%N)
for i in $(seq 1 200); do ./target/release/pi --list >/dev/null 2>&1; done
end=$(date +%s%N)
per_us=$(( (end - start) / 1000 / 200 ))
echo "METRIC startup_us=${per_us}"
