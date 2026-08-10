#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# The tuning loop's component objectives — one timed command per component.
# Order native-grounding-tuning-loop (directive 44f48dd6). Method: METHOD.md
# beside this file. Work queue: NATIVE_GROUNDING_PARITY_PLAN.md §3 ledger.
#
#   objective.sh admission   — D3 apparatus replay: the case ledger regenerates
#                              byte-identically from committed artifacts (A1's
#                              instrument stays valid while other parts tune).
#   objective.sh routing     — offline router replay of the 3 A3 probes.
#   objective.sh retrieval   — pool recomposition + keyword-ratio probe, 11 A4 cases.
#   objective.sh claims      — honesty-classifier replay, the A5 caveat case.
#
# Verdicts (four, ARCH §18.1): PASS(0) / FAIL(1) / COULD-NOT-JUDGE(2).
# never-ran is visible as the component's absence from JOURNAL.md.
set -u
REPO="$(cd "$(dirname "$0")/../../../.." && pwd)"
STEP3="$REPO/sovereign/bench/calibration/step3"
LOOP="$REPO/sovereign/bench/calibration/loop"
COMP="${1:-}"
[ -z "$COMP" ] && { echo "usage: objective.sh <admission|routing|retrieval|claims>"; exit 2; }
T0=$(date +%s)

verdict() { # $1=PASS|FAIL|COULD-NOT-JUDGE $2=metric text
  local t=$(( $(date +%s) - T0 ))
  echo "OBJECTIVE $COMP $1 ${2:-} (${t}s)"
  case "$1" in PASS) exit 0;; FAIL) exit 1;; *) exit 2;; esac
}

case "$COMP" in
admission)
  # Replay the corpus builder + attribution from committed artifacts only.
  cd "$STEP3" || verdict COULD-NOT-JUDGE "step3 dir missing"
  out=$(python3 build_failure_corpus.py 2>&1) || verdict COULD-NOT-JUDGE "builder crashed: $out"
  echo "$out" | grep -q "wrote 31 cases" || verdict FAIL "case count drifted: $(echo "$out" | head -1)"
  out2=$(python3 attribute_failures.py 2>&1) || verdict COULD-NOT-JUDGE "attribution crashed: $out2"
  echo "$out2" | grep -q "^31 cases" || verdict FAIL "attribution count drifted"
  if ! git -C "$REPO" diff --quiet -- \
      sovereign/bench/calibration/step3/failure_corpus.jsonl \
      sovereign/bench/calibration/step3/attribution.json; then
    verdict FAIL "regeneration not byte-identical to committed ledger"
  fi
  verdict PASS "31/31 cases regenerate byte-identical (15 adm / 1 abst / 4 rout / 11 retr)"
  ;;
routing)
  python3 "$LOOP/routing_objective.py"; rc=$?
  t=$(( $(date +%s) - T0 ))
  case $rc in 0) echo "OBJECTIVE routing PASS (${t}s)";; 1) echo "OBJECTIVE routing FAIL (${t}s)";;
    *) echo "OBJECTIVE routing COULD-NOT-JUDGE (${t}s)";; esac
  exit $rc
  ;;
retrieval)
  python3 "$LOOP/retrieval_objective.py" "${2:-}"; rc=$?
  t=$(( $(date +%s) - T0 ))
  case $rc in 0) echo "OBJECTIVE retrieval PASS (${t}s)";; 1) echo "OBJECTIVE retrieval FAIL (${t}s)";;
    *) echo "OBJECTIVE retrieval COULD-NOT-JUDGE (${t}s)";; esac
  exit $rc
  ;;
claims)
  python3 "$LOOP/claims_objective.py"; rc=$?
  t=$(( $(date +%s) - T0 ))
  case $rc in 0) echo "OBJECTIVE claims PASS (${t}s)";; 1) echo "OBJECTIVE claims FAIL (${t}s)";;
    *) echo "OBJECTIVE claims COULD-NOT-JUDGE (${t}s)";; esac
  exit $rc
  ;;
*)
  echo "OBJECTIVE $COMP COULD-NOT-JUDGE unknown component"; exit 2
  ;;
esac
