#!/usr/bin/env bash
# The pick-a-card live leg (PRE-REG "What the study must emit"): ONE question,
# bare and full, as two CLI invocations side by side. A live result is never
# scored into a verdict — when it differs from the measured board, that line
# says so (printed at the end, unconditionally).
#
#   research/ontology-retrieval/demo/live-run.sh "<question>"
#
# Bare vs full env mirror arms.toml. Model identity is pinned explicitly
# (commonwealth/primary resolves to the board's Q6_K on the pod; on a local
# daemon it resolves to whatever "primary" is — the script prints it).
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../../.."
Q="${1:?usage: live-run.sh \"<question>\"}"
OUT=target/demo-live/$(date -u +%Y%m%dT%H%M%SZ)
mkdir -p "$OUT"

python3 - "$Q" "$OUT/bank.toml" <<'PY'
import sys
q, path = sys.argv[1], sys.argv[2]
open(path, 'w').write(
    '[bank]\nname = "live-single"\ncorpus = "ei7-ans"\n\n'
    '[[questions]]\nid = "live-1"\ncategory = "negative"\n'
    f'question = {q!r}\n'
)
PY

run() { # arm env-prefix...
  local arm="$1"; shift
  echo "== $arm  [$(date -u +%T)]"
  env "$@" sovereign eval run --bank "$OUT/bank.toml" --synth --isolate \
    --format json --output "$OUT/$arm.json" > "$OUT/$arm.stdout" 2> "$OUT/$arm.stderr"
}

run bare  SOVEREIGN_ATLAS_GROUNDING=0 SOVEREIGN_ATOM_ENUM=0 SOVEREIGN_ATOM_ENUM_OVERVIEW=0
run full  SOVEREIGN_ATLAS_GROUNDING=1 SOVEREIGN_ATOM_ENUM=1 SOVEREIGN_ATOM_ENUM_OVERVIEW=1

python3 - "$OUT" "$Q" <<'PY'
import json, sys, textwrap
out, q = sys.argv[1], sys.argv[2]
def row(arm):
    d = json.load(open(f"{out}/{arm}.json"))
    s = (d["results"][0].get("synth") or {})
    g = (s.get("gate") or {})
    return arm, (s.get("answer") or "(no answer)"), g
print(f"\nQUESTION: {q}\n" + "=" * 72)
for arm in ("bare", "full"):
    name, ans, g = row(arm)
    print(f"\n--- {name}  (gate: {g.get('action')}, retried: {g.get('retried')}, vp: {g.get('violation_prob')})")
    print(textwrap.fill(ans.strip(), 100))
print("\n" + "=" * 72)
print("LIVE RESULT — never scored into a verdict. The measured board is the record:")
print("  ANS K1: full 0.222 vs bare 0.385 (negative, decline-carried).")
print("  If this draw disagrees with the board, the disagreement is the finding.")
PY
echo "artifacts: $OUT"
