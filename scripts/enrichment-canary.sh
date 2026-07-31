#!/usr/bin/env bash
#
# enrichment-canary.sh — P0.1 acceptance test: prove the HARD enrichment
# lane CAN fail. (ENRICHMENT_ROADMAP_SIZING.md §2 P0.1 — "the self-test
# the lane never had".)
#
# Two modes:
#   --control   No perturbation. Forces the resolve step to re-run and
#               expects the lane to stay GREEN — calibrates that a
#               clean forced rebuild doesn't red on its own (model /
#               nondeterminism drift). Run this FIRST.
#   (default)   Perturbs the resolver's rule-2 merge gate — BOTH
#               constants: ENTITY_MERGE_LEVENSHTEIN 2 → 100 (syntactic
#               pre-gate wide open) and ENTITY_MERGE_COSINE 0.85 → 0.35
#               (semantic bar under same-book description similarity).
#               Result: entities with description embeddings collapse.
#               Forces resolve, expects the lane RED with
#               status=regressed.
#               Why both: the cosine alone is DEAD on this corpus — the
#               Levenshtein ≤ 2 pre-gate empties rule 2's candidate set
#               before the cosine is consulted (measured 2026-07-31:
#               cosine 0.85→0.35 produced a byte-identical atlas).
#
# Mechanics (both modes):
#   1. Builds the CLI into a SCRATCH target dir (target/canary) so the
#      deployed debug build is never touched.
#   2. Backs up the corpus atlas, then deletes atlas/atoms.json — the
#      enrich-build pipeline caches every step; without this the
#      resolve step (where ENTITY_MERGE_COSINE lives) never re-runs
#      (first canary attempt failed exactly this way, 2026-07-31).
#   3. Runs `bench all --filter literary/bk-book-1 --rebuild` with the
#      scratch binary: re-resolves from cached cluster/name outputs,
#      re-scores, diffs vs the COMMITTED baseline.
#   4. Restores the source file and the atlas backup, whatever happens.
#
# Verdict (perturbed mode):
#   CANARY PASS — lane exited non-zero with `regressed`: the gate
#                 detects a real extraction regression.
#   CANARY FAIL — lane green despite the perturbation. This is the T1
#                 plan's go/no-go — STOP and re-plan P0 before P1.
#
# Cost: scratch debug build (cold: ~5min; warm: seconds) + resolve/
# configure phases on the daemon's primary slot (minutes, not hours —
# the expensive extract/cluster/name steps stay cached).
#
# Usage: scripts/enrichment-canary.sh [--control] [--perturb <cosine>]

set -uo pipefail

CORPUS_ID="brothers_karamazov"
BENCH_FILTER="literary/bk-book-1"
BASELINE="sovereign/bench/literary/baselines/bk-book-1/latest.json"
RESOLUTION_RS="corpus-engine/src/enrichment/atlas/resolution.rs"
CANARY_TARGET="target/canary"
ORIGINAL="0.85"
PERTURBED="0.35"
LEV_ORIGINAL="2"
LEV_PERTURBED="100"
CONTROL=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --control) CONTROL="1"; shift ;;
    --perturb) PERTURBED="$2"; shift 2 ;;
    -h|--help) sed -n '2,40p' "$0"; exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done

MODE=$([[ -n "$CONTROL" ]] && echo control || echo perturbed)
REPORT="$CANARY_TARGET/canary-report-$MODE.json"

# ── Preflight ───────────────────────────────────────────────────────────────
[[ -f "$RESOLUTION_RS" ]] || { echo "FATAL: run from the repo root ($RESOLUTION_RS not found)"; exit 2; }

# Root resolution mirrors sovereign_cli_shared::dirs — prefer ~/.svrnmesh,
# fall back to legacy ~/.sovereign. (First canary attempt backed up the
# stale legacy copy while the pipeline wrote the svrnmesh one.)
if [[ -d "$HOME/.svrnmesh/indexes/$CORPUS_ID" ]]; then
  SVRN_ROOT="$HOME/.svrnmesh"
elif [[ -d "$HOME/.sovereign/indexes/$CORPUS_ID" ]]; then
  SVRN_ROOT="$HOME/.sovereign"
else
  echo "FATAL: corpus $CORPUS_ID not found under ~/.svrnmesh or ~/.sovereign indexes"; exit 2
fi
ATLAS_DIR="$SVRN_ROOT/indexes/$CORPUS_ID/atlas"
[[ -f "$ATLAS_DIR/atoms.json" ]] || { echo "FATAL: no atoms.json at $ATLAS_DIR"; exit 2; }

if ! grep -q "pub const ENTITY_MERGE_COSINE: f32 = ${ORIGINAL};" "$RESOLUTION_RS" \
   || ! grep -q "pub const ENTITY_MERGE_LEVENSHTEIN: usize = ${LEV_ORIGINAL};" "$RESOLUTION_RS"; then
  echo "FATAL: expected ENTITY_MERGE_COSINE = ${ORIGINAL} and ENTITY_MERGE_LEVENSHTEIN = ${LEV_ORIGINAL} in $RESOLUTION_RS."
  echo "       A constant moved or changed — update this script's ORIGINAL values before running."
  exit 2
fi

if ! git diff --quiet -- "$RESOLUTION_RS"; then
  echo "FATAL: $RESOLUTION_RS has uncommitted changes — the canary restores it via git checkout."
  exit 2
fi

[[ -f "$BASELINE" ]] || { echo "FATAL: no committed baseline at $BASELINE — the canary diffs against it."; exit 2; }

# Daemon liveness (:9741 has NO /healthz) + the enrichment config's chat
# model must actually be advertised — a dead concrete pin 503s phase 8
# (hit live 2026-07-31; the durable fix is a slot alias like "primary").
if ! curl -sf --max-time 5 http://localhost:9741/status >/dev/null; then
  echo "FATAL: daemon not responding at :9741/status — the rebuild needs the primary slot."
  exit 2
fi
CHAT_MODEL="$(python3 -c "import json;print(json.load(open('$SVRN_ROOT/enrichment/$CORPUS_ID/config.json'))['chat_model'])" 2>/dev/null || true)"
if [[ -n "$CHAT_MODEL" ]] && ! curl -sf --max-time 5 http://localhost:9741/v1/models | grep -q "\"$CHAT_MODEL\""; then
  echo "FATAL: enrichment config pins chat_model '$CHAT_MODEL' but /v1/models does not advertise it."
  echo "       Fix $SVRN_ROOT/enrichment/$CORPUS_ID/config.json (use a slot alias: \"primary\")."
  exit 2
fi

# ── Backup + restore-on-exit ────────────────────────────────────────────────
ATLAS_BACKUP="$(mktemp -d)/atlas-backup"
cp -R "$ATLAS_DIR" "$ATLAS_BACKUP"
echo "atlas backed up → $ATLAS_BACKUP"

restore() {
  git checkout -- "$RESOLUTION_RS" 2>/dev/null || true
  if [[ -d "$ATLAS_BACKUP" ]]; then
    rm -rf "$ATLAS_DIR"
    cp -R "$ATLAS_BACKUP" "$ATLAS_DIR"
    echo "atlas restored from backup"
  fi
}
trap restore EXIT

# ── Perturb (unless control) + scratch build ────────────────────────────────
if [[ -z "$CONTROL" ]]; then
  sed -i.bak \
    -e "s/pub const ENTITY_MERGE_COSINE: f32 = ${ORIGINAL};/pub const ENTITY_MERGE_COSINE: f32 = ${PERTURBED};/" \
    -e "s/pub const ENTITY_MERGE_LEVENSHTEIN: usize = ${LEV_ORIGINAL};/pub const ENTITY_MERGE_LEVENSHTEIN: usize = ${LEV_PERTURBED};/" \
    "$RESOLUTION_RS"
  rm -f "$RESOLUTION_RS.bak"
  echo "perturbed ENTITY_MERGE_COSINE ${ORIGINAL} → ${PERTURBED}, ENTITY_MERGE_LEVENSHTEIN ${LEV_ORIGINAL} → ${LEV_PERTURBED}"
else
  echo "CONTROL mode — no perturbation; expecting the lane to stay green"
fi

echo "building CLI into $CANARY_TARGET (scratch — deployed debug build untouched) ..."
if ! CARGO_TARGET_DIR="$CANARY_TARGET" cargo build -p sovereign-cli-llm --bin sovereign-cli-llm; then
  echo "FATAL: scratch build failed"
  exit 2
fi

# Force the resolve step (and only it — extract/cluster/name caches are
# expensive LLM output and stay). resolve is where ENTITY_MERGE_COSINE
# fires; with atoms.json present the pipeline skips it entirely.
rm "$ATLAS_DIR/atoms.json"
echo "deleted $ATLAS_DIR/atoms.json — resolve will re-run"

# ── Run the lane ───────────────────────────────────────────────────────────
echo "running the HARD lane with --rebuild ($MODE mode) ..."
"$CANARY_TARGET/debug/sovereign-cli-llm" bench all \
  --bench-root sovereign/bench --filter "$BENCH_FILTER" --rebuild --report "$REPORT"
LANE_EXIT=$?

# ── Verdict ────────────────────────────────────────────────────────────────
echo
STATUSES="$(grep -o '"status": "[a-z_]*"' "$REPORT" 2>/dev/null | sort | uniq -c || true)"
if [[ -n "$CONTROL" ]]; then
  if [[ $LANE_EXIT -eq 0 ]]; then
    echo "CONTROL PASS — clean forced rebuild stayed green (exit 0). Canary verdicts are attributable."
    exit 0
  fi
  echo "CONTROL FAIL — clean forced rebuild exited $LANE_EXIT:"
  echo "$STATUSES"
  echo "The lane reds WITHOUT a perturbation — fix drift/nondeterminism (or re-mint the"
  echo "baseline) before running the perturbed canary, or its red proves nothing."
  exit 1
fi

if [[ $LANE_EXIT -ne 0 ]] && grep -q '"status": "regressed"' "$REPORT" 2>/dev/null; then
  echo "CANARY PASS — lane exited $LANE_EXIT with status=regressed."
  echo "The enrichment HARD gate detects a real extraction regression. (P0.1 acceptance met.)"
  exit 0
fi

echo "CANARY FAIL — lane exited $LANE_EXIT (expected non-zero + status=regressed)."
echo "$STATUSES"
echo "The gate CANNOT red on a perturbed resolver. Per the T1 plan this is the"
echo "go/no-go: STOP and re-plan P0 before spending on P1."
exit 1
