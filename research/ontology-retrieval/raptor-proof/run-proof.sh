#!/usr/bin/env bash
# ei7-prove-raptor: ONE book, NarrativeQA's own questions, bare / deep / full.
#
#   research/ontology-retrieval/raptor-proof/run-proof.sh <slug>
#
# Runs against whatever daemon SOVEREIGN_DAEMON_URL names (pod_window.sh
# exports the pod's). Order matters and each step gates the next:
#   1. closed-book x1. Mean judge >= 0.5 -> exit 10: the model knows the book,
#      nothing after this would prove anything, and no build is paid for.
#   2. atlas + RAPTOR tree + Summary atoms (the same verbs that built
#      chaos-secret-agent's 19 Summary atoms). Exit 11 if no Summary atom lands.
#   3. bare / deep / full x3, then compare.py study.
# Modelled on pilot/run-pilot.sh; the arms, manifests and scorer are harness/.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../../.."
SLUG="${1:?usage: run-proof.sh <slug>}"
HERE=research/ontology-retrieval/raptor-proof
CORPUS="raptor-$SLUG"
BANK="$HERE/bank-$SLUG.toml"
RECIPE="$HERE/recipes/$SLUG/recipe.toml"
BOOK="$PWD/$HERE/books/$SLUG.txt"
INDEX_DIR="${SOVEREIGN_DATA_DIR:-$HOME/.svrnmesh}/indexes/$CORPUS"
RUNS="$HERE/runs/$SLUG"
RUN_ARM=research/ontology-retrieval/harness/run_arm.py
COMPARE=sovereign/bench/sep_atlas/map-conversion-rung6/compare.py
SVRN="${SVRN:-$(command -v svrn || command -v sovereign)}"
RUNS_PER_ARM="${RAPTOR_RUNS:-3}"
step() { printf '\n== %s  [%s]\n' "$1" "$(date -u +%T)"; }
die()  { echo "run-proof: $*" >&2; exit "${2:-1}"; }
for f in "$BANK" "$RECIPE" "$BOOK"; do [ -f "$f" ] || die "missing $f" 2; done
[ -d "$INDEX_DIR" ] || die "corpus $CORPUS is not installed (svrn corpus install $RECIPE)" 2

arm() { python3 "$RUN_ARM" --arm "$1" --bank "$BANK" --corpus "$CORPUS" \
          --index-dir "$INDEX_DIR" --recipe "$RECIPE" --out "$RUNS" \
          --run "$2" --pool-scale "${POOL_SCALE:-2}" || die "arm $1 run $2 refused its premises"; }

rm -rf "$RUNS"
step "closed-book screen"
arm closed-book 1
python3 - "$RUNS/closed-book/run-1" <<'PY' || exit $?
import json, sys
from pathlib import Path
d = Path(sys.argv[1])
if json.loads((d / "manifest.json").read_text()).get("verdict") == "never-ran":
    print("closed-book never-ran; see its manifest", file=sys.stderr); raise SystemExit(3)
rows = json.loads((d / "eval.json").read_text())["results"]
s = [((r.get("synth") or {}).get("judge_fact_score") or {}).get("ratio") for r in rows]
s = [x for x in s if x is not None]
mean = sum(s) / len(s) if s else None
print(f"closed-book: n={len(s)} mean judge={mean if mean is None else round(mean, 3)} "
      f"known(>=0.5)={sum(1 for x in s if x >= 0.5)}")
if mean is None: raise SystemExit(3)
if mean >= 0.5:
    print("the model knows this book: nothing downstream would prove anything", file=sys.stderr)
    raise SystemExit(10)
PY

step "atlas"
# `literary_atlas`, as chaos-secret-agent's config.json records. The default
# `literary` is a legacy non-atlas pipeline that `enrich build` refuses — window
# 20260921T065349Z paid for a pod to learn that.
"$SVRN" enrich reset "$CORPUS" --full --yes >/dev/null 2>&1 || true   # a local rehearsal pins this host's daemon into config.json
"$SVRN" enrich init "$CORPUS" --source "$BOOK" --pipeline literary_atlas --force || die "enrich init failed"
# `extract` exits 1 when it SKIPS a too-short section (DECISIONS A38); --finalize
# is the step that has to succeed.
"$SVRN" enrich extract "$CORPUS" --full --resume || echo "run-proof: extract exited $? (skips count as failures; finalize decides)" >&2
"$SVRN" enrich extract "$CORPUS" --finalize || die "extract --finalize failed"
"$SVRN" enrich build "$CORPUS" --skip extract --skip tensions || die "enrich build failed"
step "raptor tree + Summary atoms"
# --force: `raptor` skips documents already built, and a local rehearsal leaves a
# tree summarised by THIS host's model. One model per board (DECISIONS A38).
"$SVRN" enrich raptor "$CORPUS" --doc-type narrative --force || die "enrich raptor failed"
"$SVRN" enrich summary-atoms "$CORPUS" || die "enrich summary-atoms failed"
python3 - "$INDEX_DIR/atlas/atoms.json" <<'PY' || exit 11
import json, sys
a = json.load(open(sys.argv[1])); a = a.get("atoms", a) if isinstance(a, dict) else a
n = sum(1 for x in a if str(x.get("kind") or x.get("atom_type") or "").lower() == "summary")
print(f"atoms={len(a)} summary={n}")
raise SystemExit(0 if n else 1)
PY

for r in $(seq 1 "$RUNS_PER_ARM"); do
  for a in bare full deep; do   # deep last: if a meter runs out, it is the arm the verdict can best spare
  step "arm $a run $r"; arm "$a" "$r"; done
done

step "board"
python3 "$COMPARE" study --runs "$RUNS" --out "$RUNS" --recipe "$RECIPE"
