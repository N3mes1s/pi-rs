#!/usr/bin/env bash
# Round-5 benchmark: clean-build wall-clock for `cargo build --release -p pi-coding-agent`.
#
# Emits:
#   METRIC build_s=<float seconds, clean build>
#   METRIC size_kib=<int KiB, stripped final binary>
#   METRIC startup_us=<int µs, mean over 50 `pi --list` runs>  (informational)
#
# Wipes target/ so every run is a cold build. CARGO_TARGET_DIR is honoured if
# pre-set (useful for CI), otherwise defaults to ./target.
set -euo pipefail
cd "$(dirname "$0")"

TARGET_DIR="${CARGO_TARGET_DIR:-target}"

# 1. Clean.
rm -rf "$TARGET_DIR"

# 2. Time the build. We use bash's $SECONDS-style high-res via `date +%s%N`.
start=$(date +%s%N)
cargo build --release -p pi-coding-agent 2>&1 | tail -3
end=$(date +%s%N)
build_ns=$(( end - start ))
# float seconds with 2 decimals
build_s=$(awk -v ns="$build_ns" 'BEGIN { printf "%.2f", ns/1e9 }')

# 3. Locate the produced binary. With the musl target in .cargo/config.toml,
#    cargo writes to target/x86_64-unknown-linux-musl/release/pi.
bin=""
for cand in \
    "$TARGET_DIR/x86_64-unknown-linux-musl/release/pi" \
    "$TARGET_DIR/release/pi"; do
  if [[ -x "$cand" ]]; then bin="$cand"; break; fi
done
if [[ -z "$bin" ]]; then
  echo "ERROR: built binary not found under $TARGET_DIR" >&2
  exit 1
fi

# 4. Apply the post-link strip the Makefile uses, so the size we report is
#    representative of what ships.
mkdir -p "$TARGET_DIR/release"
install -m 755 "$bin" "$TARGET_DIR/release/pi"
objcopy --remove-section=.eh_frame \
        --remove-section=.eh_frame_hdr \
        --remove-section=.gcc_except_table \
        "$TARGET_DIR/release/pi" 2>/dev/null || true
final="$TARGET_DIR/release/pi"

bytes=$(stat -c%s "$final")
size_kib=$(( bytes / 1024 ))

# 5. Sanity startup (informational, not a gate). 50 invocations is enough
#    to spot order-of-magnitude regressions without dominating runtime.
"$final" --list >/dev/null 2>&1 || true
s=$(date +%s%N)
for _ in $(seq 1 50); do "$final" --list >/dev/null 2>&1; done
e=$(date +%s%N)
startup_us=$(( (e - s) / 1000 / 50 ))

echo "METRIC build_s=${build_s}"
echo "METRIC size_kib=${size_kib}"
echo "METRIC startup_us=${startup_us}"
