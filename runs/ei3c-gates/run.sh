#!/usr/bin/env bash
# ei3c-gates — the mechanical gates for order ei-3c-instruments.
#
# This branch changes no Rust: the diff is corpus-mcp/acceptance.sh, two files
# under scripts/, quality/campaigns/epistemic-index.toml, corpus-mcp/README.md,
# one cell of the spec, and the run unit. So the test sweep is scoped to
# `corpus-mcp`, the one crate whose directory the diff touches, and that scope
# is PRINTED by the script's own banner rather than claimed here.
#
# Both legs need the build lock and the toolbox; the caller supplies both.
# Markers and DONE follow the same contract as the Ollama unit: out/ is reset
# first, every leg writes its rc, DONE is written on every exit path.
set -uo pipefail
RUN_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd -- "$RUN_DIR/../.." && pwd)"
OUT="$RUN_DIR/out"
rm -rf "$OUT"; mkdir -p "$OUT"
: > "$OUT/markers.txt"
mark() { echo "$1 rc=$2" >> "$OUT/markers.txt"; echo "$2" > "$OUT/rc.$1"; echo "== leg $1 -> $2" >&2; }
finish() {
  rc=$?
  { echo "finished: $(date -Is)"; free -g | head -2; df -h /home | tail -1; } > "$OUT/box-after.txt" 2>&1
  echo "DONE rc=$rc" >> "$OUT/markers.txt"
  echo "DONE rc=$rc $(date -Is)" > "$OUT/DONE"
}
trap finish EXIT
trap 'echo "signal=TERM" >> "$OUT/markers.txt"; exit 143' TERM
trap 'echo "signal=INT"  >> "$OUT/markers.txt"; exit 130' INT

{ echo "started: $(date -Is)"
  echo "container: $(head -1 /run/.containerenv 2>/dev/null || echo HOST)"
  echo "tip: $(git -C "$REPO" rev-parse HEAD)"
  free -g | head -2; df -h /home | tail -1
} > "$OUT/box-before.txt" 2>&1

[[ -f /run/.containerenv ]] || { echo "PREFLIGHT: must run INSIDE sovereign-vulkan" >&2; mark preflight 1; exit 2; }
mark preflight 0

cd "$REPO"
./scripts/sovereign-test.sh --human --package corpus-mcp > "$OUT/test.log" 2>&1
mark test $?
tail -20 "$OUT/test.log"

./scripts/pre-push.sh > "$OUT/pre-push.log" 2>&1
mark pre-push $?
tail -40 "$OUT/pre-push.log"
