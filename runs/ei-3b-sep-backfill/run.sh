#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# RUN — order `ei-3b-sep-backfill`: the SEP seed tables, as one resumable
# battery. See manifest.md beside this file.
#
# This script is SCAFFOLDING, not an engine. The work is
# `sovereign/bench/sep_atlas/backfill_index.sh`, which already owns the
# worklist, the two verbs, the JSONL ledger and resume (ARCH §19). What this
# adds is the four things the run channel needs and the driver has no opinion
# about:
#
#   1. per-batch exit markers      markers/batch-<n>.json
#   2. resume from the ledger      (the driver's, unchanged — we just don't fight it)
#   3. a terminal DONE marker      DONE, written by an EXIT trap, always
#   4. a `free -g` sample per batch  sampler.log
#
# ...plus the order's own stop condition, which is the reason this wrapper
# exists at all rather than the driver being invoked bare:
#
#   "Not worth continuing if: the daemon is OOM-killed once during the run —
#    stop, cite the journal line, report the ledger position; do not restart
#    and continue blind."
#
# So the daemon's pid is captured before the first batch and checked after
# every one. If it is gone, the driver is stopped THERE, the kernel's own OOM
# line is copied into DONE, and the ledger position is reported. A battery
# that keeps going after its daemon died produces a plausible, exit-0, wrong
# ledger, which is the failure this workspace is built to refuse (ARCH §18.3).
#
# Usage:
#   runs/ei-3b-sep-backfill/run.sh              # the full 1,770-atlas sweep
#   runs/ei-3b-sep-backfill/run.sh --limit 5    # smoke; any driver flag passes through
#
# It does NOT start, stop, restart or reconfigure the daemon, and it does not
# swap a model. Those are the seat's (ARCH: box rules). It refuses to start if
# the box is not quiet enough, rather than discovering that at batch 30.
set -uo pipefail

HERE=$(cd -- "$(dirname -- "$0")" && pwd)
REPO=$(cd -- "$HERE/../.." && pwd)
DRIVER="$REPO/sovereign/bench/sep_atlas/backfill_index.sh"
LEDGER="$REPO/sovereign/bench/sep_atlas/backfill-index.jsonl"

MARKERS="$HERE/markers"
SAMPLER="$HERE/sampler.log"
RUN_LOG="$HERE/run.log"
DONE="$HERE/DONE"

# ── preconditions (each one is a refusal, never a warning) ──────────────────
FREE_FLOOR_GB=${FREE_FLOOR_GB:-40}          # box rule: >= 40 GB available
DAEMON=${DAEMON:-http://localhost:9741}

free_gb() { free -g | awk '/^Mem:/{print $7}'; }

[[ -x "$DRIVER" ]] || { echo "no driver at $DRIVER" >&2; exit 2; }

avail=$(free_gb)
if (( avail < FREE_FLOOR_GB )); then
  echo "REFUSING: ${avail} GB available, floor is ${FREE_FLOOR_GB} GB." >&2
  echo "  This battery is the one the OOM stop conditions were written for." >&2
  echo "  The seat announces the quiet window; do not lower the floor to fit." >&2
  exit 3
fi

if ! curl -fsS -m 5 "$DAEMON/v1/models" >/dev/null 2>&1; then
  echo "REFUSING: no daemon answering at $DAEMON/v1/models." >&2
  echo "  Starting one is the seat's call, not this script's." >&2
  exit 3
fi

DPID=$(pgrep -f 'sovereign-cli-daemon' | head -1)
[[ -n "$DPID" ]] || { echo "REFUSING: daemon answers HTTP but no pid found to watch." >&2; exit 3; }

# The claim is the order's ("claim take daemon:<node>:backfill"). `claim may-i`
# is banked-broken on this host, so a failure here is NAMED and does not stop
# the run — the seat's announced window is the real interlock (ARCH §18.3: the
# substitution is reported, not silent).
CLAIM_STATE=none
if command -v sovereign >/dev/null 2>&1; then
  if sovereign claim take "daemon:$(hostname -s):backfill" >/dev/null 2>&1; then
    CLAIM_STATE=taken
  else
    CLAIM_STATE="FAILED (banked: claim surface broken 2026-09-04; seat's window is the interlock)"
  fi
fi

# A stale marker from an earlier invocation would be read as this run's, so the
# directory is emptied rather than added to. The LEDGER is the durable record
# and is never touched here — it is what resume reads.
rm -rf "$MARKERS"; mkdir -p "$MARKERS"
: > "$SAMPLER"
: > "$RUN_LOG"
rm -f "$DONE"

HEAD_SHA=$(git -C "$REPO" rev-parse --short HEAD)
started=$(date -Is)
t_start=$(date +%s)

# `ls | wc -l` is the order's own done-when instrument; record it BEFORE so the
# after-number has something to be a delta from (ARCH §18.4).
SVRN=${SVRNMESH_ROOT:-$HOME/.svrnmesh}
ann_before=$(ls -d "$SVRN"/indexes/sep-*/atlas/atoms_ann.lance 2>/dev/null | wc -l)
v2_before=$(ls -d "$SVRN"/indexes/sep-*/atlas/atoms.lance 2>/dev/null | wc -l)
atlases=$(ls -d "$SVRN"/indexes/sep-*/atlas 2>/dev/null | wc -l)

ledger_lines() { [[ -f "$LEDGER" ]] && wc -l < "$LEDGER" | tr -d ' ' || echo 0; }

# ── the terminal marker, written on EVERY exit path ─────────────────────────
STOP_REASON="completed"
DRV_RC=""
finish() {
  local rc=${DRV_RC:-$?}
  local ann_after v2_after
  ann_after=$(ls -d "$SVRN"/indexes/sep-*/atlas/atoms_ann.lance 2>/dev/null | wc -l)
  v2_after=$(ls -d "$SVRN"/indexes/sep-*/atlas/atoms.lance 2>/dev/null | wc -l)
  {
    echo "run:            ei-3b-sep-backfill"
    echo "state:          $STOP_REASON"
    echo "driver_exit:    $rc"
    echo "head:           $HEAD_SHA"
    echo "started:        $started"
    echo "finished:       $(date -Is)"
    echo "wall_seconds:   $(( $(date +%s) - t_start ))"
    echo "claim:          $CLAIM_STATE"
    echo "sep_atlases:    $atlases"
    echo "atoms_ann:      $ann_before -> $ann_after"
    echo "atoms_lance:    $v2_before -> $v2_after"
    echo "ledger_lines:   $(ledger_lines)"
    echo "ledger:         $LEDGER"
    echo "run_log:        $RUN_LOG"
    echo "sampler:        $SAMPLER"
    echo "markers:        $MARKERS"
    echo "free_gb_final:  $(free_gb)"
    if [[ "$STOP_REASON" == oom-stop ]]; then
      echo
      echo "--- kernel OOM line (the order's stop condition) ---"
      journalctl --since "$started" -k 2>/dev/null \
        | grep -iE 'out of memory|oom-kill' | tail -4
      echo
      echo "DO NOT restart and continue blind. Report the ledger position above."
    fi
    echo
    echo "--- closing table (driver) ---"
    sed -n '/^state /,$p' "$RUN_LOG" | tail -40
  } > "$DONE"
  echo "wrote $DONE (state=$STOP_REASON driver_exit=$rc)"
}
trap finish EXIT

{
  echo "=== ei-3b-sep-backfill ==="
  echo "started:  $started"
  echo "head:     $HEAD_SHA"
  echo "daemon:   $DAEMON (pid $DPID)"
  echo "claim:    $CLAIM_STATE"
  echo "free:     ${avail} GB available (floor ${FREE_FLOOR_GB})"
  echo "atlases:  $atlases sep-*  |  atoms_ann $ann_before  |  atoms.lance $v2_before"
  echo
} | tee -a "$RUN_LOG"

# ── the driver, watched batch by batch ──────────────────────────────────────
"$DRIVER" "$@" >>"$RUN_LOG" 2>&1 &
DRV=$!
echo "driver pid $DRV — batches will be marked in $MARKERS"

batch=0
# `tail --pid` ends when the driver does; the loop sees each line as it lands.
tail -n +1 -F --pid="$DRV" "$RUN_LOG" 2>/dev/null | while IFS= read -r line; do
  case "$line" in
    progress:*)
      batch=$(( batch + 1 ))
      avail_now=$(free_gb)
      printf '%s batch=%03d %s free_avail_gb=%s ledger_lines=%s\n' \
        "$(date -Is)" "$batch" "$line" "$avail_now" "$(ledger_lines)" >> "$SAMPLER"
      # Per-batch exit marker. `progress:` is only ever printed AFTER the batch's
      # per-corpus ledger lines are written, so a marker means the ledger is
      # already durable for that batch — which is what makes resume exact.
      printf '{"batch":%d,"at":"%s","progress":"%s","free_avail_gb":%s,"ledger_lines":%s,"daemon_alive":%s}\n' \
        "$batch" "$(date -Is)" "${line#progress: }" "$avail_now" "$(ledger_lines)" \
        "$(kill -0 "$DPID" 2>/dev/null && echo true || echo false)" \
        > "$MARKERS/batch-$(printf '%03d' "$batch").json"
      # THE STOP CONDITION.
      if ! kill -0 "$DPID" 2>/dev/null; then
        echo "DAEMON GONE (pid $DPID) after batch $batch — stopping the battery" | tee -a "$RUN_LOG"
        echo "oom-stop" > "$HERE/.stop"
        kill "$DRV" 2>/dev/null
        break
      fi
      ;;
  esac
done

wait "$DRV"; DRV_RC=$?
[[ -f "$HERE/.stop" ]] && { STOP_REASON=$(cat "$HERE/.stop"); rm -f "$HERE/.stop"; }
exit "$DRV_RC"
