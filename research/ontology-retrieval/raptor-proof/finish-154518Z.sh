#!/usr/bin/env bash
# Finish window 20260921T154518Z after its processes were killed by another
# session mid full-run-3: only the missing arm runs, then the board. No rm -rf.
set -uo pipefail
cd /Users/alexsbryan/dev/commonwealth-ai
eval "$(scripts/dev-pod.sh env)"
S=pilot-and-his-wife; H=research/ontology-retrieval/raptor-proof; RUNS=$H/runs/$S
arm() { python3 research/ontology-retrieval/harness/run_arm.py --arm "$1" --bank $H/bank-$S.toml --corpus raptor-$S \
  --index-dir "$HOME/.svrnmesh/indexes/raptor-$S" --recipe $H/recipes/$S/recipe.toml --out $RUNS --run "$2" --pool-scale 2; }
for pair in "full 3" "deep 3"; do set -- $pair
  if [ -f "$RUNS/$1/run-$2/manifest.json" ] && [ -f "$RUNS/$1/run-$2/eval.json" ]; then echo "== $1 run $2 already complete"; continue; fi
  echo "== arm $1 run $2  [$(date -u +%T)]"; rm -rf "$RUNS/$1/run-$2"; arm "$1" "$2"
done
echo "== board  [$(date -u +%T)]"
python3 sovereign/bench/sep_atlas/map-conversion-rung6/compare.py study --runs $RUNS --out $RUNS --recipe $H/recipes/$S/recipe.toml
echo "FINISH EXIT $?  [$(date -u +%T)]"
