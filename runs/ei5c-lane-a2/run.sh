#!/usr/bin/env bash
# ei-5c-seed-race — lane (a), re-run, with RAPTOR composed as a source.
#
# The SAME invocation as the 2026-09-08 07:28 runs, deliberately: same bank,
# same `--limit 30`, same `--prod-pipeline --isolate`, same scorer, same yield
# parser. One thing differs — the thematic row now composes `[atoms, raptor]`
# instead of reaching only atoms — so the delta is attributable (§18.4).
#
# Bar: facts >= 151/159, the committed 2026-08-10 floor under this scorer. The
# same-stack 2026-09-05 pair read 153/159, and the injector-gone reading this
# replaces was 147/159 twice.
set -uo pipefail
WT=/home/alexbryan/dev/ei5c-wt
OUT="$WT/runs/ei5c-lane-a2/out"
CLI="$WT/target/debug/sovereign-cli-llm"
LANES="$WT/runs/ei5c-lanes"
mkdir -p "$OUT"
rm -f "$OUT"/*.rc "$OUT"/DONE
cd "$WT" || exit 1

finish() { echo "$(date -Is)" > "$OUT/DONE"; }
trap finish EXIT TERM INT

box() { { echo "== box $1 $(date -Is)"; free -g | sed -n '1,2p'; df -h /home | tail -1; } > "$OUT/box-$1.txt" 2>&1; }

leg() { local name; name="$1"; shift
  echo "== $name start $(date -Is)" >> "$OUT/log.txt"
  ( "$@" ) > "$OUT/$name.txt" 2>&1; local rc=$?
  echo "$rc" > "$OUT/$name.rc"
  echo "== $name rc=$rc $(date -Is)" >> "$OUT/log.txt"; return 0; }

box before
{ echo "tip:    $(git -C "$WT" rev-parse HEAD)"
  echo "dirty:  $(git -C "$WT" status --porcelain | wc -l) path(s)"
  echo "binary: $(stat -c %y "$CLI" 2>/dev/null | cut -c1-19)"
  echo "daemon: $(curl -s -m 5 -o /dev/null -w '%{http_code}' http://127.0.0.1:9741/v1/models)"
  echo "-- sep's summary artifacts, read before they are measured:"
  d="$HOME/.svrnmesh/indexes/sep"
  echo "   raptor table:   $([ -d "$d/raptor_summaries.lance" ] && echo present || echo ABSENT)"
  echo "   raptor sidecar: $([ -f "$d/raptor_summaries.meta.json" ] && echo present || echo ABSENT)"
  echo "   Summary atoms:  $(python3 -c "
import json
try:
    a=json.load(open('$d/atlas/atoms.json'))['atoms']
    print(sum(1 for x in a if x['atom_type']=='Summary'), 'of', len(a))
except Exception as e: print('unreadable:', e)")"
} > "$OUT/preflight.txt" 2>&1
[[ -x "$CLI" ]] || { echo 90 > "$OUT/preflight.rc"; exit 90; }
echo 0 > "$OUT/preflight.rc"

sep_run() {
  local ix; ix=$1
  local tag; tag="$OUT/a2-sep-run$ix"
  local t0; t0=$(date +%s)
  RUST_LOG="warn,retrieval_audit=debug" \
  "$CLI" eval run --bank sovereign/bench/sep/questions.toml \
    --prod-pipeline --isolate --limit 30 --format json --output "${tag}.json" \
    > "${tag}.log" 2>&1
  local rc=$?
  echo "WALL_a2_run$ix: $(( $(date +%s) - t0 ))s"
  grep -q "prod-pipeline mode" "${tag}.log" || echo "INSTRUMENT_FAIL_a2_$ix: not prod-pipeline mode"
  python3 "$LANES/score.py" "${tag}.json" "a2-sep-run$ix"
  python3 "$LANES/yield.py" "${tag}.log" "a2-sep-run$ix"
  # The composition's own line, ANSI stripped: which source served, per walk.
  python3 - "${tag}.log" "$ix" <<'PYEOF'
import re, sys
ansi = re.compile(r"\x1b\[[0-9;]*m")
txt = ansi.sub("", open(sys.argv[1], errors="replace").read())
tot = {}
for m in re.findall(r"summary_sources=([a-z:,0-9]+)", txt):
    for part in m.split(","):
        k, _, v = part.partition(":")
        if v.isdigit():
            tot[k] = tot.get(k, 0) + int(v)
print(f"SOURCES_a2_run{sys.argv[2]}: " + (", ".join(f"{k}={v}" for k, v in sorted(tot.items())) or "NO LEDGER LINE FOUND"))
PYEOF
  return $rc
}
leg a2-sep-run1 sep_run 1
leg a2-sep-run2 sep_run 2
box after
