#!/usr/bin/env bash
#
# sovereign-ci-bench.sh — the ONE regression bench a developer runs to gain
# confidence the CORE of chat + inference has not regressed, in ≤ 2 hours.
#
# It does NOT reinvent measurement — it COMPOSES the existing benches, each a
# visible command (glassbox: you can see and re-run any single lane), and
# aggregates a single PASS/FAIL.
#
# ── Gate policy (decided 2026-06-06) ────────────────────────────────────────
#   HARD  (build-breaking): deterministic, baseline-diffed lanes — retrieval
#         recall, enrichment atom-F1, intent routing. These are reproducible;
#         a drop past the regression threshold fails the build.
#   SOFT  (reported, non-breaking): the synthesis answer-equiv judge lane.
#         LLM-judge variance shouldn't cause flaky red builds, so it's tracked
#         with a band, not gated.
#   TRACKED (run + reported, not yet gating): chaos-monkey (grounded
#         calibration) and mechanism-fidelity (reasoning-fidelity witness) and
#         the multi-turn degradation thread. These have *absolute* verdicts
#         (chaos is designed to break the current system; mechanism returns
#         NO-GO for any non-faithful model), so their absolute pass/fail must
#         NOT break CI. Once a baseline of their metrics is captured on a
#         healthy daemon, promote them to HARD *baseline-diff* gates (fail only
#         on regression vs that baseline) — see PROMOTE markers below.
#
# Overall exit: 0 iff every HARD lane passed AND the run stayed within budget.
#
# Usage:
#   scripts/sovereign-ci-bench.sh [--bin <path>] [--budget-secs N]
#                                 [--update-baseline] [--report <dir>] [--quick]
#
# Requires a healthy daemon (chat + embed slots). Build the CLI first:
#   cargo build -p sovereign-cli-llm --bin sovereign-cli-llm

set -uo pipefail

# ── Config ──────────────────────────────────────────────────────────────────
BIN="${SOVEREIGN_CLI:-target/debug/sovereign-cli-llm}"
BUDGET_SECS=7200          # 2h ceiling
REPORT_DIR="target/ci-bench"
UPDATE_BASELINE=""
QUICK=""                  # --quick: smaller-n slices for a fast local pre-push (~15m)
BENCH_ROOT="sovereign/bench"
MF_MANIFEST="$BENCH_ROOT/mechanism_fidelity/manifest.toml"
CHAOS_BANK="$BENCH_ROOT/chaos_monkey/secret_agent.toml"
CHAOS_MANIFEST="$BENCH_ROOT/chaos_monkey/manifest.toml"

# Core corpora the suite gates on (must be installed/queryable).
RETRIEVAL_CORPORA=(sep wikipedia)
ENRICHMENT_CORPORA=(obsidian literary)
ROUTING_FILTER="routing"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --bin) BIN="$2"; shift 2 ;;
    --budget-secs) BUDGET_SECS="$2"; shift 2 ;;
    --report) REPORT_DIR="$2"; shift 2 ;;
    --update-baseline) UPDATE_BASELINE="--update-baseline"; shift ;;
    --quick) QUICK="1"; shift ;;
    -h|--help) sed -n '2,40p' "$0"; exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done

mkdir -p "$REPORT_DIR"
START_TS=$(date +%s)
[[ -x "$BIN" ]] || { echo "FATAL: CLI not found/executable at $BIN (build it first)"; exit 2; }

# ── Lane bookkeeping ────────────────────────────────────────────────────────
declare -a LANE_NAMES LANE_KINDS LANE_STATUS LANE_SECS
HARD_FAIL=0

elapsed() { echo $(( $(date +%s) - START_TS )); }
remaining() { echo $(( BUDGET_SECS - $(elapsed) )); }

# run_lane <name> <HARD|SOFT|TRACKED> <cmd...>
# Gates on the command's exit code. HARD failures break the build; SOFT and
# TRACKED failures are recorded but do not.
run_lane() {
  local name="$1" kind="$2"; shift 2
  local budget_left; budget_left=$(remaining)
  if (( budget_left <= 60 )); then
    echo "── SKIP  [$kind] $name — out of time budget (${budget_left}s left)"
    LANE_NAMES+=("$name"); LANE_KINDS+=("$kind"); LANE_STATUS+=("SKIP(budget)"); LANE_SECS+=("0")
    [[ "$kind" == "HARD" ]] && HARD_FAIL=1
    return
  fi
  echo "── RUN   [$kind] $name   (budget left ${budget_left}s)"
  echo "         \$ $*"
  local t0; t0=$(date +%s)
  # Each lane gets at most the smaller of its share and the remaining budget.
  timeout "${budget_left}s" "$@"
  local rc=$?
  local secs=$(( $(date +%s) - t0 ))
  local status
  if (( rc == 0 )); then status="PASS"
  elif (( rc == 124 )); then status="TIMEOUT"
  else status="FAIL($rc)"; fi
  echo "── ${status}  [$kind] $name   (${secs}s)"
  LANE_NAMES+=("$name"); LANE_KINDS+=("$kind"); LANE_STATUS+=("$status"); LANE_SECS+=("$secs")
  if [[ "$kind" == "HARD" && "$status" != "PASS" ]]; then HARD_FAIL=1; fi
}

N_CASES_MF=$([[ -n "$QUICK" ]] && echo 16 || echo 30)

echo "================================================================"
echo " sovereign CI core-regression bench   budget=${BUDGET_SECS}s  bin=$BIN  quick=${QUICK:-0}"
echo "================================================================"

# ── Lane 0 + 2: enrichment atom-F1 + retrieval recall (HARD, deterministic) ──
# `bench all` discovers + baseline-diffs + exits 0/1. --filter scopes to one
# corpus at a time so a single un-indexed corpus can't void the whole lane.
for c in "${ENRICHMENT_CORPORA[@]}"; do
  run_lane "enrichment:$c" HARD \
    "$BIN" bench all --bench-root "$BENCH_ROOT" --filter "$c" $UPDATE_BASELINE \
      --report "$REPORT_DIR/enrichment-$c.json"
done
for c in "${RETRIEVAL_CORPORA[@]}"; do
  run_lane "retrieval:$c" HARD \
    "$BIN" bench all --bench-root "$BENCH_ROOT" --filter "$c" $UPDATE_BASELINE \
      --report "$REPORT_DIR/retrieval-$c.json"
done

# ── Lane 1: intent routing (HARD, deterministic, fast) ──
run_lane "routing" HARD \
  "$BIN" bench all --bench-root "$BENCH_ROOT" --routing-only --filter "$ROUTING_FILTER" \
    $UPDATE_BASELINE --report "$REPORT_DIR/routing.json"

# ── Lane 3: synthesis answer-equiv (SOFT — judge variance) ──
for c in "${RETRIEVAL_CORPORA[@]}"; do
  run_lane "synth:$c" SOFT \
    "$BIN" bench all --bench-root "$BENCH_ROOT" --synth --filter "$c" \
      --report "$REPORT_DIR/synth-$c.json"
done

# ── Lane 5: grounded calibration — chaos-monkey (TRACKED) ──
# PROMOTE to HARD once a baseline of {competence, honesty} is captured: the
# chaos bench currently exits 1 by design (the system hasn't grown into it).
if [[ -f "$CHAOS_BANK" ]]; then
  run_lane "chaos-monkey" TRACKED \
    "$BIN" bench chaos-monkey run --bank "$CHAOS_BANK" --manifest "$CHAOS_MANIFEST" \
      --out "$REPORT_DIR/chaos.jsonl"
fi

# ── Lane 6: reasoning-fidelity witness — mechanism-fidelity (TRACKED) ──
# The witness is "control d_agent == 0.000" (scoring join intact), NOT the
# GO/NO-GO verdict (NO-GO is a true finding for non-faithful models). PROMOTE
# to HARD by gating on the control witness once a baseline exists.
run_lane "mechanism-fidelity" TRACKED \
  "$BIN" bench mechanism-fidelity run --models primary --pool dev \
    --n-cases "$N_CASES_MF" --manifest "$MF_MANIFEST" \
    --out "$REPORT_DIR/mechanism.jsonl"

# ── Lane 4: multi-turn degradation — wikipedia_learn threads (TRACKED) ──
# eval --threads reports a degradation curve; gate it once a baseline of
# first_failure_turn / slope is captured.
THREAD_BANK=$(ls "$BENCH_ROOT"/wikipedia_learn/*.toml 2>/dev/null | head -1)
if [[ -n "${THREAD_BANK:-}" ]]; then
  run_lane "multiturn-degradation" TRACKED \
    "$BIN" eval run --threads --bank "$THREAD_BANK" \
      --out "$REPORT_DIR/threads.jsonl"
fi

# ── Verdict ─────────────────────────────────────────────────────────────────
TOTAL=$(elapsed)
echo
echo "================================================================"
echo " CI core-regression summary   (${TOTAL}s / ${BUDGET_SECS}s budget)"
echo "================================================================"
printf "  %-26s %-8s %-12s %s\n" "LANE" "KIND" "STATUS" "SECS"
for i in "${!LANE_NAMES[@]}"; do
  printf "  %-26s %-8s %-12s %ss\n" \
    "${LANE_NAMES[$i]}" "${LANE_KINDS[$i]}" "${LANE_STATUS[$i]}" "${LANE_SECS[$i]}"
done
echo "----------------------------------------------------------------"
if (( TOTAL > BUDGET_SECS )); then
  echo "  ⚠ exceeded the ${BUDGET_SECS}s budget"
fi
if (( HARD_FAIL == 0 )); then
  echo "  VERDICT: PASS ✓  — all HARD (deterministic) lanes within baseline."
  echo "  (SOFT/TRACKED lanes are advisory; review their numbers in $REPORT_DIR)"
  exit 0
else
  echo "  VERDICT: FAIL ✗  — a HARD lane regressed past threshold (or timed out)."
  echo "  Inspect the failing lane's report under $REPORT_DIR and re-run it alone."
  exit 1
fi
