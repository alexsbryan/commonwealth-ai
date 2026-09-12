#!/bin/sh
# counts: checker for the prompt climb (PRE-REG v0). Runs from the
# candidate workdir — $PWD IS the prompt-overlay root the solver
# snapshotted. Re-enriches the one-essay scratch corpus under the
# candidate prompts, scores the subset golden, emits PASS/FAIL lines.
#
# Portable form: the fixture (recipe, golden, essay) lives one
# directory up from this template when materialized from
# sovereign-recipes/climb-ostrom/, or at $CLIMB_FIXTURE when set.
set -eu
CORPUS=climb-ostrom
FIXTURE="${CLIMB_FIXTURE:-$(cd "$(dirname "$0")/.." && pwd)}"
GOLDEN="$FIXTURE/climb-ostrom-golden.toml"
SOURCE="$FIXTURE/Ostrom Summary.md"
REPORT=/tmp/climb-ostrom-report.json
BIN="${SOVEREIGN_BIN:-sovereign}"

# Re-extraction must actually re-run. A bare `enrich reset` clears
# nothing (31s cached runs proved it); `--full` wipes the whole
# scaffold, so the sequence is reset → re-init → build (the engine's
# own prescription). ~3s overhead for a guaranteed-fresh extraction.
"$BIN" enrich reset "$CORPUS" --full --yes >/dev/null 2>&1 || true
"$BIN" enrich init "$CORPUS" \
  --source "$SOURCE" \
  --pipeline philosophy_atlas \
  --chapter-regex '(?m)^Ostrom' \
  --force >/dev/null 2>&1 || {
  echo "error: enrich init failed"
  exit 1
}
SOVEREIGN_PROMPT_DIR="$PWD" "$BIN" enrich build "$CORPUS" --full >/dev/null 2>&1 || {
  echo "error: enrich build failed under candidate prompts"
  exit 1
}
"$BIN" enrich eval "$CORPUS" "$GOLDEN" --report "$REPORT" >/dev/null 2>&1 || true

python3 - "$REPORT" <<'PY'
import json, sys
try:
    d = json.load(open(sys.argv[1]))
except Exception as e:
    print(f"error: unreadable eval report: {e}")
    raise SystemExit(1)
sections = ["person_atoms", "concept_atoms", "event_atoms", "claim_atoms"]
sections += list((d.get("axis_scores") or {}).keys())
failed = 0
for a in sections:
    sec = d.get(a) or {}
    if not isinstance(sec, dict):
        continue
    matched = sec.get("matched", 0) or 0
    misses = sec.get("misses", []) or []
    forbidden = sec.get("forbidden_hit", 0) or 0
    for i in range(matched):
        print(f"PASS {a}.matched_{i + 1}")
    for name in misses:
        print(f"FAIL {a}.{name}")
        failed += 1
    for i in range(forbidden):
        print(f"FAIL {a}.forbidden_{i + 1}")
        failed += 1
raise SystemExit(1 if failed else 0)
PY
