#!/usr/bin/env bash
# Round-4 benchmark: startup_us + size_kib (lower = better) on the final
# binary at ./target/release/pi. Some experiments may post-process (UPX,
# extra strip) the binary inside this script — see the variables at top.
set -euo pipefail
cd "$(dirname "$0")"

# ----- knobs (experiments may flip these) ---------------------------------
# Set TARGET to switch toolchain, e.g. x86_64-unknown-linux-musl.
TARGET="${PI_AR_TARGET:-}"
# Set POSTBUILD to a script snippet that mutates ./target/release/pi
# in-place after build.  Example:  POSTBUILD='upx --best -q'
POSTBUILD="${PI_AR_POSTBUILD:-}"
# --------------------------------------------------------------------------

if [[ -n "$TARGET" ]]; then
  cargo build --release --target "$TARGET" -p pi-coding-agent 2>/dev/null
  BIN="./target/${TARGET}/release/pi"
  # Mirror to ./target/release/pi so existing tooling keeps working.
  install -m 755 "$BIN" ./target/release/pi
else
  cargo build --release -p pi-coding-agent 2>/dev/null
fi

if [[ -n "$POSTBUILD" ]]; then
  bash -c "$POSTBUILD ./target/release/pi" >/dev/null 2>&1 || true
fi

# warm fs cache & verify the binary actually runs (cheap correctness check)
./target/release/pi --list >/dev/null 2>&1

# Median of 5 trials, each = mean of 200 sequential invocations.
declare -a samples
for trial in 1 2 3 4 5; do
  start=$(date +%s%N)
  for i in $(seq 1 200); do ./target/release/pi --list >/dev/null 2>&1; done
  end=$(date +%s%N)
  samples+=($(( (end - start) / 1000 / 200 )))
done
sorted=$(printf '%s\n' "${samples[@]}" | sort -n)
startup_us=$(echo "$sorted" | sed -n '3p')

bytes=$(stat -c%s ./target/release/pi)
size_kib=$(( bytes / 1024 ))

score=$(( startup_us + size_kib ))

echo "samples (us): ${samples[*]}"
echo "METRIC startup_us=${startup_us}"
echo "METRIC size_kib=${size_kib}"
echo "METRIC score=${score}"
