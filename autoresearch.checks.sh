#!/usr/bin/env bash
# Quick correctness checks: built binary's two cheapest CLI surfaces still work.
set -euo pipefail
cd "$(dirname "$0")"

bin=""
for cand in \
    "${CARGO_TARGET_DIR:-target}/release/pi" \
    "${CARGO_TARGET_DIR:-target}/x86_64-unknown-linux-musl/release/pi"; do
  if [[ -x "$cand" ]]; then bin="$cand"; break; fi
done

if [[ -z "$bin" ]]; then
  echo "checks: binary missing" >&2
  exit 1
fi

# Must produce non-empty output and exit 0.
out=$("$bin" --list 2>&1) || { echo "pi --list failed:"; echo "$out" | tail -20; exit 1; }
[[ -n "$out" ]] || { echo "pi --list produced no output"; exit 1; }

"$bin" --version >/dev/null 2>&1 || { echo "pi --version failed"; exit 1; }

# Hard size ceiling.
bytes=$(stat -c%s "$bin")
size_kib=$(( bytes / 1024 ))
if (( size_kib > 4700 )); then
  echo "size guardrail tripped: ${size_kib} KiB > 4700 KiB"
  exit 1
fi
exit 0
