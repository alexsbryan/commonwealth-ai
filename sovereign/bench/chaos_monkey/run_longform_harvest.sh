#!/usr/bin/env bash
# Validation harvest for the longform-negative dev banks.
#
# ONE instrumented `--gv-shadow` run over BOTH amended dev banks, serial —
# never parallel. The two banks share one daemon, one model residency and
# one GPU; running them concurrently would contend for slots and make every
# per-turn timing in the log meaningless. Serial is also what the two prior
# harvests did, which is what makes this one comparable to them.
#
# `--gv-shadow` records `violation_prob` on every row WITHOUT gating, so the
# harvest observes the incumbent ladder rather than steering it. That
# shadow-never-steers property is pinned by a unit test and was confirmed
# empirically at whole-system scale (two harvests on different binaries gave
# 36/37 and 20/20 byte-identical answers).
#
# WHICH BINARY, AND WHY NOT THE DEPLOYED ONE. This calls the binary built in
# THIS worktree, not the main checkout's `target/debug`. The deployed binary
# was built 2026-08-08 13:20 and `routed_intent` landed at 17:51 (cd28e49f),
# so it emits no `routed_intent` at all — `strings` finds zero occurrences.
# The harvest report needs that field. Verified, not assumed; see the guard
# below, which refuses rather than silently harvesting a blind run.
set -euo pipefail

cd "$(dirname "$0")/../../.."   # repo root
BENCH=sovereign/bench/chaos_monkey
OUT=$BENCH/results
STAMP=${STAMP:-$(date +%Y%m%d)}
CLI=${CLI:-target/debug/sovereign-cli-llm}
LOG=$OUT/longform_negatives_${STAMP}.run.log

[ -x "$CLI" ] || { echo "no binary at $CLI — build it: cargo build -p sovereign-cli-llm" >&2; exit 2; }

# The whole point of the run is the routed_intent distribution. A binary
# without the field would produce a report whose central column is silently
# empty — absence reported, never defaulted (ARCH §18.3).
#
# The pattern is deliberately UNANCHORED. An anchored `^routed_intent$`
# was tried first and reported 0 for BOTH a stale binary and a fresh one —
# the literal is interned inside a longer string blob, so it never sits on
# a line of its own. It gave the right verdict on the stale binary by luck
# and would have refused the correct one. Measured on this host: unanchored
# finds 3 in the fresh binary and 0 in the deployed one, which is the split
# the guard needs.
if [ "$(strings "$CLI" | grep -c 'routed_intent')" -eq 0 ]; then
  echo "REFUSING: $CLI predates routed_intent (cd28e49f). Rebuild in this worktree." >&2
  exit 3
fi

# Liveness, before an hour is committed. The daemon has NO /healthz (404);
# /v1/models is the liveness surface.
curl -sf --max-time 10 http://localhost:9741/v1/models >/dev/null \
  || { echo "REFUSING: daemon not answering on :9741" >&2; exit 4; }

: >"$LOG"
echo "=== HARVEST START $(date -Iseconds) ===" | tee -a "$LOG"
echo "=== binary $CLI ($(date -r "$CLI" -Iseconds)) ===" | tee -a "$LOG"

rc_total=0
for BANK in saltgrass saltgrass_compound; do
  echo "=== $BANK START $(date -Iseconds) ===" | tee -a "$LOG"
  set +e
  "$CLI" bench chaos-monkey run \
    --bank "$BENCH/$BANK.toml" \
    --manifest "$BENCH/manifest.toml" \
    --gv-shadow \
    --out "$OUT/${BANK}_longneg_${STAMP}.jsonl" \
    --transcripts "$OUT/${BANK}_longneg_${STAMP}.transcripts.jsonl" 2>&1 | tee -a "$LOG"
  rc=${PIPESTATUS[0]}
  set -e
  echo "=== $BANK END $(date -Iseconds) exit=$rc ===" | tee -a "$LOG"
  # A non-zero exit here is a BENCH GATE verdict (competence/honesty), not a
  # harness failure, and it is expected: saltgrass_compound has zero absent
  # probes, so its honesty gate is a 0/0 NaN and it has exited 1 on every
  # harvest to date. The harvest's own success is judged by whether the
  # transcripts were written, which the report then reads.
  rc_total=$((rc_total + (rc != 0)))
done

echo "=== HARVEST COMPLETE $(date -Iseconds) banks_nonzero=$rc_total ===" | tee -a "$LOG"
