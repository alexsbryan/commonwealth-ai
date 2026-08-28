#!/usr/bin/env bash
# Progress for a running (or finished) code-intel corpus pass.
# Reads the checkpoint lines the pass already emits — no extra instrumentation.
set -uo pipefail
CORPUS="${1:-commonwealth-ai}"
LOG="/Users/alexsbryan/dev/commonwealth-ai/runs/code-intel-$CORPUS/run.log"
[ -f "$LOG" ] || { echo "no run log at $LOG"; exit 1; }

LABEL="ai.sovereign.enrich.code-intel-$CORPUS"
if launchctl list 2>/dev/null | grep -q "$LABEL"; then
  echo "job:   $(launchctl list | grep "$LABEL" | awk '{print "pid="$1" laststatus="$2}')"
else
  echo "job:   not loaded"
fi

sed 's/\x1b\[[0-9;]*m//g' "$LOG" | grep "checkpoint saved" | /usr/bin/python3 -c '
import sys,re,datetime
pts=[]
for l in sys.stdin:
    t=re.match(r"(\S+)",l).group(1)
    d=dict(re.findall(r"(\w+)=(\d+)",l))
    pts.append((datetime.datetime.fromisoformat(t.replace("Z","+00:00")),
                int(d["chunk"]),int(d["total_chunks"]),int(d["done"]),int(d["failed"])))
if not pts:
    print("no checkpoints yet"); raise SystemExit
t,c,tc,done,failed = pts[-1]
total = tc*200
# Rate from the last up-to-10 checkpoints: recent enough to reflect current load,
# wide enough that one slow chunk does not set the ETA.
w = pts[-10:] if len(pts)>1 else pts
if len(w)>1:
    dt=(w[-1][0]-w[0][0]).total_seconds(); dd=w[-1][3]-w[0][3]
    r=dt/max(dd,1)
else:
    r=float("nan")
print(f"chunk: {c}/{tc}   symbols: {done} (~{total} total)   failed: {failed} ({100*failed/max(done,1):.2f}%)")
print(f"rate:  {r:.3f} s/symbol (last {len(w)} checkpoints)")
rem=total-done
if rem>0 and r==r:
    eta=(t+datetime.timedelta(seconds=rem*r)).strftime("%H:%M")
    print(f"eta:   {rem} left -> {rem*r/3600:.2f} h  (~{eta} UTC)")
print("last checkpoint: " + t.strftime("%H:%M:%S") + " UTC")
'
# Exit 0 whether or not the run has finished — this reports, it does not gate.
# (It used to end on a bare grep, so "still running" exited 1 and read as an error.)
if sed 's/\x1b\[[0-9;]*m//g' "$LOG" | grep -qE "=== done ==="; then
  echo "state: FINISHED — $(sed 's/\x1b\[[0-9;]*m//g' "$LOG" | grep 'exit=' | tail -1)"
else
  echo "state: running"
fi
exit 0
