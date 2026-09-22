#!/usr/bin/env bash
# The loop. One arm = one name + one env. Resumable: a finished arm is skipped.
# Everything here is retrieval-only (--prod-pipeline), so an arm costs seconds.
set -uo pipefail
cd "$(dirname "$0")"

BANK=${BANK:?path to the bank TOML}
GOLD=${GOLD:-gold.tsv}
OUT=${OUT:-runs}
RESULTS=$OUT/results.tsv

# ── THE ARM LIST. Freeze it before the first run. Anything discovered
# ── mid-loop goes in NEXT.md, not in here.
ARMS=(
  "base:"
  "wide:SOVEREIGN_KQ_POOL_SCALE=4"          # the ceiling: recall(arm)/recall(wide) = selection efficiency
  "decomp:SOVEREIGN_QUERY_DECOMP=1"
  "title:SOVEREIGN_TITLE_EXPAND=1"
  "atlas_off:SOVEREIGN_ATLAS_GROUNDING=0"
  "prefilter:SOVEREIGN_CORPUS_PREFILTER_TOPK=8"
)

mkdir -p "$OUT"
[ -f "$RESULTS" ] || printf 'arm\tcls\tn\trecall\tdensity\tchars\n' > "$RESULTS"

for spec in "${ARMS[@]}"; do
  arm=${spec%%:*}; env_kv=${spec#*:}
  json=$OUT/$arm.json
  if [ -s "$json" ]; then echo "skip $arm (on disk)"; else
    echo "run  $arm  ${env_kv:-<defaults>}"
    env ${env_kv:+$env_kv} svrn eval run --bank "$BANK" --prod-pipeline \
        --format json --output "$json" >/dev/null 2>"$OUT/$arm.err" \
      || { echo "  FAILED — see $OUT/$arm.err"; continue; }
  fi
  python3 score.py "$json" "$GOLD" "$arm" "$RESULTS"
done

echo; echo "history: $RESULTS"
column -t -s$'\t' "$RESULTS"
