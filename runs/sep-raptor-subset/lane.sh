#!/usr/bin/env bash
# ei-7a SEP-subset lane, both arms, both directions (§18.5/§18.6).
#
# ARM OFF : corpus `raptor-subset-off` — no `Summary` atom in its atlas, so
#           the walk reaches none; the retiring injector supplies the rollups.
# ARM ON  : corpus `raptor-subset-on`  — byte-identical corpus that has had
#           `enrich summary-atoms` run over it, injector disabled.
#
# The two corpora are built as byte copies of each other, so the arms differ
# in exactly two switches and both are named on every row.
#
# MODE. Only `--prod-pipeline` reaches `apply_atlas_grounding`; the default
# raw-index mode does not, so an A/B run in it is one arm run twice. Every
# mode announces itself on stderr since ei-7a, and this script pins the flag
# and greps the announcement back out as a §18.4 instrument check.
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 1

OUT=runs/sep-raptor-subset/out
mkdir -p "$OUT"
CLI=./target/debug/sovereign-cli-llm

bank_for () { # corpus_id -> derived bank path
  local corpus=$1 out="$OUT/questions-$1.toml"
  # DERIVED from the control bank with only `corpus` swapped; a checked-in
  # copy would be a second decider for the question set.
  sed "s/^corpus = \"sep\"/corpus = \"$corpus\"/" sovereign/bench/sep/questions.toml > "$out"
  grep -q "^corpus = \"$corpus\"" "$out" || { echo "FAIL: bank corpus not swapped"; exit 1; }
  echo "$out"
}

run () { # arm run_ix corpus raptor_env
  local arm=$1 ix=$2 corpus=$3 raptor=$4
  local tag="$OUT/${arm}-run${ix}"
  local bank; bank=$(bank_for "$corpus")
  echo "=== arm=$arm run=$ix corpus=$corpus SOVEREIGN_RAPTOR_GROUNDING=$raptor ==="
  SOVEREIGN_RAPTOR_GROUNDING=$raptor \
    "$CLI" eval run --bank "$bank" --prod-pipeline --isolate \
      --format json --output "${tag}.json" > "${tag}.log" 2>&1
  echo "RC_${arm}_${ix}=$?"
  # The instrument, checked before the result: a run whose log does not say
  # `prod-pipeline mode` did not reach atlas grounding and its number means
  # nothing for this comparison.
  grep -q "prod-pipeline mode" "${tag}.log" \
    || echo "INSTRUMENT_FAIL_${arm}_${ix}: not prod-pipeline mode"
  tail -3 "${tag}.log"
}

# Both directions: OFF→ON, then ON→OFF. Two runs per arm, so a per-arm spread
# is visible and an ordering effect cannot hide inside it.
run off 1 raptor-subset-off 1
run on  1 raptor-subset-on  0
run on  2 raptor-subset-on  0
run off 2 raptor-subset-off 1
echo DONE
