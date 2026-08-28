#!/usr/bin/env bash
# Let the pinned control finish, then stop the arm before the 28/5 replicate.
#
# Lives in a FILE, not an inline -c string, for a reason paid for four times
# today: a `pgrep -f`/`grep` pattern typed at a shell matches the shell's OWN
# command line, and this repo's paths appear in every such line. A script's
# argv is just its path, so the patterns below cannot match it.
#
# Never edits the running sequencer either — bash reads a script incrementally
# and rewriting one mid-execution can corrupt it (see run-ceiling.sh's header).
set -u
SEQ=${1:?usage: stop-after-control.sh <sequencer-pid>}
REC=research/deep-research/drb/overall-derivation/flights-ceiling/pinned-control/t69-r1.record.json
SELF=$$
scored () { grep -q '"overall_score"' "$REC" 2>/dev/null; }
echo "watching for the control to score (sequencer $SEQ)…"
while ! scored; do
  kill -0 "$SEQ" 2>/dev/null || { echo "sequencer exited before the control scored"; break; }
  sleep 20
done
scored && echo "control SCORED at $(date -Is)" || echo "control did not score"
kill "$SEQ" 2>/dev/null && echo "sequencer stopped"
# Kill a replicate only if the sequencer already launched one in the gap.
for p in $(ps -eo pid=,args= | grep -F 'run-ceiling.sh wide-28-5' \
             | grep -v -F 'stop-after-control' | awk -v s="$SELF" '$1!=s {print $1}'); do
  kill "$p" 2>/dev/null && echo "stopped a replicate that had just started (pid $p)"
done
echo "=== ARM STOPPED AFTER THE CONTROL $(date -Is) ==="
