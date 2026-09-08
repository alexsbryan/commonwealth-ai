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
# Verdicts (ARCH §18.2): missing binary / model / llama-server / control atlas /
# candidate atlas -> exit 3, could-not-judge, naming which. A clean environment
# with a failing chain -> value 0.0. Only a finished chain plus two scored
# atlases yields a ratio.
set -uo pipefail
REPO="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
DATA_ROOT="${SOVEREIGN_DATA_DIR:-$HOME/.svrnmesh}"
TRUTH="$REPO/sovereign-recipes/wessex-hoard/truth.json"
CONTROL="$DATA_ROOT/indexes/wessex-hoard/atlas/atoms.json"
# The corpus id acceptance.sh's ingest leg writes into (its own default), and
# every dated variant a lane run may have left. Newest wins; the age is printed.
INGEST_CORPUS="${INGEST_CORPUS:-wessex-hoard-bare}"
CORPUS_MCP="${CORPUS_MCP:-$REPO/target/debug/corpus-mcp}"
EMBED_GGUF="${EMBED_GGUF:-$REPO/sovereign/models/Qwen3-Embedding-0.6B-Q8_0.gguf}"

cnj() { echo "ei3: $* — could-not-judge" >&2; exit 3; }

# Preflight BEFORE the expensive half, so an absent tool is could-not-judge and
# never reads as a broken chain (ARCH §18.4).
[[ -x "$CORPUS_MCP" ]] || cnj "no corpus-mcp at $CORPUS_MCP (cargo build -p corpus-mcp)"
[[ -f "$EMBED_GGUF" ]] || cnj "no embedding model at $EMBED_GGUF"
command -v llama-server >/dev/null || cnj "llama-server not on PATH (this arm IS llama-server)"
command -v jq >/dev/null || cnj "jq not on PATH"
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
CORPUS_MCP="$CORPUS_MCP" EMBED_GGUF="$EMBED_GGUF" \
  "$REPO/corpus-mcp/acceptance.sh" >"$log" 2>&1
chain_rc=$?
tail -5 "$log" >&2
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
