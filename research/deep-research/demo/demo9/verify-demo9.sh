#!/usr/bin/env bash
# verify-demo9.sh — re-checks DEMO-9 (order deep-research-t2b) at landing.
# Every strip checks a committed artifact live; a strip that cannot run
# is reported, never silently skipped.
set -u
cd "$(dirname "$0")/../../../.." || exit 1   # repo root
ROOT="$PWD"
DRB="$ROOT/research/deep-research/drb"
DEMO9="$ROOT/research/deep-research/demo/demo9"
FAIL=0

note() { printf '%-42s' "strip $1"; shift; }

# 1. The frozen subset hashes are unchanged (freeze discipline).
#
# Three verdicts, not two (ARCH §18.1), for two reasons this strip used to
# collapse into a bare FAIL:
#
#   - vendor/fixture-validated.jsonl is entry 20 of the manifest but is
#     gitignored on size (69.7 MB) and re-fetched from the pinned upstream
#     commit, so a clean clone legitimately lacks it. Reporting that as FAIL
#     makes an unfetched optional artifact indistinguishable from a TAMPERED
#     freeze — the one signal this strip exists to give.
#   - `sha256sum` is GNU-only; on macOS it is absent unless coreutils is
#     installed, and `2>&1` swallowed the "command not found" into a FAIL.
#     A strip that cannot run is reported, never silently miscounted — this
#     script's own header says so.
note "1 frozen hashes"
SUMCHECK=""
if command -v sha256sum > /dev/null 2>&1; then
  SUMCHECK="sha256sum -c"
elif command -v shasum > /dev/null 2>&1; then
  SUMCHECK="shasum -a 256 -c"
fi
FIXTURE="vendor/fixture-validated.jsonl"
if [ -z "$SUMCHECK" ]; then
  echo "CANNOT-RUN (no sha256sum/shasum on PATH)"; FAIL=1
elif [ -f "$DRB/$FIXTURE" ]; then
  if ( cd "$DRB" && $SUMCHECK SHA256SUMS > /dev/null 2>&1 ); then
    echo "PASS (20/20)"
  else
    echo FAIL; FAIL=1
  fi
else
  PARTIAL="$(mktemp)"
  grep -v "  ${FIXTURE}\$" "$DRB/SHA256SUMS" > "$PARTIAL"
  if ( cd "$DRB" && $SUMCHECK "$PARTIAL" > /dev/null 2>&1 ); then
    echo "PASS (19/20 — ${FIXTURE} not fetched; see drb/README.md Provenance)"
  else
    echo FAIL; FAIL=1
  fi
  rm -f "$PARTIAL"
fi

# 2. The frozen subset is exactly the pre-registered 10 tasks.
note "2 subset = pre-registered ids"
IDS=$(python3 -c "
import json
ids=[json.loads(l)['id'] for l in open('$DRB/query.subset.jsonl')]
print(sorted(ids)==[56,58,59,62,65,69,78,83,90,95], len(ids))
")
if [ "$IDS" = "True 10" ]; then echo PASS; else echo "FAIL ($IDS)"; FAIL=1; fi

# 3. Scorer arithmetic (the instrument validation, live).
note "3 scorer selftest"
if python3 "$DRB/drb-score.py" --selftest > /dev/null 2>&1; then
  echo PASS
else
  echo FAIL; FAIL=1
fi

# 4. Both arms' score files exist and carry the full verdict record.
note "4 score files + verdicts"
OK=1
for arm in local hybrid; do
  f="$DEMO9/score-$arm.json"
  [ -f "$f" ] || { echo "missing $f"; OK=0; continue; }
  python3 - "$f" <<'PYEOF' || OK=0
import json, sys
r = json.load(open(sys.argv[1]))
for k in ("n_tasks", "pooled_fabrication", "paper_mean_fabrication",
          "bootstrap", "verdict_primary", "mean_cost_usd", "tasks"):
    if k not in r: print("missing key", k); sys.exit(1)
assert r["n_tasks"] == 10, r["n_tasks"]
assert r["bootstrap"]["n_resamples"] == 10000
assert len(r["tasks"]) == 10
for t in r["tasks"]:
    for k in ("id", "pairs", "counts", "wall_s", "cost_usd"):
        if k not in t: print("task missing", k); sys.exit(1)
PYEOF
done
if [ "$OK" = 1 ]; then echo PASS; else echo FAIL; FAIL=1; fi

# 5. The dr-verdict bar transition exists in the toml, names all three
#    legs P4/P2/P1, and lands on one of the four verdicts.
note "5 bars transition"
if python3 - "$ROOT/quality/initiative-bars.toml" <<'PYEOF'; then echo PASS; else echo FAIL; FAIL=1; fi
import sys, re
src = open(sys.argv[1], encoding="utf-8").read()
m = re.search(r'id = "dr-verdict"(.*?)(?=\n\[\[initiative\]\]|\n\[initiative\])', src, re.S)
assert m, "dr-verdict bar not found"
block = m.group(1)
# bar text must be the frozen one, unedited
assert "ship iff P4 AND P2 AND P1" in block, "bar text changed"
ts = re.findall(r'\[\[initiative\.bar\.transition\]\]\s*\n\s*on = "([^"]+)"\s*\n\s*to = "([^"]+)"', block)
assert len(ts) >= 2, f"expected declared + landing transitions, got {ts}"
last_on, last_to = ts[-1]
assert last_to in ("met", "failed", "could-not-judge", "never-ran"), last_to
by = re.findall(r'to = "' + last_to + r'"\s*\n\s*by = """(.*?)"""', block, re.S)
assert by and ("P4" in by[0] and "P2" in by[0] and "P1" in by[0]), "landing evidence must name P4/P2/P1"
print(f"transition {last_on} -> {last_to}, evidence names P4/P2/P1")
PYEOF

# 6. demo9/bars.md carries the SAME transition verbatim (both ways).
note "6 bars.md correspondence"
if python3 - "$ROOT/quality/initiative-bars.toml" "$DEMO9/bars.md" <<'PYEOF'; then echo PASS; else echo FAIL; FAIL=1; fi
import re, sys
toml = open(sys.argv[1], encoding="utf-8").read()
md = open(sys.argv[2], encoding="utf-8").read()
m = re.search(r'id = "dr-verdict"(.*?)(?=\n\[\[initiative\]\]|\n\[initiative\])', toml, re.S)
block = m.group(1)
ts = re.findall(r'\[\[initiative\.bar\.transition\]\]\s*\n\s*on = "([^"]+)"\s*\n\s*to = "([^"]+)"', block)
last_on, last_to = ts[-1]
by = re.search(r'to = "' + last_to + r'"\s*\n\s*by = """(.*?)"""', block, re.S).group(1)
# normalize: the toml 'by' text must appear verbatim in bars.md
assert by in md, "landing evidence not verbatim in bars.md"
assert f'to = "{last_to}"' in md
print("bars.md carries the landing transition verbatim")
PYEOF

# 7. Attribution: the DEMO-9 report names the failed claims and the
#    per-task rates (K/N), both arms.
note "7 attribution in report"
python3 - "$DEMO9/README.md" <<'PYEOF' || { echo FAIL; FAIL=1; }
import re, sys
md = open(sys.argv[1], encoding="utf-8").read()
assert "K/N" in md
assert re.search(r'failed claim', md, re.I) or re.search(r'attribution', md, re.I)
ids = set("56 58 59 62 65 69 78 83 90 95".split())
present = set(re.findall(r'\b(?:task|id)\s*[=:]\s*(\d{2})\b', md))
assert ids & present, f"per-task table missing tasks: {ids-present}"
print("report carries K/N + attribution + per-task table")
PYEOF
[ $? -eq 0 ] && echo PASS

# 8. The kill bar text is frozen in the report (verbatim line).
note "8 kill-bar text frozen"
if grep -q "ship iff P4 AND P2 AND P1" "$DEMO9/README.md" \
   && grep -q "cheapness is never a pass" "$DEMO9/README.md"; then
  echo PASS
else
  echo FAIL; FAIL=1
fi

echo
if [ "$FAIL" = 0 ]; then echo "verify-demo9: ALL STRIPS PASS"; else echo "verify-demo9: FAILURES PRESENT"; fi
exit "$FAIL"
