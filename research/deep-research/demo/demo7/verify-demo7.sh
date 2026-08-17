#!/usr/bin/env bash
# verify-demo7.sh — DEMO-7 corpus-leg + honesty-strengthen strips (order
# deep-research-t1h).
#
# The demo's claim: the v1 report-class question is rendered by the loop
# with the t1h instrument changes landed — H1 (the hit surface carries
# the BODY; the figure-bearing decider reads title+snippet+content), H2
# (the deterministic figure inventory in the draft prompt), and the
# honesty strengthen (the witness checks the claim's OWN figure tokens
# against the evidence BEFORE extraction — the partial-trace shape the
# t1g probe exposed is downgraded, never passed). This script checks the
# flight artifacts rather than the prose:
#
#   1. the v1 corpus flight exists (the battery's v1 run dir) and
#      terminated;
#   2. every claim in the report is verdict-stamped;
#   3. every FIGURE token in any PASSED-position claim — final
#      verdict-set, per-round audits (gap-list-N), or report stamps — is
#      attributable to the run's accumulated evidence window (a flagged
#      claim's absence is named by its stamp — never enforced, never
#      removed). Same decider as DEMO-6: the scorer's OWN
#      boundary-protected tokenizer (NUMERIC_TOKEN, loaded from
#      score-arms.py — one decider, one implementation, §10.6), the
#      citation tail cut at the earliest citation marker of either
#      renderer, presence = substring of the joined window text.
#
#      At t1g this strip FAILED by measurement (the [passed] claim
#      restated the question's era years "1980"/"2024" with neither in
#      the window). The t1h flight's measured outcome: PASS — the
#      claim-figure short-circuit downgrades before the claim can sit in
#      passed position with an untraced figure. The strip's code is
#      neutral; the outcome is the measurement.
#   3a. the t1h instrument earn — the untraced-reason honesty, over the
#      FINAL verdict-set (the scorer's read surface):
#      a. no passed-position claim in the verdict-set carries an
#         "untraced:" flag (downgrade-only: the short-circuit removes
#         the fire, it never upgrades);
#      b. every "untraced:" reason names figures GENUINELY absent from
#         the accumulated window (the downgrade is honest — the reason
#         is checked, not trusted);
#      c. the citation-leak class (amendment 2): no untraced-named
#         figure appears in the claim's own citation tail — the
#         "[Source: ev-1]" shape ("1" named untraced from the claim's
#         own tail) must not recur. The leak was caught RED-FIRST on
#         this order's battery (seed-01..05 stopped, invalidated,
#         never scored; the fixed binary re-ran the battery clean).
#   3b. the acquisition source on THIS flight is the corpus (the t1g
#      instrument, unchanged):
#      a. every round-1 search hit is stamped engine "corpus" (zero
#         mock-deck hits on a corpus-source flight);
#      b. the admitted hits' locators are chunk-level estate locators —
#         `estate:dr-demo6-v1:<chunk_id>`, exactly two colons — and the
#         window chunks carry custody "personal" (the estate's stamp).
#   3c. the concept -> value shape test (the DEMO-5/6 journaled shape):
#      the round-1 queries introduce no VALUE-SHAPED digit runs beyond
#      the question's own (3+ digits, not 4-digit 19xx/20xx era years,
#      not all-zero runs), AND an admitted chunk's content carries
#      value-shaped figure runs in none of those queries.
#
#      MEASURED FAILURE on this flight (journaled, never silenced): the
#      round-1 gap-template query q1 (from the survey answer's gap row
#      g2) carries the value-shaped run "100" — the survey answer
#      (model) quoted the estate's own admitted chunk (terry-uga,
#      "the nation's largest 100 cities"), and the gap-template carried
#      the figure verbatim into the query. The figure traces to the
#      admitted window (attribution intact); what broke is the
#      query-side anti-leak property. The strip FAILS, naming "100".
#   4. bars.md carries the scorer's per-question fractions and bar legs
#      verbatim (score-report-t1h.json) — never hand-typed;
#   5. the two-arm lift is the same scorer's, over the same pairs.
#
# Exits non-zero with a named reason on any strip that fails. Measured
# failures accumulate (Amendment C, t1g precedent): every strip runs,
# every verdict is printed, the exit code is non-zero iff any strip
# failed. Hard preconditions (the flight's existence) still fail fast —
# absence is not a measurement.
set -u

FAILURES=0
verdict() { # <strip name> <exit code>
  if [ "$2" -eq 0 ]; then echo "PASS: $1"; else echo "FAIL: $1"; FAILURES=$((FAILURES + 1)); fi
}

DEMO_DIR="$(cd "$(dirname "$0")" && pwd)"
ARMS="$(cd "$DEMO_DIR/../../arms" && pwd)"
CORPUS_ID="dr-demo6-v1"

# The battery's v1 flight is the NEWEST (epochs accumulate under v1/).
# Run-dir override for instrument validation against a scratch run.
V1_RUN_DIR="${V1_RUN_DIR:-$(ls -dt "$ARMS"/runs/loop/v1/dr-* 2>/dev/null | head -1)}"
[ -n "$V1_RUN_DIR" ] || { echo "FAIL: v1 run dir missing under $ARMS/runs/loop/v1/ (run the battery first)"; exit 1; }

# --- 1. the flight terminated ---------------------------------------
[ -f "$V1_RUN_DIR/report.md" ] || { echo "FAIL: no report.md in $V1_RUN_DIR"; exit 1; }
MANIFEST_TERMINAL="$(python3 -c "import json,sys; m=json.load(open('$V1_RUN_DIR/manifest.json')); print(m.get('terminal_state') or m.get('state') or 'missing')" 2>/dev/null)"
echo "v1 corpus flight: $V1_RUN_DIR (terminal: $MANIFEST_TERMINAL)"
case "$MANIFEST_TERMINAL" in
  done|done-partial|"") : ;;
  *) echo "FAIL: flight did not terminate (terminal=$MANIFEST_TERMINAL)"; exit 1 ;;
esac

# --- 2-3. claims verdict-stamped; passed-position figures attributable
python3 - "$V1_RUN_DIR" "$ARMS" <<'PY'
import importlib.util, json, pathlib, re, sys
run = pathlib.Path(sys.argv[1])
report = (run / "report.md").read_text()

stamped = re.findall(r"\[(passed|failed|could-not-judge|never-ran)\]", report)
assert stamped, "no verdict-stamped claims in the report — a claim with no verdict is a silent number"

windows = [json.loads(p.read_text()) for p in sorted(run.glob("evidence-window-*.json"))]
window_text = "\n".join(c["content"].lower() for w in windows for c in w["chunks"])
assert window_text.strip(), f"no evidence chunks across {len(windows)} windows — nothing to attribute to"

# The decider's own claim tokenizer (score-arms.py NUMERIC_TOKEN),
# loaded, not copied — one decider, one implementation (§10.6): the
# gate cannot diverge from the scorer's figure semantics.
spec = importlib.util.spec_from_file_location("scorearms", str(pathlib.Path(sys.argv[2]) / "score-arms.py"))
sa = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sa)

TAIL = re.compile(r"(`estate-|\[estate:|\[Source:)")
def claim_body(text):
    cuts = [m.start() for m in TAIL.finditer(text)]
    return text[:min(cuts)] if cuts else text

def check_claims(claims, source):
    """Every PASSED-position claim's figure tokens must be in the window."""
    missing = []
    for c in claims:
        verdict = (c.get("verdict") or c.get("status") or "").lower()
        if verdict not in ("passed", "citation-grounded", "supported", "grounded"):
            continue
        text = claim_body(c.get("text") or c.get("claim") or "")
        for f in sa.NUMERIC_TOKEN.findall(text):
            if f.lower() not in window_text:
                missing.append((f, c.get("id"), source))
    return missing

missing = []
# (a) the final verdict-set — the scorer's read surface.
vs = json.loads((run / "verdict-set.json").read_text())
missing += check_claims(vs.get("claims", []), "verdict-set")
# (b) the per-round audits (gap-list-N.json) — a claim passed at ANY round.
for gl in sorted(run.glob("gap-list-*.json")):
    d = json.loads(gl.read_text())
    missing += check_claims(d.get("claims", []), gl.name)
# (c) the report's own stamps (the demo6 line-wise join, for parity).
flag_pattern = re.compile(r"\[(passed|failed|could-not-judge|never-ran)\]")
lines = report.splitlines()
i, n = 0, len(lines)
while i < n:
    line = lines[i]
    m = flag_pattern.search(line)
    if not m:
        i += 1
        continue
    claim_parts = [line.split("]", 1)[1]]
    j = i + 1
    while j < n and not lines[j].startswith("- ") and not lines[j].startswith("#") and lines[j].strip():
        claim_parts.append(lines[j])
        j += 1
    if m.group(1) == "passed":
        body = claim_body("\n".join(claim_parts))
        for f in sa.NUMERIC_TOKEN.findall(body):
            if f.lower() not in window_text:
                missing.append((f, line[:60], "report"))
    i = j
assert not missing, f"figures in passed-position claims absent from the evidence window: {missing[:5]}"
print(f"report: {len(stamped)} verdict-stamped claims; passed-position figures all attributable "
      f"(verdict-set + {len(list(run.glob('gap-list-*.json')))} round audits + report stamps)")
PY
verdict "attribution strips (2-3)" $?

# --- 3a. the untraced-reason honesty over the final verdict-set ------
python3 - "$V1_RUN_DIR" <<'PY'
import json, pathlib, re, sys
run = pathlib.Path(sys.argv[1])
vs = json.loads((run / "verdict-set.json").read_text())
windows = [json.loads(p.read_text()) for p in sorted(run.glob("evidence-window-*.json"))]
window_text = "\n".join(c["content"].lower() for w in windows for c in w["chunks"])

UNTRACED = re.compile(r"untraced\s*:\s*([0-9][0-9., ]*)")
TAIL = re.compile(r"\[Source:[^\]]*\]|\[[^\]]*:[\w-]+(?:/\w+)?\]")
PASSED = {"passed", "citation-grounded", "supported", "grounded"}

problems = []
n_untraced = 0
for c in vs.get("claims", []):
    verdict = (c.get("verdict") or c.get("status") or "").lower()
    flag = c.get("flag") or ""
    m = UNTRACED.search(flag)
    if not m:
        continue
    n_untraced += 1
    named = [t for t in re.findall(r"[0-9]+", m.group(1))]
    if verdict in PASSED:
        problems.append(f"claim {c['id']} carries an untraced flag in PASSED position: {flag}")
    for f in named:
        if f in window_text:
            problems.append(f"claim {c['id']} untraced reason names '{f}' but the window CARRIES it — the downgrade is dishonest")
    # the citation-leak shape (amendment 2): the named figure must not
    # come from the claim's own citation tail ("[Source: ev-1]" -> "1").
    tail = "".join(TAIL.findall(c.get("text") or ""))
    tail_digits = re.findall(r"[0-9]+", tail)
    for f in named:
        if f in tail_digits:
            problems.append(f"claim {c['id']} untraced '{f}' comes from the claim's OWN citation tail — the leak class recurred")
print(f"verdict-set: {len(vs.get('claims', []))} claims, {n_untraced} untraced-flagged")
assert not problems, "untraced-reason honesty violations:\n  " + "\n  ".join(problems[:6])
PY
verdict "untraced-reason honesty strip (3a)" $?

# --- 3b. the acquisition source is the corpus ------------------------
python3 - "$V1_RUN_DIR" <<'PY'
import json, pathlib, sys
run = pathlib.Path(sys.argv[1])
fl = json.loads((run / "fetch-list-1.json").read_text())
hits = fl["search_hits"]
assert hits, "round-1 fetch list carries no search hits — the corpus source retrieved nothing"
engines = {h.get("engine") for h in hits}
assert engines == {"corpus"}, \
    f"round-1 search hits are not all corpus-sourced: engines {engines}"
print(f"round-1 search hits: {len(hits)}, engine=corpus on every hit")
estate_hits = [h for h in hits if h.get("url", "").startswith("estate:")]
assert estate_hits, "no estate: locators on the round-1 search hits"
for h in estate_hits:
    parts = h["url"].split(":")
    assert len(parts) == 3 and parts[1] == "dr-demo6-v1", \
        f"hit locator is not a chunk-level estate locator (2 colons, corpus id): {h['url']}"
windows = [json.loads(p.read_text()) for p in sorted(run.glob("evidence-window-*.json"))]
window_chunks = [c for w in windows for c in w["chunks"]]
custodies = {c.get("custody") for c in window_chunks}
assert custodies == {"personal"}, \
    f"window custody is not the estate's personal stamp: {custodies}"
locators = [c.get("locator", "") for c in window_chunks]
assert all(l.startswith("estate:dr-demo6-v1:") and l.count(":") == 2 for l in locators), \
    f"window locators are not chunk-level estate locators: {locators[:3]}"
print(f"window: {len(window_chunks)} chunk(s), custody=personal, locators estate:dr-demo6-v1:<chunk>")
PY
verdict "corpus-source strip (3b)" $?

# --- 3c. the concept -> value shape test ------------------------------
# MEASURED FAILURE on this flight: the round-1 gap-template query q1
# (formed from the survey answer's gap row g2) carries the value-shaped
# run "100" — the survey answer (model) quoted the estate's own admitted
# chunk (terry-uga, "the nation's largest 100 cities") and the
# gap-template carried the figure verbatim into the query. The figure
# traces to the admitted window (attribution intact); the query-side
# anti-leak property is what broke. The strip fails, naming the run.
python3 - "$V1_RUN_DIR" <<'PY'
import json, pathlib, re, sys
run = pathlib.Path(sys.argv[1])
question = json.loads((run / "charter.json").read_text())["question"]
fl = json.loads((run / "fetch-list-1.json").read_text())

def value_runs(text):
    out = set()
    for d in re.findall(r"[0-9]+", text):
        if len(d) >= 3 and not (len(d) == 4 and 1900 <= int(d) <= 2099) and set(d) != {"0"}:
            out.add(d)
    return out

queries = " ".join(q.get("text", "") for q in fl.get("queries", []))
q_value = value_runs(queries) - value_runs(question)
assert not q_value, \
    f"round-1 queries introduce value-shaped digits the question did not: {sorted(q_value)}"
print("round-1 queries carry NO value-shaped digits beyond the question's own")
PY
verdict "concept->value shape strip (3c) — MEASURED FAILURE expected (query carries \"100\", journaled)" $?

# --- 4. bars.md is the scorer's numbers ------------------------------
[ -f "$ARMS/score-report-t1h.json" ] || { echo "FAIL: score-report-t1h.json missing (score the battery first)"; exit 1; }
python3 - "$ARMS" <<'PY'
import json, pathlib, sys
arms = pathlib.Path(sys.argv[1])
report = json.load(open(arms / "score-report-t1h.json"))
bars = (arms.parent / "demo/demo7/bars.md").read_text()
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
verdict "bars/score consistency (4)" $?

# --- 5. the two-arm lift is the scorer's, over the same pairs ---------
python3 - "$ARMS" <<'PY'
import json, pathlib, sys
arms = pathlib.Path(sys.argv[1])
report = json.load(open(arms / "score-report-t1h.json"))
bars = (arms.parent / "demo/demo7/bars.md").read_text()
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
verdict "two-arm lift strip (5)" $?

if [ "$FAILURES" -gt 0 ]; then
  echo "=== DEMO-7 verify: $FAILURES strip(s) FAILED — the failures are the measurements (named above) ==="
  exit 1
fi
echo "=== DEMO-7 verify: all strips pass ==="
