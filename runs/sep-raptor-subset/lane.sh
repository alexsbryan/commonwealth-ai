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
  # `retrieval_audit` is a CUSTOM tracing target: dark unless named in the
  # filter, however high the level. The walk ledger — seeds, summary_seeds,
  # summaries_appended — lives on it, and it is the yield this lane has to
  # report, so a run without this filter reports a delta it cannot explain.
  RUST_LOG="warn,retrieval_audit=debug" \
  SOVEREIGN_RAPTOR_GROUNDING=$raptor \
    "$CLI" eval run --bank "$bank" --prod-pipeline --isolate \
      --format json --output "${tag}.json" > "${tag}.log" 2>&1
  echo "RC_${arm}_${ix}=$?"
  # The instrument, checked before the result: a run whose log does not say
  # `prod-pipeline mode` did not reach atlas grounding and its number means
  # nothing for this comparison.
  grep -q "prod-pipeline mode" "${tag}.log" \
    || echo "INSTRUMENT_FAIL_${arm}_${ix}: not prod-pipeline mode"
  # Yield, per §18.3: the walk's own counters, not inferred from the score.
  # STRIP ANSI FIRST. `tracing` writes escapes BETWEEN the field name and the
  # `=`, so a naive `summary_seeds=[0-9]+` matches nothing and returns a clean,
  # plausible ZERO — indistinguishable from "the walk reached no Summary",
  # which is the exact finding this lane exists to measure. Cost me a false
  # result in round 1; the sed is the fix (§18.4, validate the instrument).
  python3 - "${tag}.log" "${arm}" "${ix}" <<'PYEOF'
import re, sys
ansi = re.compile(r"\x1b\[[0-9;]*m")
txt = ansi.sub("", open(sys.argv[1], errors="replace").read())
lines = [l for l in txt.splitlines() if "ground: walk ledger" in l]
def tot(f): return sum(int(m) for l in lines for m in re.findall(rf"\b{f}=(\d+)", l))
kinds = {}
for l in lines:
    k = re.search(r'kind="([a-z]+)"', l)
    if k: kinds[k.group(1)] = kinds.get(k.group(1), 0) + 1
print(f"YIELD_{sys.argv[2]}_{sys.argv[3]}: walks={len(lines)} seeds={tot('seeds')} "
      f"summary_seeds={tot('summary_seeds')} suppressed={tot('summary_expansions_suppressed')} "
      f"summaries_appended={tot('summaries_appended')} rows={kinds}")
PYEOF
  python3 - "$tag" <<'PYEOF'
import json, sys
try:
    d = json.load(open(sys.argv[1] + ".json"))
except Exception as e:
    print(f"SCORE_{sys.argv[1]}: unreadable ({e})"); raise SystemExit(0)
rs = d.get("results") or []
m = sum(len(r["source_score"].get("matched", [])) for r in rs)
e = sum(r["source_score"].get("total_expected", 0) for r in rs)
print(f"SCORE: sources {m}/{e} ({100*m/max(e,1):.1f}%) over {len(rs)} questions")
PYEOF
}

# Both directions: OFF→ON, then ON→OFF. Two runs per arm, so a per-arm spread
# is visible and an ordering effect cannot hide inside it.
run off 1 raptor-subset-off 1
run on  1 raptor-subset-on  0
run on  2 raptor-subset-on  0
run off 2 raptor-subset-off 1
echo DONE
