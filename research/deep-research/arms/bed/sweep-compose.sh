#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# sweep-compose.sh — sweep the WRITER's section-evidence budget against a
# frozen bed, then score every arm on the same ruler the flights use.
#
# The arms differ in ONE variable: (section_passages, per_source_cap). The
# evidence is identical by construction — one `compose-input.json`, replayed —
# so a delta between arms is the budget and nothing else. Sampling is pinned
# inside the harness for every arm.
#
# ONE ARM PER PROCESS, AND IT RESUMES. Measured 2026-08-27: the daemon was
# OOM-killed mid-sweep (anon-rss 58.7 GiB; this host's wall is ~55 GiB, note
# 0b7eb9f3) during the 28x5 arm, and because a single `cargo test` invocation
# carried all five arms, the three arms AFTER it each died instantly against
# the dead socket — one crash cost four arms. Now each arm is its own
# invocation, an arm whose markdown already exists is skipped, and the daemon
# is checked (and waited for) between arms. A crash costs ONE arm and the
# re-run picks up where it stopped.
#
# The cost of that robustness is honest: each invocation re-embeds the passage
# set (~2 min), so the per-arm wall-clock includes ranking overhead a single
# batched run would have paid once. It is identical across arms, so it does
# not bias the comparison — but it is not a production latency estimate.
#
# WHAT THIS IS NOT. The replay stops at the writer; the ~71-min AUDIT that
# follows in a real flight can still EDIT the report (surgical span repair).
# Every `arm-*.md` here is therefore a PRE-AUDIT DRAFT, and the winning budget
# owes a full end-to-end cell before it is called a result (§18.5).
#
# INVOKING THIS FROM THE HOST: PUT `env` INSIDE THE PREFIX, NOT OUTSIDE.
#     WRONG   ARMS=16:4 OUT=... toolbox run -c sovereign-vulkan ./sweep-compose.sh
#     RIGHT   toolbox run -c sovereign-vulkan env ARMS=16:4 OUT=... ./sweep-compose.sh
# `toolbox run` does not forward the caller's environment (the same trap
# documented below for EXTRA_ENV — it applies to THIS script's own knobs too,
# and hits first). The wrong form is silent: ARMS falls back to the full
# five-arm curve and OUT to rep$REP, so a run asking for one arm flies five
# into a directory it did not name. Measured 2026-08-27, caught only because
# the banner echoes the resolved arms — READ THE BANNER before walking away.
set -u

REPO=/home/alexbryan/dev/commonwealth-ai
cd "$REPO"
BED=${BED:-$REPO/research/deep-research/arms/bed-compose/compose-input.json}
ARMS=${ARMS:-8:3,16:4,28:5,44:6,60:8}
TASK=${TASK:-69}
REP=${REP:-1}
JUDGE=${JUDGE:-Qwen3.8-27B-UD-Q6_K_XL}
RESTART_BETWEEN=${RESTART_BETWEEN:-0}
# OVERRIDABLE, because arms flown under DIFFERENT CONFIGURATION must not land
# in the same directory as the curve. An arm is keyed by its `NxM` spec alone,
# so a 16:4 flown with SOVEREIGN_DR_REPORT_ARCHITECTURE=1 has the same filename
# as the 16:4 already on the curve — and the resume check ("already flown —
# skipping") then reports success while flying nothing. Observed 2026-08-27:
# exactly that, silently, because OUT was pinned to rep$REP.
OUT=${OUT:-$REPO/research/deep-research/arms/runs-compose/rep$REP}

[ -f "$BED" ] || { echo "REFUSED: no bed at $BED — run mint-compose-bed.sh first"; exit 2; }

daemon_up () { curl -sf --max-time 10 http://127.0.0.1:9741/v1/models >/dev/null 2>&1; }
judge_served () {
  curl -sf --max-time 10 http://127.0.0.1:9741/v1/models 2>/dev/null \
    | python3 -c "
import json,sys
try: ids={m['id'] for m in json.load(sys.stdin)['data']}
except Exception: sys.exit(1)
sys.exit(0 if '$JUDGE' in ids else 1)"
}
# systemd restarts the daemon on death, but a 27B reload is not instant; an arm
# fired into that window fails for a reason that has nothing to do with its
# budget. Wait for a REAL answer, not for the port to open.
wait_daemon () {
  for _ in $(seq 1 60); do daemon_up && return 0; sleep 10; done
  return 1
}

wait_daemon || { echo "REFUSED: daemon not answering on :9741"; exit 2; }
judge_served || { echo "REFUSED: judge $JUDGE is not served — arms would fly unscorable"; exit 2; }

FASTCTX=$(journalctl --user -u sovereign --since "-24h" --no-pager 2>/dev/null \
  | grep -oE 'slot="fast" n_ctx=[0-9]+' | tail -1 | grep -oE '[0-9]+$')
[ -n "${FASTCTX:-}" ] && echo "    fast slot n_ctx=$FASTCTX (the writer's real ceiling)"

RUNID=$(python3 -c "import json;print(json.load(open('$BED'))['run_id'])")
DIR="$OUT/$RUNID"
mkdir -p "$DIR"
python3 -c "
import json
d=json.load(open('$BED'))
print('    bed %s — %d chunks, %d sections, %d notes, baseline %dx%d'
      % (d['run_id'], len(d['window']['chunks']), len(d['sections']),
         len(d['notes']), d['section_passages'], d['per_source_cap']))"

echo "=== COMPOSE SWEEP rep$REP $(date -Is) ==="
echo "    arms $ARMS   task $TASK   judge $JUDGE"
[ -n "${EXTRA_ENV:-}" ] && echo "    EXTRA_ENV $EXTRA_ENV"
echo "    HEAD $(git rev-parse --short HEAD)"

PRE=""
[ -f /run/.containerenv ] || PRE="toolbox run -c sovereign-vulkan"

for spec in $(echo "$ARMS" | tr ',' ' '); do
  name=$(echo "$spec" | tr ':' 'x')
  if [ -s "$DIR/arm-$name.md" ]; then
    echo "--- $name already flown ($(wc -w < "$DIR/arm-$name.md") words) — skipping"
    continue
  fi
  if [ "$RESTART_BETWEEN" = "1" ] && [ -n "$(ls "$DIR"/arm-*.md 2>/dev/null)" ]; then
    echo "--- restarting the daemon before $name (memory reset)"
    sovereign daemon stop >/dev/null 2>&1
    sovereign daemon start >/dev/null 2>&1
  fi
  wait_daemon || { echo "REFUSED at $name: daemon never came back"; exit 4; }
  echo "--- $name $(date -Is)"
  # `toolbox run` does NOT forward the caller's environment into the container
  # (measured: `FOO=x toolbox run … sh -c 'echo $FOO'` prints nothing), and the
  # same trap is documented on the daemon's systemd drop-in 40-toolbox.conf.
  # `env` therefore runs INSIDE the container, after the prefix — which is also
  # correct when the prefix is empty. Set it BEFORE the prefix and every
  # COMPOSE_* reverts to the harness default: a sweep that believes it flew
  # `8:3` actually flies `8:3,28:5` into the wrong directory (§18.3).
  # EXTRA_ENV rides INSIDE the container with the rest (see the note above) —
  # unquoted so it word-splits into separate assignments. `compose_replay` sets
  # SOVEREIGN_DR_{PIN_SAMPLING,COMPOSED_REPORT} and the per-arm passage knobs
  # itself, and reads everything else from the process env, so a lever like
  # SOVEREIGN_DR_REPORT_ARCHITECTURE=1 passes straight through. Anything set
  # here is ALSO echoed on the banner below, because an arm flown under a
  # different configuration that is not recorded is not comparable to the ones
  # already on the curve.
  # shellcheck disable=SC2086
  $PRE env ${EXTRA_ENV:-} COMPOSE_INPUT="$BED" COMPOSE_ARMS="$spec" COMPOSE_OUT="$OUT" \
    cargo test -p sovereign-core --test compose_replay -- --ignored --nocapture \
    2>&1 | tail -4
  # PROVE the environment crossed the container boundary rather than trusting
  # it did. If COMPOSE_ARMS is lost the harness falls back to its own default
  # ("8:3,28:5") and produces a well-formed report of a DIFFERENT experiment —
  # which, with per-arm flying, would silently refly an arm we already have and
  # never fly the one we asked for. (This guard existed, and I dropped it in
  # the per-arm rewrite; it is back because the failure it catches is silent.)
  python3 research/deep-research/arms/bed/assert_arms.py \
    "$DIR/compose-replay.json" "$spec" || exit 5
  # Accumulate the per-arm report. A single-arm invocation OVERWRITES
  # compose-replay.json with only its own row, so the merged file is the only
  # place the whole sweep's timings survive.
  python3 research/deep-research/arms/bed/merge_replay.py \
    "$DIR/compose-replay.json" "$DIR/arms-merged.json" || true
done

echo
exec research/deep-research/arms/bed/score-arms.sh "$DIR" "$TASK" "$JUDGE"
