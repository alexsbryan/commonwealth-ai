#!/usr/bin/env bash
# One solo pod, three jobs, every step resumable (another session killed a run
# mid-arm on 2026-09-21): essay arms -> essay judge -> ANS proof. Nothing here
# deletes finished work. Tears the pod down itself and reads the status back.
#   setsid/nohup this; log = target/ralph/pod-session.log
set -uo pipefail
cd /Users/alexsbryan/dev/commonwealth-ai
eval "$(scripts/dev-pod.sh env)"
say() { printf '\n== %s  [%s]\n' "$1" "$(date -u +%T)"; }
S=pilot-and-his-wife; H=research/ontology-retrieval/raptor-proof
ERUNS=$H/runs-essay/$S; EBANK=$H/essay/bank-$S.toml; EOUT=$H/essay/board-$S
arm() { if [ -f "$ERUNS/$1/run-$2/manifest.json" ] && [ -f "$ERUNS/$1/run-$2/eval.json" ]; then echo "on disk: $1 run $2"; return 0; fi
  rm -rf "$ERUNS/$1/run-$2"
  python3 research/ontology-retrieval/harness/run_arm.py --arm "$1" --bank $EBANK --corpus raptor-$S \
    --index-dir "$HOME/.svrnmesh/indexes/raptor-$S" --recipe $H/recipes/$S/recipe.toml --out $ERUNS --run "$2" --pool-scale 2; }
say "essay arm closed-book run 1"; arm closed-book 1
for r in 1 2 3; do for a in bare full deep; do say "essay arm $a run $r"; arm $a $r; done; done
say "essay judge"
python3 $H/essay/essay_judge.py --runs $ERUNS --bank $EBANK --daemon "$SOVEREIGN_DAEMON_URL" \
  --model commonwealth/primary --out $EOUT --seed 20260921 --workers 2
echo "ESSAY JUDGE EXIT $?"
say "ANS proof"
RESUME=1 research/ontology-retrieval/ontology-proof/ans/run-proof.sh
echo "ANS EXIT $?"
say "teardown"
scripts/dev-pod.sh down 2>&1 | tail -2; scripts/dev-pod.sh status 2>&1 | tail -1
echo "SESSION DONE  [$(date -u +%T)]"
