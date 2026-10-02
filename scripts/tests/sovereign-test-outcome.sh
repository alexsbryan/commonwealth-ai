#!/usr/bin/env bash
# `nextest_outcome` in scripts/sovereign-test.sh, driven with each case.
#
# Before 2026-09-24 a nextest run that died compiling left no report and no run
# ID, and the script reported it as exit 5 — "a concurrent run overwrote the
# report" — so two sessions went hunting for a peer run that never existed.
# The negative control is case 4: a run that STARTED (it printed a run ID) and
# left no report of its own is still a mismatch, or the exit-5 guard is gone.
set -uo pipefail

ROOT="$(git rev-parse --show-toplevel)"
SCRIPT="$ROOT/scripts/sovereign-test.sh"
T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT

sed -n '/^nextest_outcome() {$/,/^}$/p' "$SCRIPT" > "$T/pred.sh"
if [[ ! -s "$T/pred.sh" ]]; then
    echo "  FAIL  nextest_outcome() is not in $SCRIPT any more — this suite is testing nothing"
    exit 1
fi
# shellcheck disable=SC1090
. "$T/pred.sh"

rc=0
check() {  # check <name> <expect> <junit_path> <our_run_id> <nextest_rc>
    local name="$1" expect="$2"
    junit_path="$3" our_run_id="$4" nextest_rc="$5"
    local got
    got="$(nextest_outcome)"
    if [[ "$got" == "$expect" ]]; then
        echo "  ok    $name ($got)"
    else
        echo "  FAIL  $name — wanted $expect, got $got"
        rc=1
    fi
}

echo "sovereign-test-outcome:"
check "a report this run wrote is handed to the adapter" report "/t/junit.xml" "abc" 100
check "nextest's own exit 4 is the empty run" empty "" "abc" 4
check "no run ID and a non-zero exit is a build failure" build-failed "" "" 101
check "a run that started and left no report is a mismatch" mismatch "" "abc" 100
check "no run ID and exit 0 is still a mismatch, never green" mismatch "" "" 0
exit "$rc"
