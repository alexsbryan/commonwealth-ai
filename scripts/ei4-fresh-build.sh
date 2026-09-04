#!/usr/bin/env bash
# ei4-fresh-build.sh — the EI4-self-describing-atlas instrument (operator-registered 2026-09-04, directive f109a039).
#
# EI4 is about what a pipeline WRITES, not what history left on disk: 25 of 27
# non-SEP atlas dirs on this host predate ei-2/ei-3 and lack ontology.json or a
# seed table, so a disk-wide ratio measures the past. This instrument builds a
# FRESH atom-class atlas from the wessex-hoard recipe into a throwaway corpus id
# on the resident model, then counts the artifacts the atom-class provider
# declares: ontology.json, atoms.json, atoms.lance, edges.csr, atoms_ann.lance.
#   value = present / 5 ; target 1.0 ; no floor.
# Wiki-class artifacts (articles.lance + edges.lance) are ei-7c's done-when, not
# this instrument's. Costs one live-model build (~8 min): a measure-on-landing
# instrument, run by `co-lineage.py measure epistemic-index`, not nightly.
#
# Verdicts (ARCH §18.2): a build that does not finish exits 3 (could-not-judge)
# with the failing step on stderr; a missing daemon exits 3; only a finished
# build yields a value. The throwaway index is LEFT IN PLACE as the artifact
# (`~/.svrnmesh/indexes/<id>/atlas`), so a second read of the same run is
# flagged STATIC by the loader rather than re-counted as fresh.
set -uo pipefail
REPO="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${SOVEREIGN_CLI:-$REPO/target/debug/sovereign-cli}"
SRC="$REPO/sovereign-recipes/wessex-hoard"
ID="wessex-hoard-ei4-$(date -u +%Y%m%dT%H%M%SZ)"
IDX="${SOVEREIGN_DATA_DIR:-$HOME/.svrnmesh}/indexes/$ID/atlas"
export SOVEREIGN_NO_STALE_WARN=1
[[ -x "$BIN" ]] || { echo "ei4: no sovereign-cli at $BIN" >&2; exit 3; }
curl -s -m 5 localhost:9741/v1/models >/dev/null || { echo "ei4: daemon not answering — could-not-judge" >&2; exit 3; }
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
cp -r "$SRC" "$TMP/$ID"
sed -i "s/^id = \"wessex-hoard\"/id = \"$ID\"/" "$TMP/$ID/recipe.toml"
step() { echo "ei4: $1" >&2; shift; "$@" >&2 || { echo "ei4: step failed — could-not-judge" >&2; exit 3; }; }
step "install"     "$BIN" corpus install "$TMP/$ID/recipe.toml" --wait=900
step "enrich init" "$BIN" enrich init "$ID" --from-corpus "$ID"
step "enrich build" "$BIN" enrich build "$ID" --full
"$BIN" corpus status >/dev/null 2>&1 || true   # first read materialises _summary.json v5
present=0
for f in ontology.json atoms.json atoms.lance edges.csr atoms_ann.lance; do
  if [[ -e "$IDX/$f" ]]; then present=$((present+1)); else echo "ei4: MISSING $f" >&2; fi
done
python3 -c "import json;print(json.dumps({'value': $present/5.0, 'artifact': '$IDX', 'corpus_id': '$ID', 'present': $present}))"
