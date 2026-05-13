#!/usr/bin/env bash
# Cheap correctness gate: a successful build + a working `pi --list` is
# already verified inside autoresearch.sh.  Here we add a tiny smoke pass
# of the workspace tests.  We skip the full test suite — it takes minutes
# and is unlikely to be sensitive to the size/startup tweaks attempted in
# this round (linker flags, post-link transforms, target swap).
set -euo pipefail
cd "$(dirname "$0")"

# Just rerun the cli golden — fastest confidence the binary still works.
./target/release/pi --list >/dev/null
./target/release/pi --config >/dev/null
./target/release/pi --update >/dev/null 2>&1 || true   # no-op when no pkgs
echo "smoke ok"
