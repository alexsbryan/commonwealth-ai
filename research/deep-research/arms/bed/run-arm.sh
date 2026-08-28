#!/usr/bin/env bash
# THE MULTI-TASK REPLAY BED — run one arm across every bed task, judge it, and
# report what the result can and cannot resolve.
#
#   run-arm.sh <arm-name> [--reps N] [--env KEY=VAL]...
#
# WHY THIS EXISTS (measured 2026-08-25, note 86ac6f7c). Three runs at IDENTICAL
# configuration on the task-69 replay bed scored 42.21 / 41.99 / 48.91 — a
# 6.92-point spread with evidence held constant. Every A/B before this ran n=2
# on ONE task against 2-3 point levers, so none of them could resolve, and one
# of them (a 0.22 within-arm spread from two adjacent runs) was mistaken for
# precision. Two changes answer that: MORE CELLS (tasks x reps, so the standard
# error falls as sqrt(n) instead of resting on one draw) and ONE TASK IS NOT A
# BED (iterating on task 69 alone overfits one A2A/MCP question toward a
# 100-task bar).
#
# WHAT IT REFUSES TO DO. It never prints a mean without the spread beside it.
# It never turns a could-not-judge into a zero — the ruler censors some
# articles deterministically under its greedy pin, and a missing cell is
# reported as missing. It never declares a winner; it reports the delta and
# the floor, and the caller compares them against a bar registered BEFOREHAND.
set -u
cd /home/alexbryan/dev/commonwealth-ai
BED=research/deep-research/arms/bed/bed.json
QUERIES=/home/alexbryan/dev/deep_research_bench/data/prompt_data/query.jsonl
CLI=./target/debug/sovereign-cli
[ $# -ge 1 ] || { echo "usage: run-arm.sh <arm-name> [--reps N] [--env KEY=VAL]..."; exit 2; }
ARM=$1; shift
REPS=2; ENVS=()
while [ $# -gt 0 ]; do
  case "$1" in
    --reps) REPS=$2; shift 2;;
    --env)  ENVS+=("$2"); shift 2;;
    *) echo "unknown arg: $1"; exit 2;;
  esac
done
ROOT=research/deep-research/arms/runs-bed/$ARM
FLIGHTS=research/deep-research/drb/overall-derivation/flights-bed/$ARM
mkdir -p "$ROOT" "$FLIGHTS"

rss_gb () { local p; p=$(pgrep -f "debug/sovereign-cli-daemon daemon run" | head -1); [ -n "$p" ] && echo $(( $(ps -o rss= -p "$p" 2>/dev/null || echo 0) / 1048576 )) || echo 0; }
ready () { for i in $(seq 1 120); do curl -sf --max-time 10 http://127.0.0.1:9741/v1/models >/dev/null 2>&1 && return 0; sleep 10; done; return 1; }
# THE WALL GATE — one decider, shared with run-ceiling.sh and score_race.py
# (ARCH 10.6): research/deep-research/arms/lab/host_wall.py.
#
# WHAT THIS REPLACED. The old settle restarted the daemon whenever its RSS
# passed 25 GiB. A daemon restart SPAWNS a ~11 GiB `rust-analyzer scip` child,
# and that child is exactly what turns a survivable judge prefill into an OOM
# kill on a host whose measured wall is ~55 GiB — so the remedy manufactured
# the disease. Seven kills on 2026-08-26 ended in a forced reboot. Its
# "36-38G kill band" was stale too; the kernel log puts them at 43-57 GiB.
# Wait for the competitor instead; never kill a running scip export, and never
# restart the daemon as a memory precaution mid-flight.
settle () {
  local art_chars=${1:-0} est
  est=$(( art_chars * 2 ))
  [ "$est" -lt 40000 ] && est=40000
  python3 research/deep-research/arms/lab/host_wall.py --wait "$est" | sed "s/^/    /"
  local rc=${PIPESTATUS[0]}
  [ "$rc" -ne 0 ] && return 1
  ready || return 1
}
scored () { grep -q '"overall_score"' "$1" 2>/dev/null; }

echo "=== BED ARM '$ARM' START $(date -Is) ==="
echo "    reps/task: $REPS   env: ${ENVS[*]:-<none>}   HEAD $(git rev-parse --short HEAD)"
ready || { echo "REFUSED: daemon down"; exit 2; }

mapfile -t TASKS < <(python3 -c "
import json; [print('%s %s'%(t['id'],t['estate'])) for t in json.load(open('$BED'))['tasks']]")

for entry in "${TASKS[@]}"; do
  TID=${entry%% *}; EST=${entry##* }
  Q=$(python3 -c "
import json
for l in open('$QUERIES'):
    r=json.loads(l)
    if int(r['id'])==$TID: print(r['prompt']); break")
  [ -z "$Q" ] && { echo "task $TID: NO PROMPT — skipped, not scored"; continue; }
  for rep in $(seq 1 "$REPS"); do
    CELL="t${TID}-r${rep}"; RUN="$ROOT/$CELL"
    if ! ls "$RUN"/dr-*/report.md >/dev/null 2>&1; then
      echo "--- $CELL flight $(date -Is) ---"
      ready || continue
      rm -rf "$RUN"; mkdir -p "$RUN"
      env "${ENVS[@]}" SOVEREIGN_DR_COMPOSED_REPORT=1 RUST_LOG=deep_research=info,warn \
        $CLI deep-research "$Q" --run-dir "$RUN" --max-rounds 2 --search 40 --fetch 100 \
        --search-source corpus --corpora "$EST" > "$ROOT/$CELL.log" 2>&1
      rp=$(ls "$RUN"/dr-*/report.md 2>/dev/null | head -1)
      [ -n "$rp" ] && echo "    $CELL flew — $(wc -w < "$rp") words" || { echo "    $CELL FLIGHT FAILED"; tail -2 "$ROOT/$CELL.log"; continue; }
    fi
    rec="$FLIGHTS/$CELL.record.json"; scored "$rec" && { echo "    $CELL already scored"; continue; }
    art=$(ls "$RUN"/dr-*/report.md 2>/dev/null | head -1); [ -z "$art" ] && continue
    ART_ABS=$(readlink -f "$art")   # absolute: the scorer runs from another dir
    ART_CHARS=$(wc -c < "$ART_ABS" 2>/dev/null || echo 0)
    for attempt in 1 2; do
      settle "$ART_CHARS" || break
      ( cd research/deep-research/drb/overall-derivation && \
        LLM_BACKEND=openai OPENAI_BASE_URL=http://127.0.0.1:9741/v1 OPENAI_API_KEY=local \
        python3 ../../arms/lab/score_one.py --task "$TID" --article "$ART_ABS" \
          --save-judge "flights-bed/$ARM/$CELL.judge.jsonl" ) > "$rec" 2>&1 || true
      scored "$rec" && { echo "    $CELL scored $(grep -o '\"overall_score\": [0-9.]*' "$rec" | head -1)"; break; }
      # Distinguish the two failure classes: transport is worth a retry, a
      # missing dimension under a GREEDY judge is deterministic and is not.
      if grep -q "missing dims\|empty dims" "$rec"; then
        echo "    $CELL COULD-NOT-JUDGE (judge omitted a dimension — deterministic under the greedy pin, no retry)"; break
      fi
      echo "    $CELL judge attempt $attempt failed (transport) — retrying"
    done
    scored "$rec" || echo "    $CELL UNSCORED — reported as missing, never as zero"
  done
done

echo ""; echo "=== BED ARM '$ARM' RESULT $(date -Is) ==="
ARM="$ARM" FLIGHTS="$FLIGHTS" ROOT="$ROOT" BED="$BED" python3 - <<'PY'
import json,os,re,glob,statistics as st
A=os.environ['ARM']; F=os.environ['FLIGHTS']; R=os.environ['ROOT']
tasks=[t['id'] for t in json.load(open(os.environ['BED']))['tasks']]
def rec(c):
    f=f'{F}/{c}.record.json'
    if not os.path.exists(f): return None
    t=open(f).read(); m=re.search(r'\{\s*\n\s*"id"',t)
    if not m: return None
    try: return json.loads(t[m.start():t.index(chr(10)+'}',m.start())+2])
    except Exception: return None
per, allv, missing = {}, [], []
for tid in tasks:
    vals=[]
    for c in sorted(glob.glob(f'{R}/t{tid}-r*')):
        cell=os.path.basename(c); r=rec(cell)
        if r is None: missing.append(cell); continue
        vals.append(100*r['overall_score'])
    if vals: per[tid]=vals; allv+=vals
print('%-6s %5s %8s %8s   cells' % ('task','n','mean','spread'))
for tid,v in per.items():
    print('%-6d %5d %8.2f %8.2f   %s' % (tid, len(v), st.mean(v), (max(v)-min(v)) if len(v)>1 else 0,
          ' '.join('%.2f'%x for x in v)))
if missing: print('MISSING (could-not-judge or failed flight, NOT zeros): %s' % ', '.join(missing))
if allv:
    m=st.mean(allv); sd=st.stdev(allv) if len(allv)>1 else 0
    se=sd/len(allv)**0.5 if len(allv)>1 else 0
    print('\nPOOLED  n=%d  mean %.2f  sd %.2f  se %.2f' % (len(allv), m, sd, se))
    print('RESOLVING POWER: this arm can distinguish a delta of about %.2f points (2 x se).' % (2*se))
    print('  A smaller delta is NOT resolved by this arm no matter which way it points.')
    print('  Reference: 6.92-point spread measured at identical config, n=3, task 69 (note 86ac6f7c).')
    json.dump({'arm':A,'per_task':per,'pooled_mean':m,'sd':sd,'se':se,'n':len(allv),'missing':missing},
              open(f'{F}/summary.json','w'), indent=2)
    print('  summary -> %s/summary.json' % F)
else:
    print('NO CELLS SCORED — this arm resolved nothing.')
PY
echo "=== BED ARM '$ARM' DONE $(date -Is) ==="
