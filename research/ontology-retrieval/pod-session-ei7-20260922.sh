#!/usr/bin/env bash
# ei7 closing session 2026-09-22: essay board on the COMPOSED corpus (the
# reach-fixed one — factor under test), then the ANS board re-run with the
# fixed gate + legible eval rows. Both against the pod daemon; pipeline code
# is the LOCAL fixed CLI (the gate/verifier/eval changes ride in-process).
#   setsid/nohup this; log = target/ralph/pod-session-ei7.log
set -uo pipefail
cd /Users/alexsbryan/dev/commonwealth-ai
eval "$(scripts/dev-pod.sh env)"
say() { printf '\n== %s  [%s]\n' "$1" "$(date -u +%T)"; }

S=pilot-and-his-wife-composed
H=research/ontology-retrieval/raptor-proof
ERUNS=$H/essay/runs-$S; EBANK=$H/essay/bank-$S.toml; EOUT=$H/essay/board-$S
EINDEX="$HOME/.svrnmesh/indexes/raptor-$S"
ERECIPE=$H/ontology/literary-composed.recipe.toml

arm() { if [ -f "$ERUNS/$1/run-$2/manifest.json" ] && [ -f "$ERUNS/$1/run-$2/eval.json" ]; then echo "on disk: $1 run $2"; return 0; fi
  rm -rf "$ERUNS/$1/run-$2"
  python3 research/ontology-retrieval/harness/run_arm.py --arm "$1" --bank $EBANK --corpus raptor-$S \
    --index-dir "$EINDEX" --recipe "$ERECIPE" --out "$ERUNS" --run "$2" --pool-scale 2; }

say "wait for pod daemon"
until curl -s -o /dev/null --max-time 3 "$SOVEREIGN_DAEMON_URL/v1/models"; do sleep 20; done
curl -s "$SOVEREIGN_DAEMON_URL/v1/models" | python3 -c "import json,sys; print([m['id'] for m in json.load(sys.stdin).get('data',[])])"

say "essay arm closed-book run 1"; arm closed-book 1
for r in 1 2 3; do for a in bare full deep; do say "essay arm $a run $r"; arm $a $r; done; done

say "essay judge"
python3 $H/essay/essay_judge.py --runs $ERUNS --bank $EBANK --daemon "$SOVEREIGN_DAEMON_URL" \
  --model commonwealth/primary --out $EOUT --seed 20260922 --workers 2
echo "ESSAY JUDGE EXIT $?"

say "ANS board re-run (old runs moved aside — they are the void board's artifacts, kept in git)"
ANS=research/ontology-retrieval/ontology-proof/ans
if [ -d "$ANS/runs" ] && [ ! -d "$ANS/runs-void-20260921" ]; then mv "$ANS/runs" "$ANS/runs-void-20260921"; fi
RESUME=1 $ANS/run-proof.sh
echo "ANS EXIT $?"

say "teardown"
scripts/dev-pod.sh down 2>&1 | tail -2; scripts/dev-pod.sh status 2>&1 | tail -1
echo "SESSION DONE  [$(date -u +%T)]"
