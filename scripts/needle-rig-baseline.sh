#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# NEEDLE RIG — baseline. Order `mesh-scale-t2-needle-rig`, bar
# `t2-selection-quality`. Spec: MESH_SCALE_100_USERS_1000_CORPORA.md §8.6.
#
# ONE QUESTION: at n installed corpora, does today's production corpus
# selection find the ONE corpus that holds the answer — and where in the
# evidence pool does the answering chunk land?
#
# WHAT IT REUSES (ARCH §19):
#   - `scripts/probe-t1-expansion-fanout.sh` supplies the ENTIRE run
#     harness: sealed rootless netns, throwaway $HOME whose
#     `.svrnmesh/indexes` is the rig, a private daemon on :19741, the
#     bind assertion that proves the turn reached THIS daemon and not the
#     operator's, and the `eval run --prod-pipeline` invocation. This
#     script adds only the loop, the scorer call and the bracket. Nothing
#     from that harness is copied.
#   - `scripts/needle_rig.py score` is the scorer (mechanical, no judge).
#
# TWO RUNS MINIMUM. A single run is not a measurement (ARCH §18.5); the
# report prints per-run rates and the spread between them, and refuses to
# collapse them into one number.
#
# NEVER `--isolate`. Isolating the eval to the bank's corpus would hand the
# selection decision to the harness — the exact decision under test.
#
# Usage:
#   scripts/needle-rig-baseline.sh --rig-root DIR [--runs N] [--label TEXT]
#                                  [--prefilter K] [--limit N] [--set K=V]
#
# --rig-root is the `--out` of `scripts/needle-rig-build.sh`; it must
# contain `rig/`, `spec/bank.toml` and `spec/manifest.json`.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RIG_ROOT=""
RUNS=2
LABEL="baseline"
PREFILTER=""
# `eval run --limit N`. NOT the CLI default of 10 — at 10 the harness
# truncates the production evidence set before the scorer sees it, and a
# needle at rank 11 becomes indistinguishable from a needle that was never
# retrieved. The two are different defects (ordering vs selection) and the
# rig exists to tell them apart, so the default here is wide enough that the
# truncation a reader sees is the PIPELINE's, not this harness's.
LIMIT=200
declare -a EXTRA=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --rig-root)  RIG_ROOT="$2"; shift 2 ;;
    --runs)      RUNS="$2"; shift 2 ;;
    --label)     LABEL="$2"; shift 2 ;;
    --prefilter) PREFILTER="$2"; shift 2 ;;
    --limit)     LIMIT="$2"; shift 2 ;;
    --set)       EXTRA+=(--set "$2"); shift 2 ;;
    -h|--help)   sed -n '3,34p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "needle-rig-baseline: unknown flag: $1" >&2; exit 2 ;;
  esac
done

[[ -n "$RIG_ROOT" ]] || { echo "needle-rig-baseline: --rig-root is required" >&2; exit 2; }
BANK="$RIG_ROOT/spec/bank.toml"
MANIFEST="$RIG_ROOT/spec/manifest.json"
RIG="$RIG_ROOT/rig"
for f in "$BANK" "$MANIFEST"; do
  [[ -f "$f" ]] || { echo "needle-rig-baseline: missing $f — run needle-rig-build.sh first" >&2; exit 2; }
done
[[ -d "$RIG" ]] || { echo "needle-rig-baseline: missing $RIG" >&2; exit 2; }
(( RUNS >= 2 )) || { echo "needle-rig-baseline: --runs must be >= 2; one run is not a measurement" >&2; exit 2; }

N_CORPORA="$(find "$RIG" -maxdepth 1 -mindepth 1 \( -type d -o -type l \) | wc -l)"
N_NEEDLES="$(python3 -c "
import json,sys; print(json.load(open(sys.argv[1]))['count'])" "$MANIFEST")"
SEED="$(python3 -c "
import json,sys; print(json.load(open(sys.argv[1]))['seed'])" "$MANIFEST")"

RESULTS="$RIG_ROOT/results-$LABEL"
mkdir -p "$RESULTS"

echo "════════════════════════════════════════════════════════════════"
echo "needle-rig-baseline: label      $LABEL"
echo "needle-rig-baseline: rig        $RIG"
echo "needle-rig-baseline: corpora    $N_CORPORA ($N_NEEDLES needles, seed $SEED)"
echo "needle-rig-baseline: runs       $RUNS"
echo "needle-rig-baseline: prefilter  ${PREFILTER:-off (production default)}"
echo "needle-rig-baseline: eval limit $LIMIT (CLI default 10 would censor rank at 10)"
echo "════════════════════════════════════════════════════════════════"

PF=()
[[ -n "$PREFILTER" ]] && PF=(--prefilter "$PREFILTER")

for ((r = 1; r <= RUNS; r++)); do
  echo
  echo "──── run $r/$RUNS ────────────────────────────────────────────"
  RUN_JSON="$RESULTS/run-$r.json"
  rm -f "$RUN_JSON"
  T0=$(date +%s.%N)
  "$ROOT/scripts/probe-t1-expansion-fanout.sh" --rig "$RIG" \
    --eval-bank "$BANK" --eval-output "$RUN_JSON" --eval-limit "$LIMIT" \
    "${PF[@]}" "${EXTRA[@]}" \
    2>&1 | grep -E "^PROBE_T1|^probe-t1: (BIND CHECK|per-question|eval bank|corpora)|COULD-NOT-JUDGE" || true
  T1=$(date +%s.%N)
  if [[ ! -s "$RUN_JSON" ]]; then
    echo "needle-rig-baseline: run $r produced no run JSON — NEVER-RAN, not a zero" >&2
    exit 1
  fi
  WALL=$(python3 -c "print(f'{$T1-$T0:.1f}')")
  echo "NEEDLE_RIG_RUN run=$r label=$LABEL corpora=$N_CORPORA needles=$N_NEEDLES wall_s=$WALL"
  python3 "$ROOT/scripts/needle_rig.py" score --eval-json "$RUN_JSON" \
    --manifest "$MANIFEST" --out "$RESULTS/score-$r.json" \
    | sed "s/^/run$r /"
done

echo
echo "──── bracket across $RUNS runs ───────────────────────────────"
python3 - "$RESULTS" "$RUNS" "$N_CORPORA" "$N_NEEDLES" "$LABEL" <<'PY'
import json, sys, pathlib
res, runs, corpora, needles, label = (
    pathlib.Path(sys.argv[1]), int(sys.argv[2]), int(sys.argv[3]),
    int(sys.argv[4]), sys.argv[5])
scores = []
for r in range(1, runs + 1):
    p = res / f"score-{r}.json"
    if not p.exists():
        # §18.2: a missing arm is could-not-judge, never an implicit zero.
        print(f"NEEDLE_RIG_BRACKET COULD-NOT-JUDGE reason=missing_score run={r}")
        raise SystemExit(4)
    scores.append(json.loads(p.read_text()))

n = scores[0]["questions"]
def bracket(key):
    vals = [s[key] for s in scores]
    return min(vals), max(vals)

for key, name in (("corpus_hit", "needle_corpus_hit"),
                  ("chunk_hit", "needle_chunk_hit"),
                  ("hit_at_10", "needle_hit_at_10")):
    lo, hi = bracket(key)
    print(f"NEEDLE_RIG_BRACKET {name} [{lo}/{n}, {hi}/{n}] "
          f"[{100.0*lo/n:.1f}%, {100.0*hi/n:.1f}%]")

all_ranks = [s["ranks"] for s in scores]
meds = []
for rk in all_ranks:
    meds.append(rk[len(rk) // 2] if rk else None)
if any(m is None for m in meds):
    print(f"NEEDLE_RIG_BRACKET median_rank_of_hits n/a — at least one run had zero hits")
else:
    print(f"NEEDLE_RIG_BRACKET median_rank_of_hits [{min(meds)}, {max(meds)}]")

# Run-to-run stability of the SET of questions that hit, not just the count.
# Two runs at the same rate over different questions is a different (and
# worse) result than two runs over the same questions, and a rate alone
# cannot tell them apart.
sets = [{row["question_id"] for row in s["rows"] if row["chunk_hit"]} for s in scores]
inter = set.intersection(*sets) if sets else set()
union = set.union(*sets) if sets else set()
jac = (len(inter) / len(union)) if union else 1.0
print(f"NEEDLE_RIG_BRACKET hit_set_stability jaccard={jac:.3f} "
      f"stable={len(inter)} ever={len(union)}")
print(f"NEEDLE_RIG_BRACKET context label={label} corpora={corpora} needles={needles} runs={runs}")
PY
echo
echo "needle-rig-baseline: per-run JSON + scores under $RESULTS"
