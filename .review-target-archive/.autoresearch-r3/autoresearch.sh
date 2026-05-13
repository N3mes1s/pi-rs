#!/usr/bin/env bash
# Composite benchmark: startup_us + size_kib (lower is better, both rebuilt clean per run).
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release -p pi-coding-agent 2>/dev/null

# warm
./target/release/pi --list >/dev/null 2>&1 || true

start=$(date +%s%N)
for i in $(seq 1 200); do ./target/release/pi --list >/dev/null 2>&1; done
end=$(date +%s%N)
startup_us=$(( (end - start) / 1000 / 200 ))

bytes=$(stat -c%s ./target/release/pi)
size_kib=$(( bytes / 1024 ))

score=$(( startup_us + size_kib ))

echo "METRIC startup_us=${startup_us}"
echo "METRIC size_kib=${size_kib}"
echo "METRIC score=${score}"
