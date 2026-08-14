#!/usr/bin/env bash
# Paired A/B chaos arms for the Phase-4 tombstone (order gate-tombstone-ladder).
# Pre-registration: results/PREREG_gate_tombstone_ladder_20260814.md (679bf6d3).
#
# THE ARMS ARE A FLAG FLIP, NOT A REBUILD. One binary serves both arms:
#   OLD = SOVEREIGN_GATE_LONGFORM_REPAIR=1   (repair ladder armed — pre-tombstone)
#   NEW = unset                              (tombstoned — the shipped default)
# So the arms differ only in the thing under test by construction rather than
# by care, and no rebuild can sneak an unrelated change between them.
#
# SERIAL, NEVER PARALLEL — same reasoning as run_longform_harvest.sh: the banks
# share one daemon, one model residency and one GPU. Concurrency would contend
# for slots and make every per-turn timing meaningless, and would also make the
# two arms non-comparable, which is the entire point of the run.
#
# ARM ORDER IS INTERLEAVED BY SEED (old,new / old,new / old,new) rather than
# blocked (all old, then all new). If the machine drifts over six hours —
# thermal, memory pressure, another process — a blocked design confounds that
# drift perfectly with the arm. Interleaving spreads it across both.
set -uo pipefail

cd "$(dirname "$0")/../../.."   # repo root
BENCH=sovereign/bench/chaos_monkey
OUT=$BENCH/results
STAMP=${STAMP:-$(date +%Y%m%d)}
CLI=${CLI:-target/debug/sovereign-cli-llm}
RUNDIR=${RUNDIR:-runs/tombstone-chaos}
# ONE WRITER PER LOG, BY CONSTRUCTION. The log name is DERIVED from $RUNDIR
# rather than from $STAMP alone, because $RUNDIR is already the thing that
# distinguishes one invocation of this script from another: it holds this run's
# RUNNING/DONE/FAILED markers, and run_tombstone_compound.sh already overrides
# it. A name built from $STAMP alone was shared by both writers, and on
# 2026-08-13 the chained compound run truncated the primary's 12 verdict blocks
# (recovered from runs/tombstone-chaos/launchd.out and preserved as
# results/tombstone_chaos_primary_20260813.run.log — note 071cab0d).
#
# Deriving the name is what makes this unforgettable: a future third bank that
# copies the compound script gets a distinct log for free, because setting
# RUNDIR is not optional — the marker files need it.
RUN_ID=$(basename "$RUNDIR")
LOG=$OUT/${RUN_ID}_${STAMP}.run.log
SEEDS=${SEEDS:-3}
BANKS=${BANKS:-"saltgrass secret_agent"}

mkdir -p "$RUNDIR"
# Refuse rather than join a run already in flight. With the log 1:1 with
# $RUNDIR, a live RUNNING marker is exactly "another writer owns this log".
if [ -e "$RUNDIR/RUNNING" ]; then
  echo "REFUSING: $RUNDIR/RUNNING exists — another run owns $LOG." >&2
  echo "          Wait for it, or start under a different RUNDIR." >&2
  exit 5
fi
rm -f "$RUNDIR/DONE" "$RUNDIR/FAILED"
: >"$RUNDIR/RUNNING"

fail() { echo "REFUSING: $*" | tee -a "$LOG" >&2; rm -f "$RUNDIR/RUNNING"; : >"$RUNDIR/FAILED"; exit "${2:-2}"; }

# ── Guards, before six hours are committed ──────────────────────────────
[ -x "$CLI" ] || fail "no binary at $CLI — build it: cargo build -p sovereign-cli-llm" 2

# The tombstone must actually be IN this binary. Without this guard a stale
# binary would run both arms identically and report a clean pass that verified
# nothing — the exact failure this bench exists to catch, one level up.
if [ "$(strings "$CLI" | grep -c 'annotated_marked')" -eq 0 ]; then
  fail "$CLI predates the tombstone (no 'annotated_marked'). Rebuild: cargo build -p sovereign-cli-llm" 3
fi

# Daemon liveness. It has NO /healthz (404); /v1/models is the surface.
curl -sf --max-time 10 http://localhost:9741/v1/models >/dev/null \
  || fail "daemon not answering on :9741" 4

# APPEND, NEVER TRUNCATE. `: >"$LOG"` was the second half of the 2026-08-13
# clobber: even with distinct names, a re-run of the same arm on the same day
# would erase the first run's verdict blocks. The START banner below delimits
# runs, so appending costs nothing and loses nothing.
mkdir -p "$OUT"
{
  echo "=== TOMBSTONE CHAOS ARMS START $(date -Iseconds) ==="
  echo "=== run $RUN_ID · log $LOG ==="
  echo "=== binary $CLI ($(date -r "$CLI" -Iseconds)) ==="
  echo "=== commit $(git rev-parse --short HEAD) ==="
  echo "=== banks: $BANKS · seeds: $SEEDS · arms: old(repair=1) new(default) ==="
} | tee -a "$LOG"

run_one() {  # arm bank seed
  local arm=$1 bank=$2 seed=$3
  local tag="${bank}_${arm}_r${seed}"
  local out="$OUT/tomb_${tag}_${STAMP}.jsonl"
  echo "=== $tag START $(date -Iseconds) ===" | tee -a "$LOG"
  if [ "$arm" = "old" ]; then export SOVEREIGN_GATE_LONGFORM_REPAIR=1
  else unset SOVEREIGN_GATE_LONGFORM_REPAIR; fi
  # Echo the arm's actual env so the log proves which arm ran, rather than
  # asserting it from this script's control flow (#4: cite, don't recall).
  echo "    SOVEREIGN_GATE_LONGFORM_REPAIR=${SOVEREIGN_GATE_LONGFORM_REPAIR:-<unset>}" | tee -a "$LOG"
  "$CLI" bench chaos-monkey run \
    --bank "$BENCH/${bank}.toml" \
    --manifest "$BENCH/manifest.toml" \
    --out "$out" \
    --transcripts "$OUT/tomb_${tag}_${STAMP}.transcripts.jsonl" 2>&1 | tee -a "$LOG"
  local rc=${PIPESTATUS[0]}
  # A non-zero exit is a BENCH GATE VERDICT, not a harness failure. The run's
  # success is whether rows were written; the verdict is read from the log by
  # the scorer, never inferred from $?.
  #
  # Exit codes since 2026-08-14 (GateVerdict::exit_code): 0 = every judgeable
  # gate passed · 1 = a gate FAILED · 4 = nothing was judgeable. saltgrass_
  # compound used to exit 1 on every harvest because it has zero absent probes
  # and RED-LINE 2's NaN was rendered as FAIL; that is now COULD-NOT-JUDGE and
  # excluded from the conjunction, so a compound arm's exit code carries signal
  # again instead of being a standing false red.
  echo "=== $tag END $(date -Iseconds) exit=$rc rows=$(wc -l <"$out" 2>/dev/null || echo 0) ===" | tee -a "$LOG"
}

for seed in $(seq 1 "$SEEDS"); do
  for bank in $BANKS; do
    run_one old "$bank" "$seed"
    run_one new "$bank" "$seed"
  done
done

echo "=== TOMBSTONE CHAOS ARMS COMPLETE $(date -Iseconds) ===" | tee -a "$LOG"
rm -f "$RUNDIR/RUNNING"
: >"$RUNDIR/DONE"
