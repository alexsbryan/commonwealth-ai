#!/usr/bin/env bash
# Third-bank extension for the Phase-4 tombstone arms (seat-approved 2026-08-14).
# Adds saltgrass_compound — the higher longform-density bank the standing
# harvest pairs with saltgrass — to the paired A/B set already running.
#
# WHY A SECOND ONE-SHOT INSTEAD OF EDITING THE RUNNING SCRIPT. Bash reads a
# script file INCREMENTALLY as it executes; editing `run_tombstone_arms.sh`
# in place while it is mid-run can make the shell resume at a byte offset that
# no longer means what it did, and the corruption is silent. Invoking the same
# file again as a NEW process is safe (its own descriptor); editing it is not.
#
# WHY IT WAITS RATHER THAN RUNS NOW. The banks share one daemon, one model
# residency and one GPU. Running concurrently would contend for slots, make
# every per-turn timing meaningless, and — worse — make the compound arms
# non-comparable with the saltgrass/secret_agent arms they are meant to join.
# Serial is the whole reason those numbers can be pooled.
#
# STAMP is pinned rather than derived: this starts after midnight, and
# `date +%Y%m%d` would file the extension under a different day from the arms
# it belongs to.
set -uo pipefail
cd "$(dirname "$0")/../../.."   # repo root

MAIN_DONE=runs/tombstone-chaos/DONE
MAIN_FAILED=runs/tombstone-chaos/FAILED

echo "[compound] waiting for the primary arms to finish ($MAIN_DONE) …"
while [ ! -e "$MAIN_DONE" ]; do
  if [ -e "$MAIN_FAILED" ]; then
    echo "[compound] primary arms FAILED — refusing to start; the pooled" \
         "comparison would have no partner set." >&2
    exit 1
  fi
  sleep 120
done

echo "[compound] primary arms complete — starting third bank $(date -Iseconds)"
exec env \
  STAMP=20260813 \
  BANKS="saltgrass_compound" \
  RUNDIR=runs/tombstone-chaos-compound \
  ./sovereign/bench/chaos_monkey/run_tombstone_arms.sh
