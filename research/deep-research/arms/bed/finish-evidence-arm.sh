#!/usr/bin/env bash
set -u
cd /home/alexbryan/dev/commonwealth-ai
WAIT_PID=${1:?usage: finish-evidence-arm.sh <pid-of-running-cell>}
B=research/deep-research/arms/bed
L=research/deep-research/arms/runs-aiq-bar
echo "waiting on cell pid $WAIT_PID …"
while kill -0 "$WAIT_PID" 2>/dev/null; do sleep 30; done
echo "cell $WAIT_PID done at $(date -Is)"
echo "=== pinned control (n=1) ==="
$B/run-ceiling.sh pinned-control --reps 1 --task 69 \
  --env SOVEREIGN_DR_PIN_SAMPLING=1 2>&1 | tee "$L/arm-control.log" \
  | grep -E "flew|scored|REACH|WITNESS|REFUS|RESULT"
echo "=== wide 28/5 replicate (n=2 total; r1 already flown, skipped) ==="
$B/run-ceiling.sh wide-28-5 --reps 2 --task 69 \
  --env SOVEREIGN_DR_PIN_SAMPLING=1 --env SOVEREIGN_DR_REPORT_SECTION_EVIDENCE=1 \
  2>&1 | tee "$L/arm-wide-r2.log" | grep -E "flew|scored|REACH|WITNESS|REFUS|RESULT"
echo "=== ARM COMPLETE $(date -Is) ==="
$B/fly-evidence-arm.sh --status
