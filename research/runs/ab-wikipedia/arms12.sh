#!/usr/bin/env bash
# ei-7c A/B arms 1 and 2, against arm 0's committed rows.
#
#   arm 1 — the v2 wiki store, walk, NO ANN seed table
#   arm 2 — the same store WITH the borrowed seed table (atoms_ann.lance)
#
# The two arms differ by ONE file. Both atlas dirs are symlink farms over the
# SAME rebuilt store, so nothing but the seed table's presence can vary, and
# `wikipedia-ei7c`'s chunks.lance is a symlink to the installed wikipedia's —
# the retrieval substrate is byte-identical to arm 0's.
#
# Mode is `--prod-pipeline --isolate`, the ONLY mode whose pipeline contains
# `atlas_grounding`; the default retrieval mode is arm 0's negative control and
# cannot tell the arms apart by construction (eval_cmd/mod.rs:157).
#
# RUST_LOG keeps `walk provider: opened` at info. That line prints
# `backend=wiki-class` and `seed_table=true|false`, which is the per-run PROOF
# that the arm was the arm it claims — the store path alone is not, since a
# store that failed to open reports nothing and falls to bag-of-atoms.
set -uo pipefail
WT=/home/alexbryan/dev/ei7c-wt
OUT=$WT/runs/ab-wikipedia
FIX=$HOME/.svrnmesh/indexes/wikipedia-ei7c
BANK=$OUT/questions-ei7c.toml
cd "$WT"
mkdir -p "$OUT/markers"
export RUST_LOG=corpus_engine::enrichment::atlas::provider=info,corpus_engine::enrichment::atlas::ground=info,warn

for arm in 1 2; do
  ln -sfn "$OUT/atlas-arm$arm" "$FIX/atlas"
  echo "arm $arm: atlas -> $(readlink -f "$FIX/atlas")" >&2
  ls -1 "$FIX/atlas/" >&2
  for i in 1 2; do
    echo "=== arm$arm run $i ($(date -Is)) ===" >&2
    ./target/debug/sovereign-cli eval run \
      --bank "$BANK" \
      --prod-pipeline --isolate \
      --format json --output "$OUT/arm$arm-run$i.json" \
      > "$OUT/arm$arm-run$i.txt" 2> "$OUT/arm$arm-run$i.err"
    echo "$?" > "$OUT/markers/arm$arm-run$i.rc"
  done
done
date -Is > "$OUT/markers/ARMS12_DONE"
