#!/usr/bin/env bash
# n_ubatch sweep for Flash-Next prefill. Bars pre-registered in
# research/engram/PRE-REGISTRATION-ubatch.md — read that BEFORE the numbers.
# Runs INSIDE the sovereign-vulkan toolbox.
set -uo pipefail
R=/home/alexbryan/dev/commonwealth-ai
BENCH="$R/target/llama-cmake-cache/a610ca3db8fb40e1/bin/llama-bench"
MODEL="$R/sovereign/models/Qwen3.8-Flash-Next/Qwen3.8-Flash-Next-UD-Q4_K_XL-00001-of-00004.gguf"
CLI="$R/target/debug/sovereign-cli"
OUT="$R/research/engram/ubatch-sweep.json"
LOG="$R/research/engram/ubatch-sweep.log"
TSV="$R/research/engram/ubatch-sweep.gtt.tsv"
gtt(){ local f=/sys/class/drm/card1/device/mem_info_gtt_used; [ -f "$f" ] && echo $(( $(cat "$f")/1048576 )) || echo 0; }
avail(){ awk '/MemAvailable/{print int($2/1024)}' /proc/meminfo; }
say(){ echo "[$(date +%H:%M:%S)] $*" | tee -a "$LOG"; }

: > "$LOG"
say "=== n_ubatch experiment start ==="
[ -x "$BENCH" ] || { say "FATAL: llama-bench missing at $BENCH"; exit 2; }
[ -f "$MODEL" ] || { say "FATAL: model missing"; exit 2; }

# VOID condition: another tenant mid-flight.
if pgrep -f 'sovereign-cli-llm (bench|eval)' >/dev/null 2>&1; then
  say "ABORT (VOID): a bench/eval is running — another tenant is on the box."; exit 3
fi

DAEMON_WAS_UP=0
if pgrep -f 'sovereign-cli-daemon daemon run' >/dev/null 2>&1; then DAEMON_WAS_UP=1; fi
restore(){
  if [ "$DAEMON_WAS_UP" = "1" ] && ! pgrep -f 'sovereign-cli-daemon daemon run' >/dev/null 2>&1; then
    say "restarting daemon (CLI, never systemctl)"; "$CLI" daemon start >>"$LOG" 2>&1
    sleep 5; pgrep -f 'sovereign-cli-daemon daemon run' >/dev/null 2>&1 && say "daemon back up" || say "WARN: daemon did NOT come back — check by hand"
  fi
}
trap restore EXIT INT TERM

if [ "$DAEMON_WAS_UP" = "1" ]; then
  say "stopping daemon to free GTT (operator-authorized)"; "$CLI" daemon stop >>"$LOG" 2>&1; sleep 8
fi
say "pre-run: GTT $(gtt) MiB, MemAvailable $(avail) MiB"
if [ "$(gtt)" -gt 30000 ]; then say "ABORT (VOID): GTT still $(gtt) MiB — something holds a model."; exit 3; fi

printf 'ts\tgtt_mib\tavail_mib\n' > "$TSV"
( while true; do printf '%s\t%s\t%s\n' "$(date +%H:%M:%S)" "$(gtt)" "$(avail)" >> "$TSV"; sleep 2; done ) &
SAMPLER=$!
trap 'kill '"$SAMPLER"' 2>/dev/null; restore' EXIT INT TERM

say "running sweep: -b 4096 -ub 512,1024,2048,4096 -p 4096 -n 128 -r 3 (no -ngl, see frame dead-end)"
"$BENCH" -m "$MODEL" -b 4096 -ub 512,1024,2048,4096 -p 4096 -n 128 -r 3 \
         -o json --progress > "$OUT" 2>>"$LOG"
RC=$?
kill "$SAMPLER" 2>/dev/null
say "llama-bench rc=$RC   peak GTT $(awk 'NR>1&&$2>m{m=$2}END{print m}' "$TSV") MiB   min avail $(awk 'NR>1{if(m==""||$3<m)m=$3}END{print m}' "$TSV") MiB"
say "=== sweep done, restoring daemon ==="
exit $RC
