#!/usr/bin/env bash
# Run the cartridge state-restore spike against Qwen3.8-Flash-Next.
#
# THE QUESTION (pre-registered, note d63781e3): is full-context-state
# save/restore bit-faithful on arch qwen4exp? `ple_hist` is a mutable map on
# the MODEL, not the context, and no session file serializes it — so ctx B
# should hit the `next_pos != pos` reset and diverge from ctx A.
#
# DECISION RULE, fixed before the run:
#   32/32 match  -> the note is FALSIFIED. ple_hist either round-trips or the
#                   engram's contribution is below greedy-decode resolution.
#                   Either way prefix-state restore is safe here; say which.
#   <32 match    -> CONFIRMED. Record the divergence index: 0/1/2 is the
#                   predicted window (2 tokens of EOS-padded n-gram context).
#                   A divergence LATER than token 2 is NOT this mechanism and
#                   must not be reported as it.
#   load abort / OOM / timeout -> VOID, not a result.
#
# SAFETY. The daemon is stopped for the run (it holds ~18.6 GiB of GTT at idle
# and the model needs ~95). The trap restarts it on EVERY exit path including
# Ctrl-C and the memory kill. The test binary is driven DIRECTLY, not via
# `cargo test`: note 401428eb's guard signalled the ~2 MB parent while the
# multi-GiB working set lived in a child that survived and kept running.
set -uo pipefail
cd "$(dirname "$0")/../.."

BIN="${1:?usage: spike-run.sh <test-binary>}"
MODEL="sovereign/models/Qwen3.8-Flash-Next/Qwen3.8-Flash-Next-UD-Q4_K_XL-00001-of-00004.gguf"
OUT=research/engram/spike-flashnext.log
MEM=research/engram/spike-flashnext.mem.tsv
FLOOR_MIB=4096
TIMEOUT=1800

# Disk-backed, NOT /tmp: /tmp is tmpfs here, and the session file for a 48-layer
# hybrid is large enough that putting it in RAM competes with the load.
export TMPDIR="$PWD/research/engram/spike-tmp"
mkdir -p "$TMPDIR"

restore_daemon() {
  echo "[$(date +%T)] restoring daemon…" | tee -a "$OUT"
  sovereign daemon start >>"$OUT" 2>&1
  sleep 5
  sovereign daemon status 2>&1 | grep -v '^svrnmesh:' | tee -a "$OUT"
}
trap restore_daemon EXIT

: > "$OUT"; : > "$MEM"
echo "[$(date +%T)] === spike start ===" | tee -a "$OUT"

echo "[$(date +%T)] stopping daemon" | tee -a "$OUT"
sovereign daemon stop >>"$OUT" 2>&1
sleep 8
# The daemon respawns `rust-analyzer scip .` (~10 GiB) as a child on restart
# (note 0b7eb9f3). If one survived the stop it is competing for the memory this
# load needs, so it goes too — it is a re-runnable indexer, not state.
if pgrep -f "rust-analyzer scip" >/dev/null; then
  echo "[$(date +%T)] killing surviving rust-analyzer scip child" | tee -a "$OUT"
  pkill -f "rust-analyzer scip"; sleep 3
fi
echo "[$(date +%T)] pre-run: GTT $(($(cat /sys/class/drm/card1/device/mem_info_gtt_used)/1048576)) MiB, avail $(awk '/MemAvailable/{print int($2/1024)}' /proc/meminfo) MiB" | tee -a "$OUT"

SPIKE_GGUF="$MODEL" setsid "$BIN" --ignored --nocapture --test-threads=1 >>"$OUT" 2>&1 &
PID=$!
PGID=$(ps -o pgid= -p $PID | tr -d ' ')
echo "[$(date +%T)] test pid=$PID pgid=$PGID (floor ${FLOOR_MIB} MiB, timeout ${TIMEOUT}s)" | tee -a "$OUT"

printf "t\tavail_mib\tgtt_mib\trss_mib\n" >> "$MEM"
TRIPPED=0; ELAPSED=0
while kill -0 "$PID" 2>/dev/null; do
  AVAIL=$(awk '/MemAvailable/{print int($2/1024)}' /proc/meminfo)
  GTT=$(( $(cat /sys/class/drm/card1/device/mem_info_gtt_used 2>/dev/null || echo 0) / 1048576 ))
  RSS=$(( $(awk '/VmRSS/{print $2}' /proc/$PID/status 2>/dev/null || echo 0) / 1024 ))
  printf "%s\t%s\t%s\t%s\n" "$ELAPSED" "$AVAIL" "$GTT" "$RSS" >> "$MEM"
  if (( AVAIL < FLOOR_MIB )); then
    echo "[$(date +%T)] MEMORY FLOOR TRIPPED at ${AVAIL} MiB — killing process GROUP $PGID" | tee -a "$OUT"
    kill -TERM -"$PGID" 2>/dev/null; sleep 5; kill -KILL -"$PGID" 2>/dev/null
    TRIPPED=1; break
  fi
  if (( ELAPSED > TIMEOUT )); then
    echo "[$(date +%T)] TIMEOUT after ${ELAPSED}s — killing process GROUP $PGID" | tee -a "$OUT"
    kill -TERM -"$PGID" 2>/dev/null; sleep 5; kill -KILL -"$PGID" 2>/dev/null
    TRIPPED=2; break
  fi
  sleep 2; ELAPSED=$((ELAPSED+2))
done
wait "$PID" 2>/dev/null; RC=$?

echo "[$(date +%T)] test rc=$RC tripped=$TRIPPED elapsed=${ELAPSED}s min_avail=$(awk 'NR>1{if(m==""||$2<m)m=$2}END{print m}' "$MEM") MiB peak_gtt=$(awk 'NR>1{if($3>m)m=$3}END{print m}' "$MEM") MiB" | tee -a "$OUT"
rm -rf "$TMPDIR"
exit $RC
