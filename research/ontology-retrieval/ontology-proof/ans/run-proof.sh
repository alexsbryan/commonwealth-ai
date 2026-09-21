#!/usr/bin/env bash
# ei7-prove-ontology: ANS numismatics (nine works), list questions whose answer
# key is CoinHoards/IGCH, bare / deep / full.
#
#   research/ontology-retrieval/ontology-proof/ans/run-proof.sh
#
# Same shape as raptor-proof/run-proof.sh (one driver per proof; the arms,
# manifests and scorer are harness/). Differences: the corpus carries a DECLARED
# ontology, so init is `--from-corpus` and the pipeline comes from the recipe
# (custom_atlas, as ei7-recensus-fineprint's config.json records); there is no
# RAPTOR step; closed-book does not gate the build, because exclusions are per
# question and the board applies them.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../../../.."
HERE=research/ontology-retrieval/ontology-proof/ans
CORPUS=ei7-ans
BANK="$HERE/bank.attested.toml"
RECIPE="$HERE/recipe.toml"
INDEX_DIR="${SOVEREIGN_DATA_DIR:-$HOME/.svrnmesh}/indexes/$CORPUS"
RUNS="$HERE/runs"
RUN_ARM=research/ontology-retrieval/harness/run_arm.py
COMPARE=sovereign/bench/sep_atlas/map-conversion-rung6/compare.py
SVRN="${SVRN:-$(command -v svrn || command -v sovereign)}"
RUNS_PER_ARM="${ANS_RUNS:-3}"
step() { printf '\n== %s  [%s]\n' "$1" "$(date -u +%T)"; }
die()  { echo "run-proof: $*" >&2; exit "${2:-1}"; }
for f in "$BANK" "$RECIPE"; do [ -f "$f" ] || die "missing $f" 2; done
[ -d "$INDEX_DIR" ] || die "corpus $CORPUS is not installed (svrn corpus install $RECIPE)" 2

arm() { python3 "$RUN_ARM" --arm "$1" --bank "$BANK" --corpus "$CORPUS" \
          --index-dir "$INDEX_DIR" --recipe "$RECIPE" --out "$RUNS" \
          --run "$2" --pool-scale "${POOL_SCALE:-2}" || die "arm $1 run $2 refused its premises"; }

rm -rf "$RUNS"
step "atlas"
# `enrich reset` leaves <index>/atlas in place and `build` then skips resolve as
# cached (raptor window 20260921T153541Z). The chunks are not under atlas/.
rm -rf "$INDEX_DIR/atlas"
"$SVRN" enrich reset "$CORPUS" --full --yes >/dev/null 2>&1 || true   # a local rehearsal pins this host's daemon into config.json
"$SVRN" enrich init "$CORPUS" --from-corpus "$CORPUS" --force || die "enrich init failed"
# `extract` exits 1 when it SKIPS a too-short section (DECISIONS A38); --finalize
# is the step that has to succeed.
"$SVRN" enrich extract "$CORPUS" --full --resume || echo "run-proof: extract exited $? (skips count as failures; finalize decides)" >&2
"$SVRN" enrich extract "$CORPUS" --finalize || die "extract --finalize failed"
"$SVRN" enrich build "$CORPUS" --skip extract --skip tensions || die "enrich build failed"
[ -f "$INDEX_DIR/atlas/ontology.json" ] || die "no atlas/ontology.json: the build was not a declared-ontology build" 11

step "arm closed-book run 1"; arm closed-book 1
for r in $(seq 1 "$RUNS_PER_ARM"); do
  for a in bare full deep; do step "arm $a run $r"; arm "$a" "$r"; done
done

step "board"
python3 "$COMPARE" study --runs "$RUNS" --out "$RUNS" --recipe "$RECIPE"
