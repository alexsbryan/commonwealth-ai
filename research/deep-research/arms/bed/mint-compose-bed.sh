#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# mint-compose-bed.sh — freeze ONE deep-research evidence window so
# `tests/compose_replay.rs` can sweep the writer's evidence budget without
# re-buying an acquisition per arm.
#
# WHY A SCRIPT AND NOT A FLIGHT. `compose-input.json` is written at the
# compose boundary (deep_research/mod.rs, `FREEZE THE WRITER'S INPUTS`),
# which is ~10 min in — BEFORE the ~12-min write and the ~71-min audit that
# follow it. Flying the whole ~96-min cell to collect a 10-min artifact buys
# nothing, so this starts the production flight and stops it the moment the
# artifact lands.
#
# WHY setsid + kill -PGID. `$CLI deep-research` spawns children; signalling
# the CLI's own pid leaves them running against the daemon and the next arm
# then measures a contended box. The flight therefore runs in its OWN process
# group and the whole group is signalled. (Same defect the engram harness hit
# 2026-08-27: it signalled EVAL_PID and the multi-GiB child survived.)
#
# WHY NOT REBUILD THE BED FROM evidence-window-<n>.json. On the 2026-08-26
# wide cell compose saw 61 chunks where the dumps summed to 57 — the merged
# window the writer composes from is strictly larger than what is persisted,
# so any reconstruction is a guess. That is what produced the retracted
# finding 80a442dc. (Until 2026-08-27 `ev-N` also RESTARTED each round, so
# the reconstruction collapsed round 2 onto round 1 on top of being short.
# The counter is run-scoped now; the reason above is the one that remains,
# and it is sufficient.)
#
# A run whose artifact never appears is NEVER-RAN, not an empty bed (§18.3).
set -u
cd /home/alexbryan/dev/commonwealth-ai

TASK=${TASK:-69}
OUT=${OUT:-research/deep-research/arms/bed-compose}
RUN=${RUN:-research/deep-research/arms/runs-bed-compose/t${TASK}-mint}
TIMEOUT_SECS=${TIMEOUT_SECS:-2400}     # 40 min: 4x the measured ~10-min boundary
CLI=./target/debug/sovereign-cli
BED=research/deep-research/arms/bed/bed.json
QUERIES=/home/alexbryan/dev/deep_research_bench/data/prompt_data/query.jsonl

EST=$(python3 -c "
import json
for t in json.load(open('$BED'))['tasks']:
    if int(t['id'])==$TASK: print(t['estate']); break")
[ -n "$EST" ] || { echo "REFUSED: task $TASK has no estate in $BED"; exit 2; }
Q=$(python3 -c "
import json
for l in open('$QUERIES'):
    r=json.loads(l)
    if int(r['id'])==$TASK: print(r['prompt']); break")
[ -n "$Q" ] || { echo "REFUSED: task $TASK has no prompt"; exit 2; }

# The estate is the whole experiment's ground: a mint against a missing or
# empty corpus produces a well-formed bed of nothing.
[ -d "$HOME/.svrnmesh/indexes/$EST" ] || { echo "REFUSED: estate $EST not on disk"; exit 2; }

curl -sf --max-time 10 http://127.0.0.1:9741/v1/models >/dev/null 2>&1 \
  || { echo "REFUSED: daemon not answering on :9741"; exit 2; }
PRIMARY=$(curl -sf --max-time 10 http://127.0.0.1:9741/v1/models \
  | python3 -c "import json,sys; d=json.load(sys.stdin); print(next((m['owned_by'].split('→')[-1] for m in d['data'] if m['id']=='primary'),'?'))")

# THE ROUND LEG DRAFTS ON THE *FAST* SLOT AT A ~96,000-CHAR EVIDENCE BUDGET
# (~21k tokens), and the budget decider knows nothing about that slot's
# context window — two deciders, no conversation (ARCH 10.6). When
# `[models].fast_context_size` is set below it the flight dies ~60s in with
# "Prompt too long", which names the token count and NOT the config key that
# caused it. 2026-08-27: the Flash-Next workstream set it to 16384 and cost a
# mint exactly this way. Assert the coupling here instead of rediscovering it.
FASTCTX=$(journalctl --user -u sovereign --since "-24h" --no-pager 2>/dev/null \
  | grep -oE 'slot="fast" n_ctx=[0-9]+' | tail -1 | grep -oE '[0-9]+$')
if [ -n "${FASTCTX:-}" ] && [ "$FASTCTX" -lt 24000 ]; then
  echo "REFUSED: fast slot n_ctx=$FASTCTX < 24000 — the Round drafting leg"
  echo "         sends ~21k tokens and will 503. Unset [models].fast_context_size"
  echo "         in ~/.sovereign/config.toml (it falls back to context_size) and"
  echo "         restart the daemon."
  exit 2
fi

rm -rf "$RUN"; mkdir -p "$RUN" "$OUT"
LOG="$RUN.log"

echo "=== COMPOSE BED MINT $(date -Is) ==="
echo "    task $TASK   estate $EST   primary $PRIMARY"
echo "    HEAD $(git rev-parse --short HEAD)"
echo "    stopping at the compose boundary; ceiling ${TIMEOUT_SECS}s"

setsid env SOVEREIGN_DR_PIN_SAMPLING=1 SOVEREIGN_DR_COMPOSED_REPORT=1 \
  RUST_LOG=deep_research=debug,warn \
  $CLI deep-research "$Q" --run-dir "$RUN" --max-rounds 2 --search 40 --fetch 100 \
  --search-source corpus --corpora "$EST" > "$LOG" 2>&1 &
CHILD=$!
PGID=$(ps -o pgid= -p "$CHILD" 2>/dev/null | tr -d ' ')
[ -n "$PGID" ] || { echo "REFUSED: could not resolve the flight's process group"; kill "$CHILD" 2>/dev/null; exit 2; }
echo "    flight pid $CHILD  pgid $PGID  log $LOG"

stop_flight () { kill -TERM "-$PGID" 2>/dev/null; sleep 5; kill -KILL "-$PGID" 2>/dev/null; }
trap 'echo "    interrupted — stopping the flight group"; stop_flight; exit 130' INT TERM

ART=""; t0=$(date +%s)
while :; do
  ART=$(ls "$RUN"/dr-*/compose-input.json 2>/dev/null | head -1)
  [ -n "$ART" ] && break
  if ! kill -0 "$CHILD" 2>/dev/null; then
    echo "    FLIGHT EXITED before the compose boundary — this minted NOTHING."
    tail -5 "$LOG" | sed 's/^/      /'
    exit 3
  fi
  el=$(( $(date +%s) - t0 ))
  if [ "$el" -ge "$TIMEOUT_SECS" ]; then
    echo "    CEILING ${TIMEOUT_SECS}s reached with no artifact — stopping, minted NOTHING."
    stop_flight; exit 4
  fi
  [ $(( el % 60 )) -lt 5 ] && echo "    ... ${el}s $(tail -1 "$LOG" 2>/dev/null | tail -c 100)"
  sleep 5
done

el=$(( $(date +%s) - t0 ))
echo "    artifact at ${el}s -> $ART"
# Let the write settle, then stop the flight: the writer would now spend ~12
# min composing and the audit ~71 more, and the bed is already frozen.
sleep 2
stop_flight
echo "    flight group $PGID stopped"

cp "$ART" "$OUT/compose-input.json"
python3 - "$OUT/compose-input.json" <<'PY'
import json,sys
d=json.load(open(sys.argv[1]))
print(f"    bed: run {d['run_id']}  {len(d['window']['chunks'])} chunks  "
      f"{len(d['sections'])} sections  {len(d['notes'])} notes  "
      f"baseline {d['section_passages']}x{d['per_source_cap']}")
PY
echo "=== MINTED -> $OUT/compose-input.json ==="
