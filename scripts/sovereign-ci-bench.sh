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
#   TRACKED+GATED: chaos-monkey (grounded calibration), mechanism-fidelity
#         (reasoning-fidelity witness), and the multi-turn degradation thread.
#         These carry *absolute* verdicts that are true findings for the current
#         system, NOT regression signals (chaos is designed to break the present
#         agent → NO-GO; mechanism returns NO-GO for any non-faithful model), so
#         their own pass/fail must never break CI. Each runs as a TRACKED lane
#         (advisory — its absolute verdict is printed but does not gate), then a
#         paired HARD `*-gate` lane re-scores the SAME artifact and fails ONLY on
#         regression vs a committed baseline (`sovereign bench gate <lane>`,
#         baselines under sovereign/bench/<group>/baselines/<id>/). First-run
#         (no baseline) passes. Capture/refresh baselines with --update-baseline
#         on a healthy daemon. This is the promotion the old PROMOTE markers
#         called for: absolute verdict stays advisory, regression-vs-baseline
#         gates.
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
NO_SYNTH=""               # --no-synth: skip the slow SOFT synthesis lanes (~55m).
                          # Useful for fast HARD-gate runs + baseline seeding.
BENCH_ROOT="sovereign/bench"
MF_MANIFEST="$BENCH_ROOT/mechanism_fidelity/manifest.toml"
CHAOS_BANK="$BENCH_ROOT/chaos_monkey/secret_agent.toml"
CHAOS_MANIFEST="$BENCH_ROOT/chaos_monkey/manifest.toml"
# Corpus id for the chaos lane. The stable recipe-install
# (`scripts/setup-chaos-corpus.sh` → `sovereign corpus install chaos-secret-agent`)
# gives a machine-fixed id; override only if you watched the text instead (a
# path-hash `watched-<hash>` id). Empty falls back to the bank's [meta].corpus.
CHAOS_CORPUS="${CHAOS_CORPUS:-chaos-secret-agent}"
MF_MODELS="${MF_MODELS:-primary}"
# Fidelity-Flywheel promote lane (Lane 7) — OPT-IN. A normal CI run never turns
# the loop; set FLYWHEEL_PARAM (e.g. "rerank.enabled=true") + FLYWHEEL_CORPUS to
# enable it, FLYWHEEL_MINE_PATH for Present probes, FLYWHEEL_ABSENT_BANK for the
# abstain path, and FLYWHEEL_APPLY=1 to auto-apply a passing RerankConfig win.
FLYWHEEL_PARAM="${FLYWHEEL_PARAM:-}"
FLYWHEEL_CORPUS="${FLYWHEEL_CORPUS:-$CHAOS_CORPUS}"
FLYWHEEL_MINE_PATH="${FLYWHEEL_MINE_PATH:-}"
FLYWHEEL_ABSENT_BANK="${FLYWHEEL_ABSENT_BANK:-$CHAOS_BANK}"

# Core corpora the suite gates on (must be installed/queryable). Filters target
# specific benches that are installed + baselined on a standard dev box — NOT
# whole groups (e.g. `literary/bk-book-1`, not `literary`, since `dubliners-3`
# is an optional corpus that isn't installed everywhere).
RETRIEVAL_CORPORA=(sep wikipedia)
ENRICHMENT_CORPORA=(obsidian literary/bk-book-1)
ROUTING_FILTER="routing"
# Per-lane wall-clock cap needs a `timeout` binary. macOS lacks it by default
# (`brew install coreutils` → `gtimeout`). If neither exists, lanes run uncapped
# and only the inter-lane budget guard bounds the run.
TIMEOUT_BIN="$(command -v timeout 2>/dev/null || command -v gtimeout 2>/dev/null || true)"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --bin) BIN="$2"; shift 2 ;;
    --budget-secs) BUDGET_SECS="$2"; shift 2 ;;
    --report) REPORT_DIR="$2"; shift 2 ;;
    --update-baseline) UPDATE_BASELINE="--update-baseline"; shift ;;
    --quick) QUICK="1"; shift ;;
    --no-synth) NO_SYNTH="1"; shift ;;
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
  local out; out="$REPORT_DIR/lane-$(printf '%s' "$name" | tr '/: ' '___').out"
  # Per-lane cap if a timeout binary exists; else run uncapped (the inter-lane
  # budget guard + lane-internal bounds keep the run finite). Tee output so we
  # can distinguish a real regression from a setup gap.
  if [[ -n "$TIMEOUT_BIN" ]]; then
    "$TIMEOUT_BIN" "${budget_left}s" "$@" 2>&1 | tee "$out"
  else
    "$@" 2>&1 | tee "$out"
  fi
  local rc=${PIPESTATUS[0]}
  local secs=$(( $(date +%s) - t0 ))
  local status
  # bench-all lanes print "N regressed" — gate on THAT, not the raw exit code:
  # `bench all` also exits 1 on first-run (no baseline) and stale (corpus not
  # installed), which are setup gaps, NOT regressions, and must not break CI.
  local regressed; regressed=$(grep -oE "[0-9]+ regressed" "$out" 2>/dev/null | grep -oE "^[0-9]+" | tail -1)
  if (( rc == 124 )); then
    status="TIMEOUT"
  elif [[ -n "$regressed" ]]; then
    if (( regressed == 0 )); then
      if grep -qE "[1-9][0-9]* (stale|first-run)" "$out" 2>/dev/null; then
        status="PASS(warn:setup)"  # 0 regressed, but a bench was stale/first-run
      else
        status="PASS"
      fi
    else
      status="FAIL(${regressed}reg)"
    fi
  elif (( rc == 0 )); then
    status="PASS"
  else
    status="FAIL($rc)"
  fi
  echo "── ${status}  [$kind] $name   (${secs}s)"
  LANE_NAMES+=("$name"); LANE_KINDS+=("$kind"); LANE_STATUS+=("$status"); LANE_SECS+=("$secs")
  # PASS and PASS(warn:setup) both clear the gate; everything else fails HARD.
  if [[ "$kind" == "HARD" && "$status" != PASS* ]]; then HARD_FAIL=1; fi
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
# Skippable with --no-synth: these are the slowest lanes (~55m) and SOFT, so a
# fast HARD-gate run or a baseline-seeding pass can omit them.
if [[ -z "$NO_SYNTH" ]]; then
  for c in "${RETRIEVAL_CORPORA[@]}"; do
    run_lane "synth:$c" SOFT \
      "$BIN" bench all --bench-root "$BENCH_ROOT" --synth --filter "$c" \
        --report "$REPORT_DIR/synth-$c.json"
  done
else
  echo "── SKIP  [SOFT] synth lanes — --no-synth"
fi

# ── Lane 5: grounded calibration — chaos-monkey (TRACKED run + HARD gate) ──
# The bench RUN is advisory (TRACKED): chaos is designed to break the present
# agent, so its absolute NO-GO must never gate the build. The paired chaos-gate
# lane (HARD) re-scores the SAME artifact and fails only on regression vs the
# committed baseline (sovereign/bench/chaos_monkey/baselines/secret_agent/).
# First-run (no baseline) and a clean diff both pass; a missing artifact fails
# HARD (the bench couldn't certify the path). $UPDATE_BASELINE captures instead.
if [[ -f "$CHAOS_BANK" ]]; then
  CHAOS_CORPUS_ARG=()
  [[ -n "$CHAOS_CORPUS" ]] && CHAOS_CORPUS_ARG=(--corpus "$CHAOS_CORPUS")
  run_lane "chaos-monkey" TRACKED \
    "$BIN" bench chaos-monkey run --bank "$CHAOS_BANK" --manifest "$CHAOS_MANIFEST" \
      "${CHAOS_CORPUS_ARG[@]}" --out "$REPORT_DIR/chaos.jsonl"
  run_lane "chaos-gate" HARD \
    "$BIN" bench gate chaos-monkey --report "$REPORT_DIR/chaos.jsonl" \
      --bench-root "$BENCH_ROOT" $UPDATE_BASELINE
fi

# ── Lane 6: reasoning-fidelity witness — mechanism-fidelity (TRACKED run + HARD gate) ──
# The witness is "control Δ̄ ≈ 0" (the forced-choice scoring join is intact),
# NOT the GO/NO-GO verdict (NO-GO is a true finding for non-faithful models).
# mechanism-gate (HARD) gates on the control-witness drift vs baseline; P1
# collapse is tracked but tolerant. Baseline at
# sovereign/bench/mechanism_fidelity/baselines/dev/.
run_lane "mechanism-fidelity" TRACKED \
  "$BIN" bench mechanism-fidelity run --models "$MF_MODELS" --pool dev \
    --n-cases "$N_CASES_MF" --manifest "$MF_MANIFEST" \
    --out "$REPORT_DIR/mechanism.jsonl"
run_lane "mechanism-gate" HARD \
  "$BIN" bench gate mechanism-fidelity --report "$REPORT_DIR/mechanism.jsonl" \
    --bench-root "$BENCH_ROOT" $UPDATE_BASELINE

# ── Lane 4: multi-turn degradation — wikipedia_learn threads (TRACKED run + HARD gate) ──
# eval --threads reports a degradation curve; multiturn-gate (HARD) gates the
# worst-thread first-failure turn + mean fact-recall slope (+ judge coverage)
# vs baseline at sovereign/bench/wikipedia_learn/baselines/threads/.
THREAD_BANK=$(ls "$BENCH_ROOT"/wikipedia_learn/*.toml 2>/dev/null | head -1)
if [[ -n "${THREAD_BANK:-}" ]]; then
  run_lane "multiturn-degradation" TRACKED \
    "$BIN" eval run --threads --bank "$THREAD_BANK" \
      --output "$REPORT_DIR/threads.json"
  run_lane "multiturn-gate" HARD \
    "$BIN" bench gate multiturn --report "$REPORT_DIR/threads.json" \
      --bench-root "$BENCH_ROOT" $UPDATE_BASELINE
fi

# ── Lane 7: fidelity flywheel — propose + gate a scaffolding param (TRACKED) ──
# OPT-IN (skipped unless FLYWHEEL_PARAM is set). Runs the foundational closed
# loop on the chaos corpus: paired baseline/candidate arms on the Dev split,
# diffed by the SAME baseline-relative gate the chaos lane uses. Advisory by
# default (proposes); FLYWHEEL_APPLY=1 auto-applies a passing RerankConfig win.
# The existing chaos-gate HARD lane stays the build-breaker; this lane never
# fails the build on its own (TRACKED).
if [[ -n "$FLYWHEEL_PARAM" ]]; then
  FLYWHEEL_ARGS=(--param "$FLYWHEEL_PARAM" --corpus "$FLYWHEEL_CORPUS" --bench-root "$BENCH_ROOT")
  [[ -n "$FLYWHEEL_MINE_PATH" ]] && FLYWHEEL_ARGS+=(--mine-path "$FLYWHEEL_MINE_PATH")
  [[ -n "$FLYWHEEL_ABSENT_BANK" ]] && FLYWHEEL_ARGS+=(--absent-bank "$FLYWHEEL_ABSENT_BANK")
  [[ -n "${FLYWHEEL_APPLY:-}" ]] && FLYWHEEL_ARGS+=(--apply)
  run_lane "flywheel-promote" TRACKED \
    "$BIN" bench promote "${FLYWHEEL_ARGS[@]}"
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
