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
#                                 [--rebuild]   (weekly tier: re-extract
#                                                enrichment atlases pre-score)
#
# Requires a healthy daemon (chat + embed slots). Build the CLI first:
#   cargo build -p sovereign-cli-llm --bin sovereign-cli-llm

set -uo pipefail

# ── Config ──────────────────────────────────────────────────────────────────
BIN="${SOVEREIGN_CLI:-target/debug/sovereign-cli-llm}"
# Total wall-clock ceiling. Sized for a local slow-slot model: the SOFT synth
# lanes drive full DeepQuery syntheses (~150-210s/question on the 35B-A3B), so
# sep(35 questions across 3 banks) + wikipedia synth alone is ~2.5-3h. The old
# 7200s (2h) dated from a faster-model era and starved the tail HARD lanes.
BUDGET_SECS=14400         # 4h ceiling
# Time (s) that SOFT/TRACKED lanes must LEAVE for the HARD lanes that run after
# them. Without this, a long synth lane consumes the whole remaining budget and
# the tail HARD gates (search-gym-gate, knowledge-gym-gate, agent-coding-gate)
# all SKIP(budget) → HARD_FAIL — i.e. a SOFT lane fails the build. A SOFT lane
# capped by this reserve just TIMEOUTs (advisory, non-gating) instead. Covers
# the ~agent-coding (~15m) + the fast gym gates that trail the synth lanes.
HARD_RESERVE_SECS="${HARD_RESERVE_SECS:-1800}"
REPORT_DIR="target/ci-bench"
UPDATE_BASELINE=""
# --rebuild: the WEEKLY tier (P0.1). Re-extracts each enrichment corpus's
# atlas (`bench all --rebuild`) before scoring, so the HARD enrichment lane
# diffs a FRESH extraction against baseline instead of re-reading a static
# atoms.json forever. Without a periodic rebuild the lane can only ever red
# on golden/scorer edits — extraction regressions (prompt, resolver, model)
# are invisible. Costs tens of minutes per corpus and needs the daemon's
# primary slot; run it weekly (cadence partner of the Monday quality lanes
# in .github/workflows/weekly.yml, but on a workstation with a live daemon
# — GH runners have no models). Enrichment lanes only; retrieval indexes
# are daemon-owned and unaffected.
REBUILD=""
QUICK=""                  # --quick: smaller-n slices for a fast local pre-push (~15m)
# Lean-tier synth sample: under --quick, each synth bank is down-sampled to this
# many questions (stratified by category). 5 covers SEP's 6 archetypes at ~1/7th
# the cost of the full 35-question run. Env-overridable.
SYNTH_QUICK_SAMPLE="${SYNTH_QUICK_SAMPLE:-5}"
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
# FR-9 governance lanes (A: tension detector; B: Q&A over current law). The
# corpus is set up by `scripts/setup-governance-corpus.sh` (install + enrich +
# seed + resolve), which pins the `maple-house` id. Lanes are skipped when the
# enriched corpus isn't present so a box without it doesn't fail the suite.
GOV_CORPUS="${GOV_CORPUS:-maple-house}"
GOV_BANK="$BENCH_ROOT/governance/maple_house.toml"
GOV_MANIFEST="$BENCH_ROOT/governance/manifest.toml"
GOV_INDEX="${HOME}/.sovereign/indexes/${GOV_CORPUS}"
MF_MODELS="${MF_MODELS:-primary}"
# Fidelity-Flywheel promote lane (Lane 7) — OPT-IN. A normal CI run never turns
# the loop; set FLYWHEEL_PARAM (e.g. "rerank.enabled=true") + FLYWHEEL_CORPUS to
# enable it, FLYWHEEL_MINE_PATH for Present probes, FLYWHEEL_ABSENT_BANK for the
# abstain path, and FLYWHEEL_APPLY=1 to auto-apply a passing RerankConfig win.
FLYWHEEL_PARAM="${FLYWHEEL_PARAM:-}"
FLYWHEEL_CORPUS="${FLYWHEEL_CORPUS:-$CHAOS_CORPUS}"
FLYWHEEL_MINE_PATH="${FLYWHEEL_MINE_PATH:-}"
FLYWHEEL_ABSENT_BANK="${FLYWHEEL_ABSENT_BANK:-$CHAOS_BANK}"

# ── Tool-use / agentic gyms (sample the hardest fixtures for unique signal) ──
# agent-bench is its own binary (separate crate); the two gyms are cli-llm
# subcommands. Each lane samples only the hardest fixtures — the leading-edge
# tool-call / agentic signal — to stay within the CI budget. agent-coding is the
# costly one (~10-15m), so it runs last where a budget squeeze skips it first.
AGENT_BIN="${SOVEREIGN_AGENT_BENCH:-target/debug/sovereign-agent-bench}"
SEARCH_GYM_FIXTURES="07_multicorpus_tangential_local 08_multicorpus_stale_local 09_multicorpus_topical_mismatch 10_multicorpus_contradicting_local"
KNOWLEDGE_GYM_FIXTURES="08_escalation_when_corpus_empty 10_cache_hit_on_repeat_query 11_multi_call_assembly"
# Agent-coding problems. The full run exercises three (lights-out in C + Python +
# a multi-file minilang). agent-coding is the single most expensive lane
# (~5min/problem), so under --quick we run just ONE — the Python lights-out
# problem, our trusted baseline. An explicit AGENT_PROBLEMS env always wins; the
# QUICK/FULL default is picked post-parse (once --quick is known).
AGENT_PROBLEMS_FULL="3.2-lights-out,3.2-lights-out-python,5.1-minilang-multifile-python"
AGENT_QUICK_PROBLEMS="${AGENT_QUICK_PROBLEMS:-3.2-lights-out-python}"
AGENT_PROBLEMS="${AGENT_PROBLEMS:-}"   # empty sentinel → resolved after arg-parse
# A single-problem quick run's grand_total/max_total fraction is NOT comparable
# to the 3-problem `ci` baseline, so --quick gates agent-coding against its OWN
# baseline id (passed via `bench gate --id`). First run auto-passes (first-run) +
# seeds it under --update-baseline; thereafter it's a real HARD regression gate on
# the Python-lights-out score.
AGENT_QUICK_BASELINE_ID="${AGENT_QUICK_BASELINE_ID:-ci-quick}"
# ── Multi-turn turn budget ───────────────────────────────────────────────────
# The --threads lane costs ~one chat call per TURN (~85s/turn on the 35B), so the
# uncapped 102-turn bank ate 8720s (2.4h) of the 14400s budget on the baseline run
# and starved the trailing HARD lanes. `--max-turns` bounds it by whole-thread
# packing from the front of the bank (the 21-turn marathon is last, so it's
# naturally excluded). cap=30 → threads 1–4 = 28 turns (~40min). Whole-thread, so
# the degradation curve stays honest per thread. Baselines are cap-specific: the
# multiturn baseline must be captured at the SAME cap it runs at (re-baseline on a
# cap change), which is why full and --quick use distinct caps + the quick lane is
# advisory. An explicit MULTITURN_MAX_TURNS env always wins.
MULTITURN_MAX_TURNS_FULL=30
MULTITURN_QUICK_TURNS="${MULTITURN_QUICK_TURNS:-8}"
MULTITURN_MAX_TURNS="${MULTITURN_MAX_TURNS:-}"   # empty sentinel → resolved post-parse
# agent-bench's built-in default --model is `commonwealth/coder`, which no node
# in the corrected stack advertises (→ every judge/agent call 503s → a floored
# 0/27 that hides regressions). Pin it to the primary slot the daemon actually
# serves; override with AGENT_MODEL for a dedicated coder model.
AGENT_MODEL="${AGENT_MODEL:-commonwealth/primary}"
# Agent runner. `search` is the built-in TDD red-green solver (commonwealth-tdd
# via SearchRunner) — the path the committed baseline was captured with. The
# bench's OWN default is `pi` (an external tool-calling agent) which scores far
# lower here (measured 3/27 vs search's 9/27 on the same 3 problems, 2026-06-22)
# and silently regresses any script-driven run that forgets to pass --agent.
# Pin search so the suite can't fall back to pi; override with AGENT_RUNNER=pi
# to A/B the external agent (see the daemon note on Lane 10).
AGENT_RUNNER="${AGENT_RUNNER:-search}"

# Core corpora the suite gates on (must be installed/queryable). Filters target
# specific benches that are installed + baselined on a standard dev box — NOT
# whole groups (e.g. `literary/bk-book-1`, not `literary`, since `dubliners-3`
# is an optional corpus that isn't installed everywhere).
RETRIEVAL_CORPORA=(sep wikipedia)
# `obsidian` is a personal vault, not present on most boxes — its bench filter
# matches nothing there, exiting non-zero → a spurious HARD FAIL(1). It's now
# OPT-IN via CI_BENCH_OBSIDIAN=1 (set it on the box that actually has the vault
# indexed). The default set covers only the portable, checked-in corpus.
#
# `literary/bk-book-1` needs the `brothers_karamazov` corpus. Install it with
#   svrn corpus install brothers_karamazov
# (237 KB prebuilt snapshot: 41 chunks + the reference atlas the committed
# baseline was minted from). WITHOUT IT THIS LANE MEASURES NOTHING AND STILL
# CLEARS THE GATE — `bench all` reports `1 stale`, and the status mapping below
# grades 0-regressed + stale as PASS(warn:setup). That was the state on every
# box but one until 2026-08-07; see sovereign/bench/literary/README.md.
ENRICHMENT_CORPORA=(literary/bk-book-1)
[[ -n "${CI_BENCH_OBSIDIAN:-}" ]] && ENRICHMENT_CORPORA=(obsidian "${ENRICHMENT_CORPORA[@]}")
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
    --rebuild) REBUILD="--rebuild"; shift ;;
    --quick) QUICK="1"; shift ;;
    --no-synth) NO_SYNTH="1"; shift ;;
    -h|--help) sed -n '2,40p' "$0"; exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done

# Lean tier: --quick down-samples the SOFT synth lanes (SYNTH_QUICK_SAMPLE) and
# tightens the ceiling so a pre-push run stays minutes, not hours. An explicit
# --budget-secs still wins — we only lower the *default* 14400 ceiling.
if [[ -n "$QUICK" && "$BUDGET_SECS" == "14400" ]]; then
  BUDGET_SECS=3600
fi

# Agent-coding problem set: an explicit AGENT_PROBLEMS env wins; otherwise --quick
# runs the single trusted baseline (Python lights-out) and the full run runs all
# three. Deferred to here so it can key on --quick.
if [[ -z "$AGENT_PROBLEMS" ]]; then
  if [[ -n "$QUICK" ]]; then AGENT_PROBLEMS="$AGENT_QUICK_PROBLEMS"; else AGENT_PROBLEMS="$AGENT_PROBLEMS_FULL"; fi
fi

# Multi-turn cap: explicit env wins; otherwise --quick uses the tiny quick cap and
# the full run uses the full cap. Deferred to here so it can key on --quick.
if [[ -z "$MULTITURN_MAX_TURNS" ]]; then
  if [[ -n "$QUICK" ]]; then MULTITURN_MAX_TURNS="$MULTITURN_QUICK_TURNS"; else MULTITURN_MAX_TURNS="$MULTITURN_MAX_TURNS_FULL"; fi
fi

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
  # A SOFT/TRACKED lane must not devour the budget the trailing HARD lanes need
  # (see HARD_RESERVE_SECS). Cap non-HARD lanes at budget_left − reserve so a
  # slow synth TIMEOUTs (advisory) instead of starving agent-coding-gate into a
  # build-failing SKIP. HARD lanes always get the full remaining budget.
  local lane_cap="$budget_left"
  if [[ "$kind" != "HARD" ]] && (( budget_left > HARD_RESERVE_SECS + 60 )); then
    lane_cap=$(( budget_left - HARD_RESERVE_SECS ))
  fi
  echo "── RUN   [$kind] $name   (budget left ${budget_left}s, lane cap ${lane_cap}s)"
  echo "         \$ $*"
  local t0; t0=$(date +%s)
  local out; out="$REPORT_DIR/lane-$(printf '%s' "$name" | tr '/: ' '___').out"
  # Per-lane cap if a timeout binary exists; else run uncapped (the inter-lane
  # budget guard + lane-internal bounds keep the run finite). Tee output so we
  # can distinguish a real regression from a setup gap.
  if [[ -n "$TIMEOUT_BIN" ]]; then
    "$TIMEOUT_BIN" "${lane_cap}s" "$@" 2>&1 | tee "$out"
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
  elif (( rc == 4 )); then
    # COULD-NOT-JUDGE, not FAILED — the lane ran and had nothing to verify.
    # Exit 4 means exactly this and nothing else in the bench family (both
    # sites are bench_cmd/faithfulness.rs: "no RAPTOR nodes" and "zero claims
    # judged — nothing verified is not a pass"). Distinguishing it matters
    # now that the faithfulness lane discovers every enriched corpus: a corpus
    # whose nodes are all sentinel-filtered would otherwise post a red FAIL(4)
    # every run and train everyone to ignore the lane.
    #
    # A HARD lane still fails on it, via the PASS* test below — for a gated
    # corpus, "suddenly nothing to judge" is a regression signal, not a pass.
    status="SKIP(no-data)"
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
    "$BIN" bench all --bench-root "$BENCH_ROOT" --filter "$c" $UPDATE_BASELINE $REBUILD \
      --report "$REPORT_DIR/enrichment-$c.json"
done
for c in "${RETRIEVAL_CORPORA[@]}"; do
  run_lane "retrieval:$c" HARD \
    "$BIN" bench all --bench-root "$BENCH_ROOT" --filter "$c" $UPDATE_BASELINE \
      --report "$REPORT_DIR/retrieval-$c.json"
done

# ── Lane 2b: retrieval through the PRODUCTION pipeline (HARD, deterministic) ──
# The bench-prod parity lane (sovereign/docs/RETRIEVAL_REDESIGN.md §7.1): each
# question drives the production KnowledgeQuery retrieval pipeline in-process
# (context build → kq_pipeline() → merge/truncate, NO synthesis) and the
# composed evidence pool is baseline-diffed. This is the lane that would have
# caught the 2026-07-16 finding — the pipeline delivering −12 facts vs the raw
# index on wiki multi-fact questions — which the raw lanes above are blind to.
# --isolate scopes each bank to its target corpus for cross-box determinism.
# Baselines at `baselines/<bench>-prod-isolated/`; first run passes + seeds
# under --update-baseline.
for c in "${RETRIEVAL_CORPORA[@]}"; do
  run_lane "retrieval-prod:$c" HARD \
    "$BIN" bench all --bench-root "$BENCH_ROOT" --filter "$c" --prod-pipeline --isolate \
      $UPDATE_BASELINE --report "$REPORT_DIR/retrieval-prod-$c.json"
done

# ── Lane 1: intent routing (HARD, deterministic, fast) ──
run_lane "routing" HARD \
  "$BIN" bench all --bench-root "$BENCH_ROOT" --routing-only --filter "$ROUTING_FILTER" \
    $UPDATE_BASELINE --report "$REPORT_DIR/routing.json"

# ── Lane 3: synthesis answer-equiv (SOFT — judge variance) ──
# Skippable with --no-synth: these are the slowest lanes and SOFT.
#
# LEAN TIER: under --quick, down-sample each synth bank to SYNTH_QUICK_SAMPLE
# questions (stratified by category — every archetype stays represented). A
# synthesis regression surfaces across categories, so a handful of questions
# retains the signal at a fraction of the wall time (SEP's 35-question synth
# ≈ 100 min on the 35B → a few min at N=5). The sampled run is ADVISORY — SOFT
# already, and at a reduced N it's not baseline-comparable, so it never gates.
# Full runs (no --quick) keep the whole bank for the baseline-tracked signal.
SYNTH_SAMPLE_ARGS=""
[[ -n "$QUICK" ]] && SYNTH_SAMPLE_ARGS="--sample-questions $SYNTH_QUICK_SAMPLE"
if [[ -z "$NO_SYNTH" ]]; then
  for c in "${RETRIEVAL_CORPORA[@]}"; do
    run_lane "synth:$c" SOFT \
      "$BIN" bench all --bench-root "$BENCH_ROOT" --synth --filter "$c" \
        $SYNTH_SAMPLE_ARGS --report "$REPORT_DIR/synth-$c.json"
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

# ── Lane 5c: faithfulness — RAPTOR summary claims vs member texts (T1 P0.3) ──
# Same TRACKED-run + HARD-gate pattern as chaos. The run judges every node
# summary's claims against the node's own member chunks (production
# extract/support registers); the gate fails ONLY on unsupported-claim-rate
# regression vs sovereign/bench/faithfulness/baselines/<corpus>/.
#
# EVERY ENRICHED CORPUS, not one (T1 A5 — P0.3's gate reads "reports a number
# for every enriched corpus in CI"; until 2026-08-03 this lane ran exactly one
# and the other half of the gate was unmet). The set is DISCOVERED from the
# state db — every distinct corpus_id with RAPTOR nodes — so enriching a corpus
# is what enrols it. Nothing to remember, nothing to edit here.
#
# FAITHFULNESS_CORPUS still pins the lane to a single corpus when set; that is
# the local-iteration escape hatch, not the CI path.
#
# A box with no RAPTOR tier at all skips the lane, as before. Per-corpus, a
# first run has no committed baseline and the gate PASSES by contract
# ("First-run (no baseline) passes" — bench gate --help), so discovering a new
# corpus reports its number without turning CI red on a baseline nobody has
# captured yet. Capture with --update-baseline.
#
# NEVER hardcode a personal / machine-local corpus here: its baseline id is
# meaningless off the box that has it. Discovery keeps that property — each box
# gates the corpora it actually has, and only committed baselines are shared.
faith_db=""
if command -v sqlite3 >/dev/null 2>&1; then
  for db in "$HOME/.svrnmesh/svrnmesh.db" "$HOME/.svrnmesh/sovereign.db" \
            "$HOME/.sovereign/svrnmesh.db" "$HOME/.sovereign/sovereign.db"; do
    [[ -f "$db" ]] || continue
    if [[ "$(sqlite3 "$db" "SELECT COUNT(*) FROM conv_raptor_nodes;" 2>/dev/null || echo 0)" -gt 0 ]]; then
      faith_db="$db"
      break
    fi
  done
fi
FAITHFULNESS_CORPORA=()
if [[ -n "${FAITHFULNESS_CORPUS:-}" ]]; then
  FAITHFULNESS_CORPORA=("$FAITHFULNESS_CORPUS")
elif [[ -n "$faith_db" ]]; then
  while IFS= read -r c; do
    [[ -n "$c" ]] && FAITHFULNESS_CORPORA+=("$c")
  done < <(sqlite3 "$faith_db" \
    "SELECT DISTINCT corpus_id FROM conv_raptor_nodes ORDER BY corpus_id;" 2>/dev/null || true)
fi
if [[ ${#FAITHFULNESS_CORPORA[@]} -eq 0 ]]; then
  echo "[skip] faithfulness: no corpus has a RAPTOR tier — build one with: svrn enrich raptor --corpus $CHAOS_CORPUS"
else
  echo "[info] faithfulness: ${#FAITHFULNESS_CORPORA[@]} enriched corpus/corpora — ${FAITHFULNESS_CORPORA[*]}"
  # REPORTING and GATING are deliberately separated.
  #
  # Every enriched corpus gets a NUMBER (TRACKED) — that is the P0.3 gate.
  # Only a corpus with a COMMITTED baseline gets the HARD regression gate, plus
  # the chaos corpus, which is portable by construction and always gated.
  #
  # Why not gate everything: discovery on a real workstation finds vaults,
  # watched folders and hash-suffixed local corpora that exist on exactly one
  # box. Under --update-baseline those would commit baselines nobody else can
  # reproduce, into a directory `svrn posture` counts. Enrolling a corpus into
  # the shared gate stays a deliberate human act: run the gate for it once with
  # --update-baseline and commit the result. Until then it reports and does not
  # vote.
  for fc in "${FAITHFULNESS_CORPORA[@]}"; do
    run_lane "faithfulness:$fc" TRACKED \
      "$BIN" bench faithfulness run --corpus "$fc" \
        --out "$REPORT_DIR/faithfulness-$fc.jsonl"
    if [[ -f "$BENCH_ROOT/faithfulness/baselines/$fc/latest.json" || "$fc" == "$CHAOS_CORPUS" ]]; then
      run_lane "faithfulness-gate:$fc" HARD \
        "$BIN" bench gate faithfulness \
          --report "$REPORT_DIR/faithfulness-$fc.jsonl" \
          --bench-root "$BENCH_ROOT" $UPDATE_BASELINE
    else
      echo "[info] faithfulness-gate:$fc — reported, not gated (no committed baseline). Enrol with: $BIN bench gate faithfulness --report $REPORT_DIR/faithfulness-$fc.jsonl --bench-root $BENCH_ROOT --update-baseline"
    fi
  done
fi

# ── Lane 5b: FR-9 governance — detector (Lane A) + Q&A (Lane B) ──
# Same TRACKED-run + HARD-gate pattern as chaos. Skipped when the enriched
# maple-house corpus isn't present (set up via scripts/setup-governance-corpus.sh)
# so a box without it doesn't fail the suite. Lane A (detector) is cheap — it
# reads the committed atlas and scores tension precision/recall, so it always
# runs. Lane B (Q&A) is a live model run, so it rides the --no-synth guard like
# the synth lanes; it drives the chaos two-red-line path over the governance
# corpus, where the active-set filter + GateSurface::Governance apply (the
# corpus carries an oplog), so SupersededTrap rows measure RL-3 (no dead law).
if [[ -f "$GOV_INDEX/atlas/atoms.json" ]]; then
  run_lane "governance-detector" TRACKED \
    "$BIN" bench governance run "$GOV_CORPUS" --split test \
      --out "$REPORT_DIR/governance-a.json"
  run_lane "governance-gate" HARD \
    "$BIN" bench gate governance --report "$REPORT_DIR/governance-a.json" \
      --bench-root "$BENCH_ROOT" $UPDATE_BASELINE
  if [[ -z "$NO_SYNTH" && -f "$GOV_BANK" ]]; then
    run_lane "governance-qa" TRACKED \
      "$BIN" bench governance qa "$GOV_CORPUS" --bank "$GOV_BANK" \
        --manifest "$GOV_MANIFEST" --out "$REPORT_DIR/governance-b.jsonl"
    run_lane "governance-qa-gate" HARD \
      "$BIN" bench gate governance-qa --report "$REPORT_DIR/governance-b.jsonl" \
        --bench-root "$BENCH_ROOT" $UPDATE_BASELINE
  else
    echo "── SKIP  [TRACKED] governance-qa (Lane B) — --no-synth or missing bank"
  fi
else
  echo "── SKIP  [governance] maple-house atlas not present at $GOV_INDEX — run scripts/setup-governance-corpus.sh"
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
  # --max-turns bounds the lane's intrinsic cost (≈one chat call per turn) so it
  # completes within budget and hands multiturn-gate a COMPLETE report — a timeout
  # here would leave a partial report and spuriously fail the HARD gate.
  run_lane "multiturn-degradation" TRACKED \
    "$BIN" eval run --threads --bank "$THREAD_BANK" \
      --max-turns "$MULTITURN_MAX_TURNS" \
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

# ── Lanes 8-10: tool-use / agentic gyms (TRACKED run + HARD baseline gate) ──
# Sample the hardest fixtures of each gym for the unique tool-CALLING / agentic
# signal the retrieval+synthesis lanes don't cover. Each gym RUN is advisory
# (its own pass rate is a true finding, not a regression); the paired HARD gate
# re-scores its artifact vs a committed baseline (first-run passes). The gyms
# print JSON to stdout, so we redirect into the report dir; agent-bench writes
# its --report file directly.

# Lane 8: search-gym — web-search judiciousness (search only when needed; cite
# from results). ~3-5m over the 4 hardest multicorpus fixtures × 5 replays.
SEARCH_FIX_ARGS=""
for f in $SEARCH_GYM_FIXTURES; do SEARCH_FIX_ARGS="$SEARCH_FIX_ARGS --fixture $f"; done
run_lane "search-gym" TRACKED \
  bash -c "'$BIN' search-gym run --json --replays 5 $SEARCH_FIX_ARGS > '$REPORT_DIR/search-gym.json'"
run_lane "search-gym-gate" HARD \
  "$BIN" bench gate search-gym --report "$REPORT_DIR/search-gym.json" \
    --bench-root "$BENCH_ROOT" $UPDATE_BASELINE

# Lane 9: knowledge-gym — knowledge_lookup discipline (corpus-vs-web escalation,
# citation faithfulness, multi-turn cache). ~1m over the 3 hardest fixtures.
KN_FIX_ARGS=""
for f in $KNOWLEDGE_GYM_FIXTURES; do KN_FIX_ARGS="$KN_FIX_ARGS --fixture $f"; done
run_lane "knowledge-gym" TRACKED \
  bash -c "'$BIN' knowledge-gym run --json $KN_FIX_ARGS > '$REPORT_DIR/knowledge-gym.json'"
run_lane "knowledge-gym-gate" HARD \
  "$BIN" bench gate knowledge-gym --report "$REPORT_DIR/knowledge-gym.json" \
    --bench-root "$BENCH_ROOT" $UPDATE_BASELINE

# Lane 10: agent-coding — end-to-end agentic code loop (plan→implement→test→
# iterate). The costly lane (~10-15m for the 3 hardest problems), so it's last:
# the run_lane budget guard skips it first under a squeeze, protecting the
# cheaper lanes. Separate binary; gated on grand_total/max_total.
#
# DAEMON CONFIG (2026-06-22): with AGENT_RUNNER=search (the default) this lane
# uses the commonwealth-tdd solver, which ORCHESTRATES its own red-green loop
# over the chat backend — it does NOT depend on the model emitting tool calls,
# so it needs NO SOVEREIGN_FORCE_TOOL_CALLS and runs inline on the SAME clean
# (force-off) daemon as every other lane. No separate daemon pass.
#   The force-tool-calls dance is ONLY for AGENT_RUNNER=pi (the external agent),
#   whose zero-tool-call exit otherwise floors it (~3/27; see
#   inference_adapter.rs:722). FORCE_TOOL_CALLS is DAEMON-GLOBAL and regresses
#   the gyms' "don't search unless needed" judiciousness, so if you A/B `pi`,
#   run it in its OWN pass apart from the gym/chaos lanes:
#     sovereign daemon stop && SOVEREIGN_FORCE_TOOL_CALLS=1 \
#       SOVEREIGN_DISABLE_AUTO_RESUME=1 sovereign daemon start
if [[ -x "$AGENT_BIN" ]]; then
  run_lane "agent-coding" TRACKED \
    "$AGENT_BIN" run --agent "$AGENT_RUNNER" --problems "$AGENT_PROBLEMS" --judge-trials 1 \
      --model "$AGENT_MODEL" --report "$REPORT_DIR/agent-coding.json"
  # --quick gates the single-problem run against its own baseline id (see the
  # AGENT_QUICK_BASELINE_ID note above); full runs use the default `ci` baseline.
  AGENT_GATE_ID_ARG=""
  [[ -n "$QUICK" ]] && AGENT_GATE_ID_ARG="--id $AGENT_QUICK_BASELINE_ID"
  run_lane "agent-coding-gate" HARD \
    "$BIN" bench gate agent-coding --report "$REPORT_DIR/agent-coding.json" \
      $AGENT_GATE_ID_ARG --bench-root "$BENCH_ROOT" $UPDATE_BASELINE
else
  echo "── SKIP  [HARD] agent-coding — binary not found at $AGENT_BIN (build: cargo build -p sovereign-agent-bench)"
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
