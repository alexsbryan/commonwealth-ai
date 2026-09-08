#!/usr/bin/env bash
# ei3-acceptance-recall.sh — the EI3-recipe-to-answer instrument (ei-3c, 2026-09-07).
#
# The bar: "corpus-mcp/acceptance.sh runs recipe new -> ingest -> serve -> ask on
# the wessex fixture against a bare chat+embed endpoint; truth.json recall >= the
# daemon-built corpus". Two halves, and this measures both:
#
#   1. THE CHAIN, live. `acceptance.sh` without ACCEPT_INGEST — `recipe new`, the
#      discovery ladder, a bare `llama-server` embed endpoint, corpus_search /
#      atoms_lookup / corpus_ontology / ask over stdio, and the dep-tree
#      assertion. If that fails the chain is broken and the bar is not met:
#      value 0.0, with the failing acceptance line on stderr.
#   2. THE RECALL, from the artefact the ingest leg leaves behind.
#      `scripts/truth-recall.py` — the SAME scorer and the SAME invocation
#      acceptance.sh's own bar uses, minus `--gate`, because a non-zero exit is
#      `could-not-judge` to the campaign loader and a miss must arrive as a value
#      (co-lineage.py::measure_bar). value = recall(candidate) / recall(control).
#
# Why the ingest is not re-run here. It is 60-90 minutes against a live chat
# model (measured 2026-09-05: 5,418 s for one full manifest), which no measure
# window holds and no `timeout_s` should try to. So the ingest is the LANE's
# work, run through `ACCEPT_INGEST=1 corpus-mcp/acceptance.sh`, and this reads
# the atlas it left — the same shape as the EI2 instrument, which reads the
# newest verdicts artefact rather than re-judging a bank. The artefact's age is
# printed and rides in the row, so a stale read is visible rather than silent.
#
# Verdicts (ARCH §18.2). acceptance.sh distinguishes the two cases in its exit
# code — 2 is REFUSED, a dependency this machine does not have; 1 is FAIL, an
# assertion it lost — so a box without llama-server reads could-not-judge and
# never as a failed bar. Exit 3 here: acceptance refused, or the control /
# candidate atlas the ratio is between is absent, naming which. Value 0.0: a
# clean environment and a failing chain. Only a finished chain plus two scored
# atlases yields a ratio.
set -uo pipefail
REPO="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
DATA_ROOT="${SOVEREIGN_DATA_DIR:-$HOME/.svrnmesh}"
TRUTH="$REPO/sovereign-recipes/wessex-hoard/truth.json"
CONTROL="$DATA_ROOT/indexes/wessex-hoard/atlas/atoms.json"
# The corpus id acceptance.sh's ingest leg writes into (its own default), and
# every dated variant a lane run may have left. Newest wins; the age is printed.
INGEST_CORPUS="${INGEST_CORPUS:-wessex-hoard-bare}"

cnj() { echo "ei3: $* — could-not-judge" >&2; exit 3; }

# The environment half of the preflight is acceptance.sh's own list, asked for
# by name rather than kept twice (ARCH §10.6). It exits 2 for a dependency this
# machine lacks — could-not-judge — and that is also how a FULL run's exit 2 is
# read below. It resolves the embed gguf against the main checkout when this
# runs from a worktree, which every campaign worker does.
pf="$("$REPO/corpus-mcp/acceptance.sh" --preflight 2>&1)"
pf_rc=$?
(( pf_rc == 0 )) || cnj "acceptance.sh refused its own preflight: $pf"
echo "ei3: $pf" >&2

# What only THIS instrument needs: the two atlases the ratio is between.
[[ -f "$TRUTH" ]] || cnj "no truth.json at $TRUTH"
[[ -f "$CONTROL" ]] || cnj "no daemon-built control atlas at $CONTROL — the bar is relative to it"

candidate=""
for d in "$DATA_ROOT/indexes/$INGEST_CORPUS"*/atlas/atoms.json; do
  [[ -f "$d" ]] || continue
  if [[ -z "$candidate" || "$d" -nt "$candidate" ]]; then candidate="$d"; fi
done
[[ -n "$candidate" ]] || cnj "no bare-endpoint atlas under $DATA_ROOT/indexes/$INGEST_CORPUS* — \
the ingest leg has NEVER RUN here; run \`ACCEPT_INGEST=1 CHAT_GGUF=<gguf> corpus-mcp/acceptance.sh\` in a lane window"
age_h=$(( ( $(date +%s) - $(stat -c %Y "$candidate") ) / 3600 ))
echo "ei3: candidate atlas $candidate (${age_h}h old); control $CONTROL" >&2

log="$(mktemp)"
trap 'rm -f "$log"' EXIT
echo "ei3: running the llama-server arm of corpus-mcp/acceptance.sh (no ACCEPT_INGEST)" >&2
"$REPO/corpus-mcp/acceptance.sh" >"$log" 2>&1
chain_rc=$?
tail -5 "$log" >&2
if (( chain_rc == 2 )); then
  # A dependency went missing between the preflight and the run (a model file
  # pulled out from under it, llama-server removed). Environment, not a miss.
  cnj "acceptance.sh REFUSED mid-run: $(grep -m1 'acceptance: REFUSED' "$log")"
fi
if (( chain_rc != 0 )); then
  echo "ei3: the recipe -> serve -> ask chain FAILED (exit $chain_rc) — value 0.0, not could-not-judge:" >&2
  grep -m1 'acceptance: FAIL' "$log" >&2 || true
  python3 -c "import json;print(json.dumps({'value':0.0,'artifact':'$candidate','commit':'$(git -C "$REPO" rev-parse HEAD 2>/dev/null)','chain':'failed'}))"
  exit 0
fi
echo "ei3: chain ok" >&2

# The same scorer acceptance.sh's own bar runs, without --gate: the miss must be
# a VALUE, not an exit code.
python3 "$REPO/scripts/truth-recall.py" --truth "$TRUTH" \
  --control "$CONTROL" --candidate "$candidate" \
  | python3 -c '
import json, subprocess, sys
d = json.loads(sys.stdin.read().splitlines()[-1])
d["commit"] = subprocess.run(["git", "-C", sys.argv[1], "rev-parse", "HEAD"],
                             capture_output=True, text=True).stdout.strip() or None
d["chain"] = "ok"
print(json.dumps(d))' "$REPO"
