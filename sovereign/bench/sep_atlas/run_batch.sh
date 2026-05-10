#!/usr/bin/env zsh
# SEP atlas batch driver — hash-mod-2 split across two peers.
#
# Each peer runs this with its own --peer-index (0 or 1). The hash of
# every slug is computed deterministically; only slugs whose hash mod 2
# matches the peer's index are processed locally. Both peers run the
# same script with the same slug list and reach a disjoint cover.
#
# Usage:
#   run_batch.sh --peer-index 0|1 [--slugs FILE] [--limit N] [--dry-run]
#
# Without --slugs: reads slugs from stdin (one per line; whitespace
# tolerant — plays nice with `sovereign enrich sep-ingest --list`).
#
# Idempotent: skips slugs whose `atlas/atoms.json` already exists. Use
# `--force` in your sep-ingest invocation manually if you need to rebuild.

set -euo pipefail

SCLI=${SCLI:-/Users/alexsbryan/dev/commonwealth-ai/sovereign/target/release/sovereign-cli}
SOVEREIGN_HOME=${SOVEREIGN_HOME:-$HOME/.sovereign}
LOG_DIR=${LOG_DIR:-$(dirname -- "$0")/logs}

PEER_INDEX=""
SLUG_FILE=""
LIMIT=""
DRY_RUN=0

usage() {
  sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//'
}

while (( $# > 0 )); do
  case "$1" in
    --peer-index)  PEER_INDEX=$2;  shift 2 ;;
    --slugs)       SLUG_FILE=$2;   shift 2 ;;
    --limit)       LIMIT=$2;       shift 2 ;;
    --dry-run)     DRY_RUN=1;      shift 1 ;;
    -h|--help)     usage; exit 0 ;;
    *) echo "unknown arg: $1" >&2; usage; exit 1 ;;
  esac
done

if [[ "$PEER_INDEX" != "0" && "$PEER_INDEX" != "1" ]]; then
  echo "error: --peer-index must be 0 or 1" >&2; exit 2
fi

mkdir -p "$LOG_DIR"
ts=$(date +%Y%m%d-%H%M%S)
SUCCESS_LOG="$LOG_DIR/peer-$PEER_INDEX-$ts.success.log"
FAIL_LOG="$LOG_DIR/peer-$PEER_INDEX-$ts.fail.log"
SKIP_LOG="$LOG_DIR/peer-$PEER_INDEX-$ts.skip.log"
RUN_LOG="$LOG_DIR/peer-$PEER_INDEX-$ts.run.log"

if [[ -n "$SLUG_FILE" ]]; then
  exec < "$SLUG_FILE"
fi

read_count=0; matched=0; ok=0; fail=0; skipped=0
t_start=$(date +%s)

while IFS= read -r line; do
  # Accept either bare slug per line, OR `sep-ingest --list` rows of
  # "  <count>  <slug>". Skip the "1770 article(s) in /path" header
  # row by requiring slug match SEP's kebab-case shape.
  slug=$(printf '%s' "$line" | awk '{print $NF}')
  [[ -z "$slug" || "$slug" == \#* ]] && continue
  # SEP slugs are alphanumeric+`-_`. Reject paths/header lines.
  if ! [[ "$slug" =~ ^[A-Za-z0-9][A-Za-z0-9_-]*$ ]]; then continue; fi
  read_count=$((read_count+1))

  # SHA-256 → first 8 hex chars → int → mod 2. Stable across machines.
  bucket=$(python3 -c "import hashlib,sys; print(int(hashlib.sha256(sys.argv[1].encode()).hexdigest()[:8],16)%2)" "$slug")
  if (( bucket != PEER_INDEX )); then continue; fi

  matched=$((matched+1))
  if [[ -n "$LIMIT" && $matched -gt $LIMIT ]]; then break; fi

  atlas_path="$SOVEREIGN_HOME/indexes/sep-$slug/atlas/atoms.json"
  if [[ -f "$atlas_path" ]]; then
    echo "  · $slug (skip — atlas exists)"
    echo "$slug" >> "$SKIP_LOG"
    skipped=$((skipped+1))
    continue
  fi

  if (( DRY_RUN )); then
    echo "  → $slug (dry-run)"
    continue
  fi

  echo "→ $slug"
  t0=$(date +%s)
  if "$SCLI" enrich sep-ingest "$slug" >>"$RUN_LOG" 2>&1 \
     && "$SCLI" enrich build "sep-$slug" >>"$RUN_LOG" 2>&1; then
    dt=$(($(date +%s) - t0))
    echo "  ✓ $slug (${dt}s)"
    echo "$slug ${dt}s" >> "$SUCCESS_LOG"
    ok=$((ok+1))
  else
    dt=$(($(date +%s) - t0))
    echo "  ✗ $slug (${dt}s) — see $RUN_LOG"
    echo "$slug ${dt}s" >> "$FAIL_LOG"
    fail=$((fail+1))
  fi
done

dt_total=$(($(date +%s) - t_start))
echo
echo "summary peer=$PEER_INDEX read=$read_count bucket-matched=$matched ok=$ok fail=$fail skipped=$skipped wall=${dt_total}s"
echo "logs: $SUCCESS_LOG $FAIL_LOG $SKIP_LOG $RUN_LOG"
