#!/usr/bin/env bash
# ei-7c: gates at the rebased tip. Run INSIDE the toolbox, under a MemoryMax scope + flock.
set -u
cd /home/alexbryan/dev/ei7c-wt || exit 99
OUT=/home/alexbryan/dev/ei7c-wt/runs/gates-resume
mkdir -p "$OUT/markers"
echo "tip=$(git rev-parse HEAD)" > "$OUT/provenance.txt"
date >> "$OUT/provenance.txt"

./scripts/sovereign-lint.sh --human --full > "$OUT/lint.log" 2>&1
echo $? > "$OUT/markers/lint.rc"

./scripts/sovereign-test.sh --human > "$OUT/test.log" 2>&1
echo $? > "$OUT/markers/test.rc"

./scripts/pre-push.sh > "$OUT/prepush.log" 2>&1
echo $? > "$OUT/markers/prepush.rc"

date >> "$OUT/provenance.txt"
echo DONE > "$OUT/markers/DONE"
