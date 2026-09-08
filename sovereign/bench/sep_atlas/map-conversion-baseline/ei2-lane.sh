#!/usr/bin/env bash
# EI2 SEP lane, same invocation as ei5c-lane-a2, at a given question-kind floor.
# usage: ei2-lane.sh <tag> <min_sim>
set -u
S=$(cd "$(dirname "$0")" && pwd); cd "$(git -C "$S" rev-parse --show-toplevel)"
tag=$S/ei2-$1; t0=$(date +%s)
SOVEREIGN_QUESTION_KIND_MIN_SIM=$2 RUST_LOG="warn,retrieval_audit=debug" \
  target/debug/sovereign-cli-llm eval run --bank sovereign/bench/sep/questions.toml \
  --prod-pipeline --isolate --limit 30 --format json --output "$tag.json" > "$tag.log" 2>&1
rc=$?
echo "WALL_$1: $(( $(date +%s) - t0 ))s rc=$rc floor=$2"
grep -q "prod-pipeline mode" "$tag.log" || echo "INSTRUMENT_FAIL_$1: not prod-pipeline mode"
python3 "$S/score.py" "$tag.json" "$1"
python3 "$S/yield.py" "$tag.log" "$1"
python3 - "$tag.log" "$1" <<'PYEOF'
import re, sys, collections
ansi = re.compile(r"\x1b\[[0-9;]*m")
txt = ansi.sub("", open(sys.argv[1], errors="replace").read())
tot = {}
for m in re.findall(r"summary_sources=([a-z:,0-9]+)", txt):
    for part in m.split(","):
        k, _, v = part.partition(":")
        if v.isdigit(): tot[k] = tot.get(k, 0) + int(v)
print(f"SOURCES_{sys.argv[2]}: " + (", ".join(f"{k}={v}" for k, v in sorted(tot.items())) or "NO LEDGER LINE FOUND"))
kinds = collections.Counter(re.findall(r"kind=([a-z]+)[^\n]{0,40}?kind_source=([a-z-]+)", txt))
print(f"KINDS_{sys.argv[2]}: " + (", ".join(f"{k}/{s}={n}" for (k,s),n in sorted(kinds.items())) or "no kind lines"))
PYEOF
