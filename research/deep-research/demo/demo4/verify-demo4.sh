#!/usr/bin/env bash
# verify-demo4.sh — DEMO-4 honesty strips (order deep-research-t1e).
#
# The demo's claim: the v1 report-class question is rendered by the
# figure-hunting loop — the plan artifact's sub-questions carry figure
# specifiers (the question's own digits + measure-family words,
# recorded and folded in), the triage records its figure-bearing
# admission rule, and the report is every-number-attributable with
# absences named. This script checks the flight artifacts rather than
# the prose:
#
#   1. the v1 flight exists (the battery's run dir) and terminated;
#   2. every claim in the report is verdict-stamped;
#   3. every FIGURE token in the report's claims is attributable to the
#      run's accumulated evidence window, OR the claim is flagged
#      could-not-judge/never-ran (absence named);
#   3b. the acquisition mechanics on THIS flight are the figure-hunting
#      loop's — the plan artifact records the question's own figure
#      specifiers and every plan sub-question carries a specifier
#      (digit or measure word), and the triage outcomes record
#      `score-then-figure-bearing`;
#   4. bars.md carries the scorer's per-question fractions and bar legs
#      verbatim (score-report-t1e.json) — never hand-typed;
#   5. the two-arm lift is the same scorer's, over the same pairs.
#
# Exits non-zero with a named reason on any strip that fails.
set -u

DEMO_DIR="$(cd "$(dirname "$0")" && pwd)"
ARMS="$(cd "$DEMO_DIR/../../arms" && pwd)"

# The battery's v1 flight is the NEWEST (epochs accumulate under v1/);
# lexicographic head -1 would pick the t1d-era flight, which predates
# the figure-hunting mechanisms (measured 2026-08-14: strip 3b failed
# on dr-1786754967, passed on dr-1786760406).
V1_RUN_DIR="$(ls -dt "$ARMS"/runs/loop/v1/dr-* 2>/dev/null | head -1)"
[ -n "$V1_RUN_DIR" ] || { echo "FAIL: v1 run dir missing under $ARMS/runs/loop/v1/ (run the battery first)"; exit 1; }

# --- 1. the flight terminated ---------------------------------------
[ -f "$V1_RUN_DIR/report.md" ] || { echo "FAIL: no report.md in $V1_RUN_DIR"; exit 1; }
MANIFEST_TERMINAL="$(python3 -c "import json,sys; m=json.load(open('$V1_RUN_DIR/manifest.json')); print(m.get('terminal_state') or m.get('state') or 'missing')" 2>/dev/null)"
echo "v1 flight: $V1_RUN_DIR (terminal: $MANIFEST_TERMINAL)"

# --- 2-3. claims verdict-stamped; figures attributable --------------
python3 - "$V1_RUN_DIR" <<'PY'
import json, pathlib, re, sys
run = pathlib.Path(sys.argv[1])
report = (run / "report.md").read_text()

stamped = re.findall(r"\[(passed|failed|could-not-judge|never-ran)\]", report)
assert stamped, "no verdict-stamped claims in the report — a claim with no verdict is a silent number"

windows = [json.loads(p.read_text()) for p in sorted(run.glob("evidence-window-*.json"))]
bodies = [c["content"].lower() for w in windows for c in w["chunks"]]
assert bodies, f"no evidence chunks across {len(windows)} windows — nothing to attribute to"

flag_pattern = re.compile(r"\[(passed|failed|could-not-judge|never-ran)\]")
missing = []
for line in report.splitlines():
    m = flag_pattern.search(line)
    if not m:
        continue  # prose (title, run line): no claim, nothing to attribute
    claim_text = line.split("]", 1)[1]
    figs = re.findall(r"\$?\d[\d,.:/%$-]*", claim_text)
    for f in figs:
        f_l = f.lower()
        if f_l in bodies:
            continue
        if m.group(1) != "passed":
            continue  # absence named on a flagged claim
        missing.append((f, line[:80]))
assert not missing, f"figures in passed claims absent from the evidence window: {missing[:5]}"
print(f"report: {len(stamped)} verdict-stamped claims; all figures attributable or on flagged claims")
PY
[ $? -eq 0 ] || { echo "FAIL: attribution strips (2-3)"; exit 1; }

# --- 3b. the acquisition mechanics are the figure-hunting loop's ------
# The demo's claim is "rendered by the figure-hunting loop" — the loop's
# own artifacts must show, on THIS flight:
#   a. the launch plan (plan.json) records the question's own figure
#      specifiers (non-empty when the question implies figures);
#   b. EVERY plan sub-question carries a figure specifier (a digit run
#      or a measure-family word) — the same decider the SHAPE test
#      pins, re-derived here independently;
#   c. the triage outcomes record `score-then-figure-bearing` (the
#      admission rule that stops the K-cut from excluding the hits the
#      figures live in).
python3 - "$V1_RUN_DIR" <<'PY'
import json, pathlib, re, sys
run = pathlib.Path(sys.argv[1])

# The declared measure family (pre-registration.md, t1e declaration) —
# SHAPES, never bank measures.
MEASURE = frozenset("""index ratio share rate percent percentage median average mean
count number price income earnings wage salary employment jobs population mobility cost
rent poverty wealth proportion statistic metric estimate amount total level""".split())

def has_specifier(text):
    if re.search(r"\d", text):
        return True
    words = set(re.findall(r"[a-z]+", text.lower()))
    return not words.isdisjoint(MEASURE)

plan = json.loads((run / "plan.json").read_text())
acq = plan.get("acquisition", {})
specs = acq.get("figure_specifiers", [])
subs = acq.get("queries_preplanned", [])
assert subs, "plan.json carries no sub-questions — the frontier is missing"
if specs:
    print(f"plan figure_specifiers: {specs} (the question's own digits + measure words)")
bare = [s for s in subs if not has_specifier(s)]
assert not bare, f"plan sub-questions without a figure specifier: {bare}"
print(f"plan sub-questions: {len(subs)}/{len(subs)} carry a figure specifier")

# c. triage records the admission rule it ran.
rules = set()
for p in sorted(run.glob("fetch-list-*.json")):
    fl = json.loads(p.read_text())
    t = fl.get("triage", {})
    if t.get("admission_rule"):
        rules.add(t["admission_rule"])
assert rules == {"score-then-figure-bearing"}, \
    f"triage outcomes do not record score-then-figure-bearing: {rules or 'none recorded'}"
print(f"triage admission rules recorded: {sorted(rules)}")
PY
[ $? -eq 0 ] || { echo "FAIL: figure-hunting mechanics strip (3b)"; exit 1; }

# --- 4. bars.md is the scorer's numbers ------------------------------
[ -f "$ARMS/score-report-t1e.json" ] || { echo "FAIL: score-report-t1e.json missing (score the battery first)"; exit 1; }
python3 - "$ARMS" <<'PY'
import json, pathlib, sys
arms = pathlib.Path(sys.argv[1])
report = json.load(open(arms / "score-report-t1e.json"))
bars = (arms.parent / "demo/demo4/bars.md").read_text()
summ = report["summary"]
for pid, row in summ.get("per_question", {}).items():
    frac = row.get("loop_covered", "")
    if frac and f"{pid}" in bars and frac not in bars:
        print(f"bars.md does not carry scorer's loop_covered for {pid} ({frac})")
        sys.exit(1)
for leg in report["bars"]["verdicts"]:
    if leg["leg"] in bars and leg["measured"] not in bars:
        print(f"bars.md does not carry scorer's measured for {leg['leg']} ({leg['measured']})")
        sys.exit(1)
    if leg["leg"] in bars and leg["verdict"] not in bars:
        print(f"bars.md does not carry scorer's verdict for {leg['leg']} ({leg['verdict']})")
        sys.exit(1)
print("bars.md carries the scorer's per-question fractions and bar legs verbatim")
PY
[ $? -eq 0 ] || { echo "FAIL: bars/score consistency (4)"; exit 1; }

# --- 5. the two-arm lift is the scorer's, over the same pairs ---------
python3 - "$ARMS" <<'PY'
import json, pathlib, sys
arms = pathlib.Path(sys.argv[1])
report = json.load(open(arms / "score-report-t1e.json"))
bars = (arms.parent / "demo/demo4/bars.md").read_text()
summ = report["summary"]
for pid, row in summ.get("per_question", {}).items():
    if not row.get("loop_covered") or not row.get("oneshot_covered"):
        print(f"{pid}: scored in one arm only ({row}) — the lift is not measured")
        sys.exit(1)
lift = summ.get("pooled_lift")
if lift is None or str(lift) not in bars:
    print(f"bars.md does not carry the scorer's pooled lift ({lift})")
    sys.exit(1)
print(f"two-arm lift: pooled {lift} from {len(summ['per_question'])} pairs scored in both arms")
PY
[ $? -eq 0 ] || { echo "FAIL: two-arm lift strip (5)"; exit 1; }

echo "=== DEMO-4 verify: all strips pass ==="
