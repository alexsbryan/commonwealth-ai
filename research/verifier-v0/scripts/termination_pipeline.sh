#!/usr/bin/env bash
# Wait out the peer, then judge. The box is shared: the daemon currently holds
# a 30GB primary for someone else's job, so this waits for room rather than
# competing for it. Every wait is bounded and says why it gave up.
set -u
cd "$(dirname "$0")/.."
LOG=runs/delta/pipeline.log
say() { echo "[$(date +%H:%M:%S)] $*" | tee -a $LOG; }

say "waiting for generation to finish"
i=0
# NOTE the bracket: `pgrep -f` matches FULL command lines, and the shell that
# wrote this script via heredoc carries the pattern in its own argv -- so a
# plain pattern self-matches an ancestor and waits forever. Observed
# 2026-09-08: the waiter sat on "waiting for generation" for 30min after
# generation had finished cleanly. The bracket breaks the literal match.
while pgrep -f "termination[_]sweep.py --phase generate" >/dev/null 2>&1; do
  i=$((i+1)); [ $i -gt 480 ] && { say "ABORT: generation still running after 80min"; exit 1; }
  sleep 10
done
[ -s runs/delta/trajectory.jsonl ] || { say "ABORT: no trajectory.jsonl produced"; exit 1; }
say "generation done: $(wc -l < runs/delta/trajectory.jsonl) rows"

say "waiting for >=14GB available (peer holds a 30GB primary)"
i=0
while [ "$(free -g | awk '/^Mem:/{print $7}')" -lt 14 ]; do
  i=$((i+1))
  [ $((i % 30)) -eq 0 ] && say "  still waiting; available=$(free -g | awk '/^Mem:/{print $7}')GB"
  [ $i -gt 360 ] && { say "ABORT: box never freed 14GB in 60min; judge phase NOT run"; exit 2; }
  sleep 10
done
say "room available: $(free -g | awk '/^Mem:/{print $7}')GB — serving rung-1000"

~/dev/llama.cpp/build/bin/llama-server \
  -m runs/scored/rung-1000/rung-1000-q8.gguf --port 8089 -c 16384 \
  --parallel 4 -ngl 99 --no-warmup --alias rung-1000 > runs/delta/server.log 2>&1 &
SRV=$!
trap 'kill $SRV 2>/dev/null || true' EXIT

i=0
until curl -sf http://127.0.0.1:8089/v1/models >/dev/null 2>&1; do
  i=$((i+1)); [ $i -gt 150 ] && { say "ABORT: server never came up"; tail -15 runs/delta/server.log | tee -a $LOG; exit 3; }
  sleep 2
done
say "server up after ~$((i*2))s — judging"
python3 scripts/termination_sweep.py --phase judge --passes 5 --concurrency 4 2>&1 | tee -a $LOG
rc=${PIPESTATUS[0]}
say "judge phase exit=$rc; stopping server"
exit $rc
