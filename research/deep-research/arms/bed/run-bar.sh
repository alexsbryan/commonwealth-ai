#!/usr/bin/env bash
# THE AIQ BAR — AIQ's own published articles through OUR pinned 27B judge, so
# the bar is MEASURED rather than translated by an offset (drb/bars.json).
#
#   run-bar.sh                 # resume the newest flight automatically
#   run-bar.sh --fresh         # ignore prior sidecars, judge all ten
#   run-bar.sh --resume <dir>  # resume a specific race-* dir
#   run-bar.sh --stop          # stop the running judge, leaving the sidecar
#   run-bar.sh --status        # where the run is, and what is on disk
#
# WHY THIS IS A SCRIPT AND NOT A COMMAND YOU TYPE. Two hazards, both paid for.
# (1) `pkill -f "python3 -u score_race.py"` typed at a shell MATCHES THE SHELL
#     RUNNING IT, because the pattern is in its own command line — it kills the
#     terminal, not the judge. Stopping goes through the recorded pid here.
# (2) The 2026-08-26 reboot destroyed a driver script and a run log that lived
#     in /tmp. Everything this writes lands beside the run.
set -u
cd /home/alexbryan/dev/commonwealth-ai/research/deep-research/drb/overall-derivation
RUNS=/home/alexbryan/dev/commonwealth-ai/research/deep-research/arms/runs-aiq-bar
FLIGHTS=flights-aiq-bar
JUDGE=Qwen3.8-27B-UD-Q6_K_XL
mkdir -p "$RUNS"

newest_sidecar () {   # the race dir whose sidecar carries the MOST ids
  local best="" n=0 c
  for f in "$FLIGHTS"/race-*/ab-aiq/judge_output.jsonl; do
    [ -f "$f" ] || continue
    c=$(grep -c . "$f" 2>/dev/null || echo 0)
    [ "$c" -gt "$n" ] && { n=$c; best=$(dirname "$(dirname "$f")"); }
  done
  echo "$best"
}

case "${1:-}" in
  --stop)
    p=$(cat "$RUNS/.bar.pid" 2>/dev/null || true)
    if [ -n "$p" ] && kill -0 "$p" 2>/dev/null; then kill "$p"; echo "stopped judge pid $p"
    else echo "no judge running (pid file: ${p:-none})"; fi
    exit 0;;
  --status)
    p=$(cat "$RUNS/.bar.pid" 2>/dev/null || true)
    if [ -n "$p" ] && kill -0 "$p" 2>/dev/null; then echo "RUNNING pid $p"; else echo "not running"; fi
    d=$(newest_sidecar)
    [ -n "$d" ] && { echo "newest sidecar: $d"
      python3 -c "
import json,sys
ids=[json.loads(l)['id'] for l in open('$d/ab-aiq/judge_output.jsonl') if l.strip()]
todo=[i for i in [56,58,59,62,65,69,78,83,90,95] if i not in ids]
print(f'  judged {len(ids)}/10: {ids}')
print(f'  remaining: {todo}')"; }
    [ -f "$RUNS/.bar.log" ] && tail -3 "$(cat "$RUNS/.bar.log")"
    exit 0;;
esac

# THE JUDGE WINDOW MUST BE OPEN. score_race.py refuses (exit 2, no judge call)
# if the pin is not loaded, and a cold daemon after a reboot never is.
if ! curl -sf -m 60 http://127.0.0.1:9741/v1/models 2>/dev/null | grep -q .; then
  echo "REFUSED: daemon not answering on :9741"; exit 2; fi
echo "opening the judge window (loads $JUDGE if cold)…"
curl -s -m 900 http://127.0.0.1:9741/v1/chat/completions -H 'Content-Type: application/json' \
  -d "{\"model\":\"$JUDGE\",\"max_tokens\":1,\"messages\":[{\"role\":\"user\",\"content\":\"ok\"}]}" \
  -o /dev/null -w '  judge probe http=%{http_code}\n'

# A ~12 GiB `rust-analyzer scip` child is the difference between the largest
# task fitting under the ~55 GiB wall and not. NEVER kill it — a half-killed
# export wipes the code-intel graph. Wait it out.
while pgrep -f "rust-analyzer scip" >/dev/null; do
  echo "  waiting on a rust-analyzer scip export (~12 GiB competitor)…"; sleep 60
done

RESUME=""
case "${1:-}" in
  --fresh)  echo "fresh: every task judged";;
  --resume) RESUME="$2";;
  "")       RESUME=$(newest_sidecar);;
  *)        echo "unknown arg: $1"; exit 2;;
esac
[ -n "$RESUME" ] && echo "resuming from $RESUME (seeded ids are REUSED, never re-judged)"

TS=$(date +%Y%m%dT%H%M%S); LOG=$RUNS/bar-$TS.log
nohup python3 -u score_race.py --arm ab --peer aiq --arm-label ab-aiq \
  --out "$FLIGHTS" ${RESUME:+--resume "$RESUME"} > "$LOG" 2>&1 &
sleep 3
# the PYTHON pid, never the nohup wrapper: `$!` after `nohup … &` inside a
# harness eval has twice captured a wrapper and orphaned a live judge, which
# then contended for the single daemon slot and restarted it mid-request.
PID=$(pgrep -f "score_race[.]py" | head -1)
echo "$PID" > "$RUNS/.bar.pid"; echo "$LOG" > "$RUNS/.bar.log"
echo "judge pid $PID   log $LOG"
