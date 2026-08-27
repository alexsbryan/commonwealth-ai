#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# fly-outline-arm.sh — the structure lever, end to end.
#
# At 16x4 the per-criterion gaps put the ENTIRE remaining deficit in
# readability (-1.21; the other three dimensions are -0.17/+0.12/+0.20), and
# readability is the one dimension the evidence budget does not move. The
# judge's objection is structural: "a somewhat fragmented structure with many
# short sections that jump between topics ... rather than a single narrative
# arc". The bed pins 20 sections; the reference reads as nine.
#
# So: plan a SHORTER outline with the production planner, then compose the SAME
# window against it. One variable moves.
set -u
cd /home/alexbryan/dev/commonwealth-ai

OUTLINE=${OUTLINE:-research/deep-research/arms/bed-compose/outline-planned.json}
ARM=${ARM:-16:4}
OUT=${OUT:-/home/alexbryan/dev/commonwealth-ai/research/deep-research/arms/runs-compose/outline1}
PRE=""
[ -f /run/.containerenv ] || PRE="toolbox run -c sovereign-vulkan"

# The planner and the judge both run on `primary` (the 27B). Flying while a
# score is in the air just earns a 503 shed, so wait it out rather than retry
# into it — the same distinction score-arms.sh makes.
while pgrep -f 'score_one.py' >/dev/null 2>&1; do
  echo "    judge busy on primary — waiting"; sleep 30
done

if [ -s "$OUTLINE" ]; then
  echo "=== outline already planned: $OUTLINE ($(python3 -c "import json;print(len(json.load(open('$OUTLINE'))))") sections)"
else
  echo "=== PLANNING OUTLINE (production planner, architecture cap on) ==="
  # `env` runs INSIDE the container, after the prefix — toolbox does not
  # forward the caller's environment.
  $PRE env COMPOSE_SECTIONS_OUT="$OUTLINE" SOVEREIGN_DR_REPORT_ARCHITECTURE=1 \
    cargo test -p sovereign-core --test compose_replay \
    -- --ignored --nocapture plan_outline_dump 2>&1 | tail -25
  [ -s "$OUTLINE" ] || { echo "REFUSED: planner wrote no outline"; exit 3; }
fi

N=$(python3 -c "import json;print(len(json.load(open('$OUTLINE'))))")
echo "=== FLYING $ARM against a $N-section outline (bed pins 20) -> $OUT ==="
EXTRA_ENV="COMPOSE_SECTIONS=$OUTLINE SOVEREIGN_DR_REPORT_ARCHITECTURE=1" \
  ARMS="$ARM" OUT="$OUT" ./research/deep-research/arms/bed/sweep-compose.sh
