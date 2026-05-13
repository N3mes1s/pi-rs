#!/usr/bin/env bash
# Round-4 benchmark: startup_us + size_kib. The session-level autoresearch.sh
# at repo root drives the cargo build; this file is a copy for reference.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release -p pi-coding-agent 2>/dev/null

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
