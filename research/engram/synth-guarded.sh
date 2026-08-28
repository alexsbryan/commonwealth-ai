#!/usr/bin/env bash
# Run ONE synth lane under a memory guard, sampling the envelope throughout.
#
# WHY A GUARD: on 2026-08-26 20:39:20 the kernel OOM killer fired inside the
# toolbox scope while serving Flash-Next under ci-bench load, killing the daemon
# and voiding 18 downstream lanes. That is a MEASURED hazard, so this re-run
# encodes the brake rather than repeating the run and hoping. The guard kills
# the EVAL CHILD (never the daemon) when MemAvailable falls under GUARD_MIB, so
# the kernel never has to choose a victim and partial results survive.
#
# Usage: synth-guarded.sh <corpus> [sample_n]   (sample_n omitted = FULL bank)
set -uo pipefail
CORPUS="${1:?corpus (sep|wikipedia)}"
SAMPLE="${2:-}"
GUARD_MIB="${GUARD_MIB:-6144}"
ROOT=/home/alexbryan/dev/commonwealth-ai
OUT="$ROOT/research/engram/synth-$CORPUS$([ -n "$SAMPLE" ] && echo "-n$SAMPLE").log"
TSV="$ROOT/research/engram/synth-$CORPUS$([ -n "$SAMPLE" ] && echo "-n$SAMPLE").mem.tsv"
REPORT="$ROOT/target/ci-bench/synth-$CORPUS-rerun.json"
mkdir -p "$ROOT/target/ci-bench"

ARGS=(bench all --bench-root "$ROOT/sovereign/bench" --synth --filter "$CORPUS" --report "$REPORT")
[ -n "$SAMPLE" ] && ARGS+=(--sample-questions "$SAMPLE")

printf 'ts\tavail_mib\tgtt_mib\tdaemon_anon_mib\tdaemon_file_mib\teval_rss_mib\n' > "$TSV"
"$ROOT/target/debug/sovereign-cli-llm" "${ARGS[@]}" > "$OUT" 2>&1 &
EVAL_PID=$!
echo "eval pid=$EVAL_PID guard=${GUARD_MIB}MiB corpus=$CORPUS sample=${SAMPLE:-FULL}"
echo "  log=$OUT"

gtt() { local f=/sys/class/drm/card1/device/mem_info_gtt_used; [ -f "$f" ] && echo $(( $(cat "$f") / 1048576 )) || echo 0; }
fld() { grep -E "^$2:" "/proc/$1/status" 2>/dev/null | awk '{print int($2/1024)}'; }
dpid() { pgrep -f 'sovereign-cli-daemon daemon run' | head -1; }

TRIPPED=0
while kill -0 "$EVAL_PID" 2>/dev/null; do
  avail=$(awk '/MemAvailable/{print int($2/1024)}' /proc/meminfo)
  d=$(dpid)
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$(date +%H:%M:%S)" "$avail" "$(gtt)" \
    "$([ -n "$d" ] && fld "$d" RssAnon || echo 0)" \
    "$([ -n "$d" ] && fld "$d" RssFile || echo 0)" \
    "$(fld "$EVAL_PID" VmRSS || echo 0)" >> "$TSV"
  if [ "$avail" -lt "$GUARD_MIB" ]; then
    TRIPPED=1
    echo "GUARD TRIPPED: MemAvailable ${avail}MiB < ${GUARD_MIB}MiB — reaping the eval tree (daemon untouched)" | tee -a "$OUT"
    # Reap the CHILD FIRST. `bench all` spawns an `eval run` child that holds the
    # multi-GiB working set; the parent is ~2 MB, so signalling only EVAL_PID
    # frees nothing and the child keeps running orphaned (observed 2026-08-26).
    for c in $(pgrep -P "$EVAL_PID" 2>/dev/null) $(pgrep -f 'sovereign-cli-llm eval run' 2>/dev/null); do
      kill -TERM "$c" 2>/dev/null
    done
    sleep 4
    for c in $(pgrep -f 'sovereign-cli-llm eval run' 2>/dev/null); do kill -KILL "$c" 2>/dev/null; done
    kill -TERM "$EVAL_PID" 2>/dev/null; sleep 3; kill -KILL "$EVAL_PID" 2>/dev/null
    break
  fi
  sleep 2
done
wait "$EVAL_PID" 2>/dev/null; RC=$?
echo "── synth:$CORPUS rc=$RC guard_tripped=$TRIPPED min_avail=$(awk 'NR>1{if(m==""||$2<m)m=$2}END{print m"MiB"}' "$TSV")"
exit "$RC"
