#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# One measure-iterate cycle of the chaos QA loop.
#
#   run-qa-iteration.sh <label> <minutes> [extra chaos.mjs args...]
#
# Runs the chaos QA agent (attach mode — your resident daemon + corpora) for
# <minutes>, archives the field journal under test-artifacts/qa-iterations/
# <label>.jsonl (so iterations are comparable side-by-side), and prints the
# disentangled scorecard. Pass per-iteration runtime knobs via the environment
# (they propagate to the spawned desktop), e.g.
#
#   SOVEREIGN_KQ_RETRY_FLOOR=0.35 run-qa-iteration.sh iter-2 60
#
# The agent, the question stream, and the bench oracle are FROZEN across
# iterations — only the app (and the knobs it reads) change. That is what keeps
# a rising score honest rather than coached.
set -uo pipefail
cd "$(dirname "$0")/../../.." || exit 2   # crate root (sovereign-desktop)

LABEL="${1:?usage: run-qa-iteration.sh <label> <minutes> [chaos args...]}"
MINUTES="${2:?usage: run-qa-iteration.sh <label> <minutes> [chaos args...]}"
shift 2 || true

SCRIPTS="tests/e2e/scripts"
ART="test-artifacts"
ITERDIR="$ART/qa-iterations"
mkdir -p "$ITERDIR"

echo "── QA iteration '$LABEL' — ${MINUTES}min — knobs: ${SOVEREIGN_KQ_RETRY_FLOOR:+RETRY_FLOOR=$SOVEREIGN_KQ_RETRY_FLOOR }${SOVEREIGN_KQ_FANOUT_CONCURRENCY:+FANOUT=$SOVEREIGN_KQ_FANOUT_CONCURRENCY }${SOVEREIGN_KQ_PER_CORPUS_CAP:+CAP=$SOVEREIGN_KQ_PER_CORPUS_CAP }"

# Run the wander. Default RUST_LOG (debug) is left intact so the gate verdicts +
# top_similarity + latency components land in chaos-app.log for analysis.
node "$SCRIPTS/chaos.mjs" --attach --spawn --minutes "$MINUTES" "$@"
RC=$?

# Archive this iteration's journal + app log (chaos truncates them next run).
cp -f "$ART/chaos-journal.jsonl" "$ITERDIR/$LABEL.jsonl" 2>/dev/null
cp -f "$ART/chaos-app.log" "$ITERDIR/$LABEL.app.log" 2>/dev/null

echo ""
node "$SCRIPTS/chaos-scorecard.mjs" "$ITERDIR/$LABEL.jsonl" --label "$LABEL"
echo "iteration '$LABEL' wander exit=$RC  journal=$ITERDIR/$LABEL.jsonl"
