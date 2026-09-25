#!/usr/bin/env bash
# ei7 atlas rebuild with the ref-prompt fix (2026-09-22): extract re-runs over
# all sections (local CLI carries the new prompt; pod daemon serves the model),
# then build, then the ref census + structural K1 test. NO arms run here —
# this is the mechanism instrument, not a study.
set -uo pipefail
cd /Users/alexsbryan/dev/commonwealth-ai
eval "$(scripts/dev-pod.sh env)"
say() { printf '\n== %s  [%s]\n' "$1" "$(date -u +%T)"; }
CORPUS=ei7-ans
INDEX_DIR="${SOVEREIGN_DATA_DIR:-$HOME/.svrnmesh}/indexes/$CORPUS"
RECIPE=research/ontology-retrieval/ontology-proof/ans/recipe.toml

say "wait for pod daemon"
until curl -s -o /dev/null --max-time 3 "$SOVEREIGN_DAEMON_URL/v1/models"; do sleep 20; done

say "backup old atlas (pre-fix evidence)"
[ -d "$INDEX_DIR/atlas.pre-ref-fix" ] || cp -R "$INDEX_DIR/atlas" "$INDEX_DIR/atlas.pre-ref-fix"

say "extract"
rm -rf "$INDEX_DIR/atlas"
sovereign enrich reset "$CORPUS" --full --yes >/dev/null 2>&1 || true
sovereign enrich init "$CORPUS" --from-corpus "$CORPUS" --force || { echo "init failed"; exit 2; }
sovereign enrich extract "$CORPUS" --full --resume || echo "extract exits nonzero on skips; finalize decides" >&2
sovereign enrich extract "$CORPUS" --finalize || { echo "finalize failed"; exit 3; }

say "build"
sovereign enrich build "$CORPUS" --skip extract --skip tensions || { echo "build failed"; exit 4; }
[ -f "$INDEX_DIR/atlas/ontology.json" ] || { echo "no ontology.json"; exit 5; }

say "ref census (post-fix)"
python3 research/ontology-retrieval/ontology-proof/ans/ref_census.py \
  > target/ref-census-post-fix.json 2> target/ref-census-post-fix.stderr
cat target/ref-census-post-fix.stderr

say "teardown"
scripts/dev-pod.sh down 2>&1 | tail -1
echo "REBUILD DONE  [$(date -u +%T)]"
