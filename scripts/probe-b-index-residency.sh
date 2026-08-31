#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# PROBE B — order `mesh-scale-t0`, MESH_SCALE_100_USERS_1000_CORPORA.md §8.
#
# ONE QUESTION: what does an installed corpus cost in resident memory once
# the hourly maintenance sweep has opened it? That per-handle number exists
# nowhere in the tree, which is why §7.2 had to refuse the index-handle LRU
# on principle instead of on arithmetic.
#
# HOW: clone one tiny REAL index N times into a THROWAWAY directory, then
# run the sweep over all N in both arms —
#   pinned    = pre-fix `open_index` (admits every handle to the query cache)
#   transient = post-fix `open_index_transient`
# — and report RSS after the sweep, resident handle count, sweep wall time,
# and per-query wall time for each arm.
#
# SAFETY: the source index is read ONLY. Everything written goes under a
# throwaway dir that this script creates and (unless --keep) removes. The
# probe itself refuses to run against a path ending in `.svrnmesh/indexes`.
#
# Usage:
#   scripts/probe-b-index-residency.sh [--corpora N] [--source DIR] [--keep]
#
# Defaults: N=1000, source = the smallest real index in ~/.svrnmesh/indexes.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CORPORA=1000
SOURCE=""
KEEP=0
WORK="${TMPDIR:-/tmp}/probe-b-$$"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --corpora) CORPORA="$2"; shift 2 ;;
    --source)  SOURCE="$2"; shift 2 ;;
    --keep)    KEEP=1; shift ;;
    -h|--help) sed -n '3,26p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done

if [[ -z "$SOURCE" ]]; then
  SOURCE="$HOME/.svrnmesh/indexes/folder-df-fix-drive-4769b5117dd2"
fi
if [[ ! -d "$SOURCE/chunks.lance" ]]; then
  echo "probe-b: --source must be an installed index dir containing chunks.lance (got $SOURCE)" >&2
  exit 2
fi

# The throwaway home. NEVER the operator's indexes dir — the probe asserts
# this too, but a script that can only ever produce a safe path is better
# than one that relies on the assertion.
INDEX_DIR="$WORK/indexes"
mkdir -p "$INDEX_DIR"
cleanup() { [[ "$KEEP" == 1 ]] || rm -rf "$WORK"; }
trap cleanup EXIT

DIMS="$(python3 -c "import json,sys;print(json.load(open(sys.argv[1]))['embedding_dimensions'])" \
        "$SOURCE/_corpus_meta.json")"

echo "probe-b: source        $SOURCE"
echo "probe-b: dimensions    $DIMS"
echo "probe-b: throwaway     $INDEX_DIR"
echo "probe-b: cloning $CORPORA copies…"
for ((i = 0; i < CORPORA; i++)); do
  cp -r "$SOURCE" "$INDEX_DIR/probe-$(printf '%04d' "$i")"
done
# Each clone needs its own corpus_id or `dedupe_by_corpus_id` collapses
# them all into one and the sweep measures a single corpus. One pass, not
# one python per clone.
python3 - "$INDEX_DIR" <<'PY'
import json, os, sys
root = sys.argv[1]
for name in os.listdir(root):
    meta = os.path.join(root, name, "_corpus_meta.json")
    if not os.path.exists(meta):
        continue
    m = json.load(open(meta))
    m["corpus_id"] = name
    json.dump(m, open(meta, "w"))
PY
echo "probe-b: cloned. on-disk $(du -sh "$INDEX_DIR" | cut -f1)"

# Build once so the two arms are not separated by a compile.
cd "$REPO_ROOT"
cargo test -p corpus-engine --test main probe_index_residency --no-run --quiet

for MODE in pinned transient; do
  echo
  echo "── arm: $MODE ─────────────────────────────────────────"
  PROBE_INDEX_DIR="$INDEX_DIR" PROBE_MODE="$MODE" PROBE_EMBED_DIMS="$DIMS" \
    cargo test -p corpus-engine --test main probe_index_residency --quiet \
      -- --ignored --nocapture probe_b_index_handle_residency 2>&1 \
    | grep -E '^PROBE_B_' || {
        echo "probe-b: arm $MODE produced no result line — the probe did not run" >&2
        exit 1
      }
done

echo
echo "probe-b: done. Record both arms in MESH_SCALE_100_USERS_1000_CORPORA.md §8."
