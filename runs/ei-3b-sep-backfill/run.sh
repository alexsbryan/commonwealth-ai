#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# RUN — order `ei-3b-sep-backfill`: the SEP seed tables, as one resumable
# battery, in TWO LEGS. See manifest.md beside this file.
#
# This script is SCAFFOLDING, not an engine. The work is
# `sovereign/bench/sep_atlas/backfill_index.sh`, which already owns the
# worklist, the two verbs, the JSONL ledger and resume (ARCH §19). What this
# adds is the five things the run channel needs and the driver has no opinion
# about:
#
#   1. a BINARY-FRESHNESS refusal   (see "the open item", below)
#   2. a BUSY-BOX refusal            builds != 0, or MemAvailable < floor
#   3. per-leg + per-batch markers   markers/leg<N>.rc, markers/leg<N>-batch-*.json
#   4. a terminal DONE marker        DONE, written by an EXIT trap, always
#   5. a `free -g` sample per batch  sampler.log
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
# THE OPEN ITEM THIS REVISION CLOSES. The previous staging pointed at a binary
# built 2026-09-04 15:11. That predates `c0f632403` (ei-3c: the seed table's
# population became map-derived — the whole point of the table), `57e5ee76d`
# (ei-7c: the installed atlas serves from the v2 store) and `78308e392`
# (ei-7a: `AtomType::Summary`). A 2.5 h battery against a binary that cannot
# see the change it exists to propagate is the exit-0-and-wrong failure in its
# purest form. So the check is STRUCTURAL, not remembered (ARCH §10, §7): the
# run refuses to start if `$SCLI` is older than the commit it is run at.
#
# TWO LEGS, because a 2.5 h battery whose first invocation is also its first
# test is a bad bet. Leg 1 is a `--limit 5` smoke that costs about a minute
# and proves the whole chain — dispatcher, exec into the sibling, daemon
# probe, embed, ledger write, marker, resume. Leg 2 is the full sweep, and the
# driver's own resume means it simply skips leg 1's five.
#
# Usage:
#   runs/ei-3b-sep-backfill/run.sh          # leg 1 (smoke) then leg 2 (sweep)
#   SMOKE_ONLY=1 runs/.../run.sh            # leg 1 only
#   ALLOW_BUSY_BOX=1 runs/.../run.sh        # override the busy-box refusal
#
# It does NOT start, stop, restart or reconfigure the daemon, and it does not
# swap a model. Those are the seat's (ARCH: box rules).
set -uo pipefail

HERE=$(cd -- "$(dirname -- "$0")" && pwd)
REPO=$(cd -- "$HERE/../.." && pwd)
DRIVER="$REPO/sovereign/bench/sep_atlas/backfill_index.sh"
LEDGER="$REPO/sovereign/bench/sep_atlas/backfill-index.jsonl"

# The driver defaults SCLI to its OWN repo's target/debug. Pinning it here as
# well is not redundancy: it makes the binary this run uses a fact recorded in
# DONE, and it is what the freshness refusal below has to check.
SCLI=${SCLI:-$REPO/target/debug/sovereign-cli}
export SCLI

MARKERS="$HERE/markers"
SAMPLER="$HERE/sampler.log"
RUN_LOG="$HERE/run.log"
DONE="$HERE/DONE"

FREE_FLOOR_GB=${FREE_FLOOR_GB:-60}          # order's floor for THIS battery
DAEMON=${DAEMON:-http://localhost:9741}

free_gb() { free -g | awk '/^Mem:/{print $7}'; }
ledger_lines() { [[ -f "$LEDGER" ]] && wc -l < "$LEDGER" | tr -d ' ' || echo 0; }

# A refusal must leave the same terminal marker a run does, or the seat cannot
# tell "refused" from "never started" (ARCH §18.1: never-ran is its own verdict).
refuse() { # reason... ; exit 1 after writing DONE
  local reason="$1"
  mkdir -p "$MARKERS"
  {
    echo "run:            ei-3b-sep-backfill"
    echo "state:          refused"
    echo "driver_exit:    1"
    echo "reason:         $reason"
    echo "at:             $(date -Is)"
    echo "scli:           $SCLI"
    echo "free_avail_gb:  $(free_gb)"
    echo "ledger_lines:   $(ledger_lines)"
  } > "$DONE"
  echo "REFUSING: $reason" >&2
  echo "wrote $DONE (state=refused)" >&2
  exit 1
}

# ── preconditions (each one is a refusal, never a warning) ──────────────────
[[ -x "$DRIVER" ]] || refuse "no driver at $DRIVER"
[[ -x "$SCLI" ]]   || refuse "no sovereign-cli at $SCLI — build it, or set SCLI"

# (1) BINARY FRESHNESS. The dispatcher execs `sovereign-cli-llm` from its own
# directory, so BOTH must postdate the commit. `%ct` is the commit date of the
# tree this run is launched at; a binary older than it cannot contain it.
HEAD_SHA=$(git -C "$REPO" rev-parse --short HEAD)
head_ct=$(git -C "$REPO" log -1 --format=%ct)
for b in "$SCLI" "$(dirname "$SCLI")/sovereign-cli-llm"; do
  [[ -x "$b" ]] || refuse "missing sibling binary $b (the dispatcher execs it for \`atlas\`)"
  b_mt=$(stat -c %Y "$b")
  if (( b_mt < head_ct )); then
    refuse "$(basename "$b") built $(date -d "@$b_mt" -Is), BEFORE HEAD $HEAD_SHA ($(date -d "@$head_ct" -Is)) — rebuild: cargo build -p sovereign-cli -p sovereign-cli-llm --features corpus-engine/treesitter,sovereign-cli/dev-tools,sovereign-cli/code-intel,sovereign-cli/awareness"
  fi
done

# (2) BUSY BOX. A battery shares the box with nothing. `pgrep -x` matches the
# EXACT binary name — a phrase match here would find this script's own wrapper
# shells and refuse against itself.
builds=$(( $(pgrep -x cargo 2>/dev/null | wc -l) + $(pgrep -x rustc 2>/dev/null | wc -l) ))
avail=$(free_gb)
if (( builds != 0 || avail < FREE_FLOOR_GB )); then
  if [[ -n "${ALLOW_BUSY_BOX:-}" ]]; then
    echo "WARNING: busy box overridden by ALLOW_BUSY_BOX=1 (builds=$builds avail=${avail}GB)" >&2
    BUSY_NOTE="OVERRIDDEN (builds=$builds avail=${avail}GB)"
  else
    refuse "busy box: builds=$builds (want 0), ${avail} GB available (floor ${FREE_FLOOR_GB} GB). This battery is the one the OOM stop conditions were written for. The seat announces the quiet window; do not lower the floor to fit. Override: ALLOW_BUSY_BOX=1"
  fi
fi
BUSY_NOTE=${BUSY_NOTE:-clean}

curl -fsS -m 5 "$DAEMON/v1/models" >/dev/null 2>&1 \
  || refuse "no daemon answering at $DAEMON/v1/models — starting one is the seat's call"

DPID=$(pgrep -f 'sovereign-cli-daemon' | head -1)
[[ -n "$DPID" ]] || refuse "daemon answers HTTP but no pid found to watch"

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
: > "$SAMPLER"; : > "$RUN_LOG"; rm -f "$DONE" "$HERE/.stop"

started=$(date -Is)
t_start=$(date +%s)

SVRN=${SVRNMESH_ROOT:-$HOME/.svrnmesh}
ann_before=$(ls -d "$SVRN"/indexes/sep-*/atlas/atoms_ann.lance 2>/dev/null | wc -l)
v2_before=$(ls -d "$SVRN"/indexes/sep-*/atlas/atoms.lance 2>/dev/null | wc -l)
atlases=$(ls -d "$SVRN"/indexes/sep-*/atlas 2>/dev/null | wc -l)
ledger_before=$(ledger_lines)

# ── the terminal marker, written on EVERY exit path ─────────────────────────
STOP_REASON="completed"
FINAL_RC=""
LEG1_RC="-"; LEG2_RC="-"
finish() {
  local rc=${FINAL_RC:-$?}
  local ann_after v2_after
  ann_after=$(ls -d "$SVRN"/indexes/sep-*/atlas/atoms_ann.lance 2>/dev/null | wc -l)
  v2_after=$(ls -d "$SVRN"/indexes/sep-*/atlas/atoms.lance 2>/dev/null | wc -l)
  {
    echo "run:            ei-3b-sep-backfill"
    echo "state:          $STOP_REASON"
    echo "driver_exit:    $rc"
    echo "leg1_smoke_rc:  $LEG1_RC"
    echo "leg2_sweep_rc:  $LEG2_RC"
    echo "head:           $HEAD_SHA"
    echo "scli:           $SCLI ($(date -d "@$(stat -c %Y "$SCLI")" -Is))"
    echo "box_at_start:   $BUSY_NOTE"
    echo "started:        $started"
    echo "finished:       $(date -Is)"
    echo "wall_seconds:   $(( $(date +%s) - t_start ))"
    echo "claim:          $CLAIM_STATE"
    echo "sep_atlases:    $atlases"
    echo "atoms_ann:      $ann_before -> $ann_after"
    echo "atoms_lance:    $v2_before -> $v2_after"
    echo "ledger_lines:   $ledger_before -> $(ledger_lines)"
    echo "ledger:         $LEDGER"
    echo "run_log:        $RUN_LOG"
    echo "sampler:        $SAMPLER"
    echo "markers:        $MARKERS"
    echo "free_gb_final:  $(free_gb)"
    echo "daemon_alive:   $(kill -0 "$DPID" 2>/dev/null && echo true || echo false)"
    echo
    echo "--- kernel OOM lines since start (the order's stop condition) ---"
    journalctl --since "$started" -k 2>/dev/null \
      | grep -iE 'out of memory|oom-kill' | tail -6 || true
    echo "(no lines above = the daemon was not OOM-killed during the run)"
    if [[ "$STOP_REASON" == oom-stop ]]; then
      echo
      echo "DO NOT restart and continue blind. Report the ledger position above."
    fi
    echo
    echo "--- closing table (driver) ---"
    sed -n '/^state /,$p' "$RUN_LOG" | tail -60
  } > "$DONE"
  echo "wrote $DONE (state=$STOP_REASON exit=$rc)"
}
trap finish EXIT

{
  echo "=== ei-3b-sep-backfill ==="
  echo "started:  $started"
  echo "head:     $HEAD_SHA"
  echo "scli:     $SCLI ($(date -d "@$(stat -c %Y "$SCLI")" -Is))"
  echo "daemon:   $DAEMON (pid $DPID)"
  echo "claim:    $CLAIM_STATE"
  echo "box:      $BUSY_NOTE — ${avail} GB available (floor ${FREE_FLOOR_GB}), builds=$builds"
  echo "atlases:  $atlases sep-*  |  atoms_ann $ann_before  |  atoms.lance $v2_before"
  echo
} | tee -a "$RUN_LOG"

# ── one leg: the driver, watched batch by batch ─────────────────────────────
run_leg() { # <leg-name> <driver args...>
  local leg=$1; shift
  echo "=== leg $leg: $DRIVER $* ===" | tee -a "$RUN_LOG"
  "$DRIVER" "$@" >>"$RUN_LOG" 2>&1 &
  local drv=$!
  echo "  driver pid $drv — batches marked in $MARKERS"

  # `tail --pid` ends when the driver does; the loop sees each line as it lands.
  # It runs in a subshell (pipeline), so the stop decision is communicated out
  # through `$HERE/.stop`, not a variable.
  tail -n +1 -F --pid="$drv" "$RUN_LOG" 2>/dev/null | {
    local batch=0
    while IFS= read -r line; do
      case "$line" in
        progress:*)
          batch=$(( batch + 1 ))
          local avail_now; avail_now=$(free_gb)
          printf '%s leg=%s batch=%03d %s free_avail_gb=%s ledger_lines=%s\n' \
            "$(date -Is)" "$leg" "$batch" "$line" "$avail_now" "$(ledger_lines)" >> "$SAMPLER"
          # `progress:` is only ever printed AFTER the batch's per-corpus ledger
          # lines are written, so a marker means the ledger is already durable
          # for that batch — which is what makes resume exact.
          printf '{"leg":"%s","batch":%d,"at":"%s","progress":"%s","free_avail_gb":%s,"ledger_lines":%s,"daemon_alive":%s}\n' \
            "$leg" "$batch" "$(date -Is)" "${line#progress: }" "$avail_now" "$(ledger_lines)" \
            "$(kill -0 "$DPID" 2>/dev/null && echo true || echo false)" \
            > "$MARKERS/leg${leg}-batch-$(printf '%03d' "$batch").json"
          # THE STOP CONDITION.
          if ! kill -0 "$DPID" 2>/dev/null; then
            echo "DAEMON GONE (pid $DPID) after leg $leg batch $batch — stopping the battery" | tee -a "$RUN_LOG"
            echo "oom-stop" > "$HERE/.stop"
            kill "$drv" 2>/dev/null
            break
          fi
          ;;
      esac
    done
  }
  wait "$drv"; local rc=$?
  echo "$rc" > "$MARKERS/leg${leg}.rc"
  echo "  leg $leg exit $rc" | tee -a "$RUN_LOG"
  return "$rc"
}

# ── leg 1: the smoke ────────────────────────────────────────────────────────
run_leg 1 --limit 5
LEG1_RC=$(cat "$MARKERS/leg1.rc")
if [[ -f "$HERE/.stop" ]]; then
  STOP_REASON=$(cat "$HERE/.stop"); rm -f "$HERE/.stop"; FINAL_RC=1; exit 1
fi
if [[ "$LEG1_RC" != 0 ]]; then
  STOP_REASON="smoke-failed"; FINAL_RC=$LEG1_RC
  echo "leg 1 (smoke) exited $LEG1_RC — NOT starting the 2.5 h sweep" | tee -a "$RUN_LOG"
  exit "$LEG1_RC"
fi
if [[ -n "${SMOKE_ONLY:-}" ]]; then
  STOP_REASON="smoke-only"; FINAL_RC=0; exit 0
fi

# ── leg 2: the full sweep (the driver's resume skips leg 1's five) ──────────
run_leg 2
LEG2_RC=$(cat "$MARKERS/leg2.rc")
if [[ -f "$HERE/.stop" ]]; then
  STOP_REASON=$(cat "$HERE/.stop"); rm -f "$HERE/.stop"; FINAL_RC=1; exit 1
fi
FINAL_RC=$LEG2_RC
exit "$LEG2_RC"
