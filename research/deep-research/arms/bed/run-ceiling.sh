#!/usr/bin/env bash
# THE MODEL-CEILING ARM — "is the ~11-point gap to AIQ our ARCHITECTURE or our MODEL?"
#
#   run-ceiling.sh <arm-name> --reps N [--task ID] [--env KEY=VAL]... [--control]
#
# WHY A SEPARATE RUNNER FROM run-arm.sh. Two reasons, both structural.
# (1) run-arm.sh pins RUST_LOG=deep_research=info, and the ONLY evidence that
#     SOVEREIGN_DR_ALL_LEGS_SLOW did anything is a tracing::debug! naming
#     ?leg/?speed (port.rs:478). Under info that event never emits, so the arm
#     would be unfalsifiable -- six hours of flights that cannot prove the
#     writer moved off the 4B. This runner forces debug and ASSERTS on it.
# (2) run-arm.sh was mid-flight when this was written. Bash reads a script
#     incrementally; editing a running one can corrupt its execution.
# The thing that MUST NOT fork is the scorer, and it does not: both call
# arms/lab/score_one.py, which shares judge_instrument.py with score_race.py
# (§10.6 -- one judge instrument, one sampling pin).
#
# WHAT IT REFUSES TO DO. It refuses to fly a second cell if the first cell
# cannot prove the routing changed (SOVEREIGN_DR_ALL_LEGS_SLOW arms only).
# A flag that silently no-ops is the exact failure §18.1 exists to catch:
# a green run that verified nothing. It never turns an unscored cell into a
# zero. It never prints a mean without the spread beside it.
set -u
cd /home/alexbryan/dev/commonwealth-ai
QUERIES=/home/alexbryan/dev/deep_research_bench/data/prompt_data/query.jsonl
BED=research/deep-research/arms/bed/bed.json
CLI=./target/debug/sovereign-cli
[ $# -ge 1 ] || { echo "usage: run-ceiling.sh <arm-name> --reps N [--task ID] [--env K=V]... [--control]"; exit 2; }
ARM=$1; shift
REPS=4; TASK=69; ENVS=(); CONTROL=0
while [ $# -gt 0 ]; do
  case "$1" in
    --reps) REPS=$2; shift 2;;
    --task) TASK=$2; shift 2;;
    --env)  ENVS+=("$2"); shift 2;;
    --control) CONTROL=1; shift;;
    *) echo "unknown arg: $1"; exit 2;;
  esac
done
ROOT=research/deep-research/arms/runs-ceiling/$ARM
FLIGHTS=research/deep-research/drb/overall-derivation/flights-ceiling/$ARM
mkdir -p "$ROOT" "$FLIGHTS"

EST=$(python3 -c "
import json
for t in json.load(open('$BED'))['tasks']:
    if int(t['id'])==$TASK: print(t['estate']); break")
[ -n "$EST" ] || { echo "REFUSED: task $TASK has no estate in bed.json"; exit 2; }
Q=$(python3 -c "
import json
for l in open('$QUERIES'):
    r=json.loads(l)
    if int(r['id'])==$TASK: print(r['prompt']); break")
[ -n "$Q" ] || { echo "REFUSED: task $TASK has no prompt"; exit 2; }

rss_gb () { local p; p=$(pgrep -f "debug/sovereign-cli-daemon daemon run" | head -1); [ -n "$p" ] && echo $(( $(ps -o rss= -p "$p" 2>/dev/null || echo 0) / 1048576 )) || echo 0; }
ready () { for i in $(seq 1 120); do curl -sf --max-time 10 http://127.0.0.1:9741/v1/models >/dev/null 2>&1 && return 0; sleep 10; done; return 1; }
# THE WALL GATE — one decider, shared with score_race.py (ARCH 10.6):
# research/deep-research/arms/lab/host_wall.py carries the model and the
# evidence for it.
#
# WHAT THIS REPLACED, AND WHY IT WAS DANGEROUS. The previous settle compared
# daemon RSS against 25 GiB and, above it, ran `daemon restart`. Three things
# wrong with that, all measured on 2026-08-26:
#   1. The daemon sits at ~47 GiB during any flight, so it fired EVERY time.
#   2. A daemon restart SPAWNS a ~11 GiB `rust-analyzer scip` child. The
#      remedy manufactured the competitor that turns a survivable prefill into
#      an OOM kill — seven of them took the machine down that morning.
#   3. Its "kill band 36-38G" was stale; the kernel log puts the kills at
#      43-57 GiB against a ~55 GiB wall.
# The invariant it violated was already written down: never restart the daemon
# as a memory precaution mid-flight — wait. This waits.
#
# The judge prompt is the article plus the reference plus the criteria; on the
# frozen subset it measures ~1.95x article_1 (t69: 75,303-char article ->
# 146,857-char prompt), so 2x the article is the honest conservative estimate
# to gate on. Never kills the scip export: a half-killed export wipes the
# code-intel graph.
settle () {
  local art_chars=${1:-0} est
  est=$(( art_chars * 2 ))
  [ "$est" -lt 40000 ] && est=40000
  python3 research/deep-research/arms/lab/host_wall.py --wait "$est" \
    | sed "s/^/    /"
  local rc=${PIPESTATUS[0]}
  [ "$rc" -ne 0 ] && return 1
  ready || return 1
}
scored () { grep -q '"overall_score"' "$1" 2>/dev/null; }

# THE ROUTING WITNESS. port.rs:478 emits, per drafting leg:
#   deep-research: drafting leg dispatched  leg=Section speed=Fast ...
# Under SOVEREIGN_DR_ALL_LEGS_SLOW=1 EVERY leg must read speed=Slow. Section is
# the leg that writes the deliverable, so Section is the one that matters.
# Returns 0 only if Section was seen AND every Section dispatch was Slow.
# The subscriber (cli-shared/tracing_init.rs -> tracing_subscriber::fmt())
# emits ANSI, so a field is literally ESC[3mleg ESC[0m ESC[2m= ESC[0m Section.
# A naive grep for 'leg=Section' matches NOTHING and the arm would refuse on
# cell 1 after a wasted flight. Strip ANSI first, always.
decolor () { sed -r 's/\x1b\[[0-9;]*m//g' "$1"; }

# THE ARCHITECTURE WITNESS (drb1-r8). SOVEREIGN_DR_REPORT_ARCHITECTURE changes
# the deliverable's SHAPE, and the shape is checkable in the artifact itself —
# cheaper and more direct than a trace event. A cell that flew the flag but
# produced a control-shaped report is a cell that measured the control, and
# scoring it as the arm is exactly the green-that-verified-nothing of ARCH 18.1.
# Two marks, both structural: the H1 must not be the question (the default
# composes `# {question}`), and the report must carry an Executive Summary.
architecture_witness () {
  local report=$1 question=$2
  local h1 ok=0
  h1=$(grep -m1 '^# ' "$report" | sed 's/^# //')
  if [ "$h1" = "$question" ]; then
    echo "      WITNESS: H1 is still the raw question — the flag did not take"
  else
    ok=$((ok+1)); echo "      WITNESS: H1 = \"${h1:0:70}\""
  fi
  if grep -q '^## Executive Summary' "$report"; then
    ok=$((ok+1)); echo "      WITNESS: Executive Summary present"
  else
    echo "      WITNESS: no '## Executive Summary' section"
  fi
  echo "      WITNESS: $(grep -c '^#\{1,3\} ' "$report") headings h1-h3"
  [ "$ok" -eq 2 ]
}
routing_witness () {
  local log=$1 want=$2   # want = Slow | Fast
  local n_evt n_sec n_bad
  n_evt=$(decolor "$log" | grep -c 'drafting leg dispatched' || true)
  if [ "$n_evt" -eq 0 ]; then
    echo "      WITNESS: the debug event never emitted — RUST_LOG did not reach deep_research=debug"
    return 2
  fi
  n_sec=$(decolor "$log" | grep 'drafting leg dispatched' | grep -c 'leg=Section' || true)
  if [ "$n_sec" -eq 0 ]; then
    echo "      WITNESS: $n_evt leg dispatches logged but NONE were leg=Section"; return 2
  fi
  n_bad=$(decolor "$log" | grep 'drafting leg dispatched' | grep 'leg=Section' | grep -vc "speed=$want" || true)
  echo "      WITNESS: $n_evt leg dispatches, $n_sec Section, $n_bad not speed=$want"
  # Report the whole routing table so the record shows what every leg did.
  decolor "$log" | grep 'drafting leg dispatched' \
    | sed -n 's/.*leg=\([A-Za-z]*\).*speed=\([A-Za-z]*\).*/\1 \2/p' \
    | sort | uniq -c | sed 's/^/      routing: /'
  [ "$n_bad" -eq 0 ]
}

echo "=== CEILING ARM '$ARM' START $(date -Is) ==="
echo "    task $TASK   reps $REPS   estate $EST"
echo "    env: ${ENVS[*]:-<none>}"
echo "    HEAD $(git rev-parse --short HEAD)   control-arm: $CONTROL"
ready || { echo "REFUSED: daemon down"; exit 2; }

WANT=Fast; WANT_ARCH=0; WANT_WIDE=0
for e in "${ENVS[@]:-}"; do
  case "$e" in
    SOVEREIGN_DR_ALL_LEGS_SLOW=1|SOVEREIGN_DR_ALL_LEGS_SLOW=true) WANT=Slow;;
    SOVEREIGN_DR_REPORT_ARCHITECTURE=1|SOVEREIGN_DR_REPORT_ARCHITECTURE=true) WANT_ARCH=1;;
    SOVEREIGN_DR_REPORT_SECTION_EVIDENCE=1|SOVEREIGN_DR_REPORT_SECTION_EVIDENCE=true) WANT_WIDE=1;;
  esac
done
echo "    routing witness expects Section speed=$WANT"
[ "$WANT_ARCH" = "1" ] && echo "    architecture witness expects a planned H1 + an Executive Summary"

WITNESS_OK=unknown
for rep in $(seq 1 "$REPS"); do
  CELL="t${TASK}-r${rep}"; RUN="$ROOT/$CELL"; LOG="$ROOT/$CELL.log"
  if ! ls "$RUN"/dr-*/report.md >/dev/null 2>&1; then
    echo "--- $CELL flight $(date -Is) ---"
    ready || { echo "    daemon not ready — stopping"; break; }
    rm -rf "$RUN"; mkdir -p "$RUN"
    env "${ENVS[@]}" SOVEREIGN_DR_COMPOSED_REPORT=1 \
      RUST_LOG=deep_research=debug,warn \
      $CLI deep-research "$Q" --run-dir "$RUN" --max-rounds 2 --search 40 --fetch 100 \
      --search-source corpus --corpora "$EST" > "$LOG" 2>&1
    rp=$(ls "$RUN"/dr-*/report.md 2>/dev/null | head -1)
    [ -n "$rp" ] && echo "    $CELL flew — $(wc -w < "$rp") words" || { echo "    $CELL FLIGHT FAILED"; tail -3 "$LOG"; continue; }
  fi
  # Witness the SECTION EVIDENCE BUDGET from the run's own trace. The flag is
  # a number the writer never announces in its output, so unlike the
  # architecture lever it has no textual mark — compose_report logs the
  # decided (want, cap) at info and this reads it back. A cell whose log says
  # wide=false flew the control (ARCH 18.1).
  if [ "$WANT_WIDE" = "1" ]; then
    if decolor "$LOG" | grep -q 'section evidence budget decided'; then
      decolor "$LOG" | grep -m1 -o 'passages_per_section=[0-9]* per_source_cap=[0-9]*[^"]*wide=[a-z]*' \
        | sed 's/^/      WITNESS: /'
      if ! decolor "$LOG" | grep -q 'wide=true'; then
        echo "    $CELL SECTION-EVIDENCE NOT WITNESSED — the budget stayed narrow."
        echo "    REFUSING to fly further cells (ARCH 18.1)."
        WITNESS_OK=no; break
      fi
    else
      echo "    $CELL SECTION-EVIDENCE NOT WITNESSED — the budget event never emitted."
      WITNESS_OK=no; break
    fi
  fi
  # Witness the ARCHITECTURE before the routing when that is the arm's lever:
  # it reads the artifact, costs nothing, and refuses the same way.
  if [ "$WANT_ARCH" = "1" ]; then
    rp=$(ls "$RUN"/dr-*/report.md 2>/dev/null | head -1)
    if [ -z "$rp" ] || ! architecture_witness "$rp" "$Q"; then
      echo "    $CELL ARCHITECTURE NOT WITNESSED — the report kept the control's"
      echo "    shape. REFUSING to fly further cells: an arm that cannot prove"
      echo "    its own lever changed anything is not a measurement (ARCH 18.1)."
      WITNESS_OK=no; break
    fi
  fi
  # Witness the routing BEFORE spending a judge call on the article.
  if routing_witness "$LOG" "$WANT"; then
    WITNESS_OK=yes
  else
    WITNESS_OK=no
    echo "    $CELL ROUTING NOT WITNESSED — expected Section speed=$WANT."
    echo "    REFUSING to fly further cells: an arm that cannot prove its own"
    echo "    lever changed anything is not a measurement (ARCH 18.1)."
    break
  fi
  # EVIDENCE REACH — free, deterministic, collected on every cell whether or
  # not it is judged. Two numbers the 7-point score band cannot swallow:
  # how much of the acquired window the deliverable cites, and how many
  # citation handles it INVENTED (measured 2026-08-26: 2 of 13 runs cite ev-N
  # strictly beyond their own window). Reach does NOT predict score
  # (corr -0.30, n=7) and must never be reported as quality — but a fabricated
  # handle is a grounding defect at n=1, which is exactly what an underpowered
  # arm needs. Never fails a cell: a reporting line, not a gate.
  rd=$(ls -d "$RUN"/dr-* 2>/dev/null | head -1)
  [ -n "$rd" ] && python3 research/deep-research/arms/lab/evidence_utilisation.py --oneline "$rd" \
      2>/dev/null | tail -1 | sed "s/^/      REACH: /"

  rec="$FLIGHTS/$CELL.record.json"; scored "$rec" && { echo "    $CELL already scored"; continue; }
  art=$(ls "$RUN"/dr-*/report.md 2>/dev/null | head -1); [ -z "$art" ] && continue
  ART_ABS=$(readlink -f "$art")
  ART_CHARS=$(wc -c < "$ART_ABS" 2>/dev/null || echo 0)
  for attempt in 1 2; do
    settle "$ART_CHARS" || break
    ( cd research/deep-research/drb/overall-derivation && \
      LLM_BACKEND=openai OPENAI_BASE_URL=http://127.0.0.1:9741/v1 OPENAI_API_KEY=local \
      python3 ../../arms/lab/score_one.py --task "$TASK" --article "$ART_ABS" \
        --save-judge "flights-ceiling/$ARM/$CELL.judge.jsonl" ) > "$rec" 2>&1 || true
    scored "$rec" && { echo "    $CELL scored $(grep -o '\"overall_score\": [0-9.]*' "$rec" | head -1)"; break; }
    if grep -q "missing dims\|empty dims" "$rec"; then
      echo "    $CELL COULD-NOT-JUDGE (judge omitted a dimension — deterministic under the greedy pin, no retry)"; break
    fi
    echo "    $CELL judge attempt $attempt failed (transport) — retrying"
  done
  scored "$rec" || echo "    $CELL UNSCORED — reported as missing, never as zero"
done

echo ""; echo "=== CEILING ARM '$ARM' RESULT $(date -Is)  routing-witnessed: $WITNESS_OK ==="
ARM="$ARM" FLIGHTS="$FLIGHTS" ROOT="$ROOT" TASK="$TASK" WITNESS="$WITNESS_OK" python3 - <<'PY'
import json,os,re,glob,statistics as st
A=os.environ['ARM']; F=os.environ['FLIGHTS']; R=os.environ['ROOT']; T=os.environ['TASK']
def rec(p):
    if not os.path.exists(p): return None
    t=open(p,errors='replace').read(); m=re.search(r'\{\s*"id".*?\}', t, re.S)
    try: return json.loads(m.group(0)) if m else None
    except Exception: return None
vals=[]; missing=[]
for p in sorted(glob.glob(f'{F}/t{T}-r*.record.json')):
    r=rec(p)
    (vals.append(100*r['overall_score']) if r else missing.append(os.path.basename(p)))
DIMS=["comprehensiveness","insight","instruction_following","readability"]
# THE BARS COME FROM ONE FILE (drb/bars.json), never from a literal here.
# Until 2026-08-26 this line carried AIQ_T69 = 54.81 while the pre-registration
# retired that bar — a bar two files disagree about is not a bar (ARCH 10.6).
# An absent bar is REPORTED ABSENT; it is never replaced by a stale default.
BARS=json.load(open('research/deep-research/drb/bars.json'))
_c=BARS['control']['task_69']['like_for_like']
CTRL_MEAN, CTRL_N = _c['mean'], _c['n']
CTRL_SD = BARS['control']['per_draw_sd']
CTRL_NAME = _c['name']
AIQ_T69 = BARS['aiq_on_our_ruler']['task_69']   # None until measured
print(f'arm cells scored: {len(vals)}   missing: {missing or "none"}')
if not vals:
    print('NO CELLS SCORED — this arm resolved nothing.'); raise SystemExit
print('  ' + ' '.join('%.2f'%v for v in sorted(vals)))
m=st.mean(vals); sd=st.stdev(vals) if len(vals)>1 else float("nan")
se=(sd/len(vals)**0.5) if len(vals)>1 else float("nan")
print(f'  n={len(vals)}  mean {m:.2f}  spread {(max(vals)-min(vals)) if len(vals)>1 else 0:.2f}  sd {sd:.2f}  se {se:.2f}')
print(f'\nCONTROL ({CTRL_NAME}): n={CTRL_N} mean {CTRL_MEAN:.2f} sd {CTRL_SD:.2f} (per-draw)')
delta=m-CTRL_MEAN
pooled_se=(CTRL_SD**2/CTRL_N + (sd**2 if len(vals)>1 else CTRL_SD**2)/max(len(vals),1))**0.5
print(f'DELTA vs control: {delta:+.2f}   pooled 2se ~ {2*pooled_se:.2f}')
if abs(delta) <= 2*pooled_se:
    print('  NOT RESOLVED at this n. The arm does not distinguish itself from control.')
else:
    print('  RESOLVED: the delta exceeds twice the pooled standard error.')
if AIQ_T69 is None:
    print('AIQ task-69 bar on our ruler: NOT MEASURED — no gap reported.')
    print('  (bars.json aiq_on_our_ruler.status is "pending"; the retired 54.81')
    print('   estimate sat ABOVE the ruler ceiling and is not a substitute.)')
else:
    print(f'AIQ task-69 bar (our ruler, MEASURED): {AIQ_T69:.2f}   arm mean is {m-AIQ_T69:+.2f} against it')
    gap = AIQ_T69 - CTRL_MEAN
    if abs(gap) > 1e-9:
        print(f'  gap closed: {100*delta/gap:.0f}% of the {gap:.2f}-point control-to-AIQ gap')
print(f'\nrouting witnessed: {os.environ["WITNESS"]}  <- if not "yes", the numbers above measure NOTHING')
json.dump({'arm':A,'task':int(T),'values':vals,'n':len(vals),'mean':m,'sd':sd,'se':se,
           'missing':missing,'control_mean':CTRL_MEAN,'control_sd':CTRL_SD,'control_n':CTRL_N,
           'delta':delta,'pooled_2se':2*pooled_se,'aiq_t69_our_ruler':AIQ_T69,
           'control_name':CTRL_NAME,
           'routing_witnessed':os.environ['WITNESS']},
          open(f'{F}/summary.json','w'), indent=2)
print(f'  summary -> {F}/summary.json')
PY
echo "=== CEILING ARM '$ARM' DONE $(date -Is) ==="
