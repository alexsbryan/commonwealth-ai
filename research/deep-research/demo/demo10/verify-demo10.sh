#!/usr/bin/env bash
# verify-demo10.sh — DEMO-10 corpus admission tie-break + strip-3c strips
# (order deep-research-t2c).
#
# The demo's claim: the v1 report-class question is rendered by the loop
# with the two t2c instruments landed — Instrument 1 (the corpus
# admission decider's deterministic second key: score desc -> query-term
# overlap desc -> insertion order, the term-ranked mock's reference
# shape, §10.6) and Instrument 2 (the strip-3c anti-leak: gap queries
# carry no figure tokens beyond the question's own — both the
# floor-capped FACT query and the prose template). This script checks
# the flight artifacts rather than the prose:
#
#   1. the v1 corpus flight exists (the battery's v1 run dir) and
#      terminated;
#   2. every claim in the report is verdict-stamped;
#   3. every FIGURE token in any PASSED-position claim — final
#      verdict-set, per-round audits (gap-list-N), or report stamps —
#      is attributable to the run's accumulated evidence window (the
#      honesty constitution — zero untraced figures in [passed]
#      position, artifact-verified). Same decider as DEMO-7: the
#      scorer's OWN boundary-protected tokenizer (NUMERIC_TOKEN,
#      loaded from score-arms.py — one decider, one implementation,
#      §10.6), the citation tail cut at the earliest citation marker,
#      presence = substring of the joined window text;
#   3a. the untraced-reason honesty over the final verdict-set (no
#      untraced flag on a passed-position claim; every untraced reason
#      names figures genuinely absent from the window; no citation-tail
#      leak);
#   3b. the acquisition source is the corpus (every round-1 search hit
#      engine=corpus, chunk-level estate locators, personal custody);
#   3c. the concept -> value shape test — the strip-3c FIX, the
#      measured flip: at t1h this strip FAILED by measurement (the
#      round-1 gap-template query q1 carried "100", the survey
#      answer's own quoted figure from the admitted estate chunk); on
#      the t2c flight the round-1 queries introduce no VALUE-SHAPED
#      digit runs beyond the question's own (3+ digits, not 4-digit
#      19xx/20xx era years, not all-zero runs — the demo7 decider,
#      verbatim). The strip's code is neutral; the outcome is the
#      measurement;
#   3d. the tie-break decider's engagement + K/N per key: the scorer's
#      OWN decider (score-arms.py score_keys + parse_v1_keys +
#      V1_CORRECTIONS, loaded, not copied) over the 16 v1 bank keys on
#      this flight's report. The engagement condition is artifact-level
#      (the t1h mechanism): every round-1 search hit sits in the
#      identical f32 bucket AND that bucket is the triage threshold AND
#      the triage below_cut carries rejects — the corpus search
#      returned more hits than were admitted, so admission inside the
#      tied bucket was decided by the second key this order added (the
#      decider's behavior at equal score is pinned by the landed
#      red-first unit test, gym.rs). The pre-registered prediction is
#      journaled by measured outcome, never silenced (§7.6 — predicted,
#      never assumed; Amendment C — a measured failure is the
#      measurement): measured 2/16 — the 10 predicted Class-C
#      recoveries did NOT hold, the covered keys (K8, K14) were outside
#      the predicted set, and the frozen Class-D ceiling held for K9
#      (cannot-clear). The strip's verdict covers the artifacts and the
#      scorer's consistency, not the prediction's truth — the bar
#      verdict lives in bars.md, verbatim from the scorer (strip 4);
#   4. bars.md carries the scorer's per-question fractions and bar legs
#      verbatim (score-report-t2c.json) — never hand-typed;
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
DR_ROOT="$(cd "$DEMO_DIR/../.." && pwd)"
CORPUS_ID="dr-demo6-v1"

# The battery's v1 corpus flight is the NEWEST corpus-sourced epoch
# under v1/ (epochs accumulate; the mock v1 flight from run-arms.sh
# may sit alongside). Run-dir override for instrument validation
# against a scratch run.
V1_RUN_DIR="${V1_RUN_DIR:-$(ls -dt "$ARMS"/runs/loop/v1/dr-* 2>/dev/null | head -1)}"
[ -n "$V1_RUN_DIR" ] || { echo "FAIL: v1 run dir missing under $ARMS/runs/loop/v1/ (run the battery first)"; exit 1; }
V1_ENGINE="$(python3 -c "
import json,sys
m=json.load(open('$V1_RUN_DIR/manifest.json'))
spent=m.get('budget',{}).get('spent',{})
print('corpus' if any('corpus' in k for k in spent) else 'mock')
" 2>/dev/null)"
if [ "$V1_ENGINE" != "corpus" ]; then
  # The newest epoch is the run-arms.sh mock flight — step over it.
  for d in $(ls -dt "$ARMS"/runs/loop/v1/dr-* 2>/dev/null); do
    spent="$(python3 -c "
import json,sys
m=json.load(open('$d/manifest.json'))
spent=m.get('budget',{}).get('spent',{})
print('corpus' if any('corpus' in k for k in spent) else 'mock')
" 2>/dev/null)"
    if [ "$spent" = "corpus" ]; then V1_RUN_DIR="$d"; break; fi
  done
fi
[ -n "$V1_RUN_DIR" ] || { echo "FAIL: no corpus-sourced v1 flight under $ARMS/runs/loop/v1/"; exit 1; }

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

# The audits' evidence (audit_pass) is the merged window: round 1's
# estate window (the survey's searched hits — survey-1.json, ids
# estate-N) plus the acquisition windows (evidence-window-N.json). On
# the t2c flight the survey window is an 8-chunk SUPERSET of the
# 4-chunk acquisition window (the precondition search sees all 8; the
# tie-break triage admits 4) — a passed claim's figure may trace to a
# survey-window chunk the acquisition window does not carry (measured:
# gap-list-1 c4's "100" lives in survey chunk 33, absent from
# evidence-window-1). The strip checks against the UNION — the window
# the audits actually saw — never a subset (one run of the
# subset-check measured a false violation; §18.4 — validate the
# instrument before the result).
def audit_evidence(run):
    texts = []
    sw = run / "survey-1.json"
    if sw.exists():
        d = json.loads(sw.read_text())
        for q in d.get("searched", []):
            for h in q.get("hits", []):
                c = h.get("content") or h.get("snippet") or ""
                if c:
                    texts.append(c)
    for w in sorted(run.glob("evidence-window-*.json")):
        for c in json.loads(w.read_text()).get("chunks", []):
            texts.append(c["content"])
    return "\n".join(texts)

window_text = audit_evidence(run).lower()
assert window_text.strip(), "no evidence chunks across the survey + acquisition windows — nothing to attribute to"

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
vs = json.loads((run / "verdict-set.json").read_text())
missing += check_claims(vs.get("claims", []), "verdict-set")
for gl in sorted(run.glob("gap-list-*.json")):
    d = json.loads(gl.read_text())
    missing += check_claims(d.get("claims", []), gl.name)
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

# The audits' evidence — the survey window + the acquisition windows
# (see the strip 2-3 header: the round-1 audit saw the 8-chunk survey
# window, a superset of evidence-window-1's 4 chunks).
def audit_evidence(run):
    texts = []
    sw = run / "survey-1.json"
    if sw.exists():
        d = json.loads(sw.read_text())
        for q in d.get("searched", []):
            for h in q.get("hits", []):
                c = h.get("content") or h.get("snippet") or ""
                if c:
                    texts.append(c)
    for w in sorted(run.glob("evidence-window-*.json")):
        for c in json.loads(w.read_text()).get("chunks", []):
            texts.append(c["content"])
    return "\n".join(texts)

window_text = audit_evidence(run).lower()

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

# --- 3c. the concept -> value shape test (the strip-3c flip) ----------
# At t1h this strip FAILED by measurement: the round-1 gap-template
# query q1 carried "100", the survey answer's own quoted figure from
# the admitted estate chunk (terry-uga, "the nation's largest 100
# cities"). The t2c instrument (strip-3c: gap formation carries no
# figure tokens beyond the question's own) is what this flight
# measures. The decider is demo7's, verbatim: value-shaped = 3+ digits,
# not 4-digit 19xx/20xx era years, not all-zero runs.
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
print("round-1 queries carry NO value-shaped digits beyond the question's own (t1h: \"100\" leaked — measured flip)")
PY
verdict "concept->value shape strip (3c) — the strip-3c fix" $?

# --- 3d. the tie-break decider: quantized bucket + selectivity + K/N --
# The engagement condition (the t1h mechanism, artifact-level): every
# round-1 search hit's score sits in the identical f32 bucket, that
# bucket IS the triage threshold, and the triage below_cut carries the
# rejects — the corpus search returned more hits than were admitted, so
# admission inside the tied bucket was decided by the second key this
# order added. The decider's behavior at equal score (term-overlap desc
# admits the figure-bearing hit over the figure-free hit inserted first)
# is pinned by the landed red-first unit test in gym.rs — the flight
# strip verifies the engagement and measures the outcome with the
# scorer's OWN decider (loaded, not copied), journaling the
# pre-registered prediction by measured outcome (FAILED — never
# silenced). The corpus-admission trace is debug-level and does not
# reach the default console log, so no console assertion is attempted
# (journaled in the execution record).
python3 - "$V1_RUN_DIR" "$ARMS" "$DR_ROOT" <<'PY'
import importlib.util, json, pathlib, sys
run = pathlib.Path(sys.argv[1])
arms = pathlib.Path(sys.argv[2])
root = pathlib.Path(sys.argv[3])

fl = json.loads((run / "fetch-list-1.json").read_text())
hits = fl["search_hits"]
scores = {h.get("score") for h in hits}
assert len(scores) == 1 and 0.0 < next(iter(scores)) < 1.0, \
    f"round-1 search hits do not sit in one identical score bucket: {scores}"
bucket = next(iter(scores))
triage = fl.get("triage") or {}
threshold = triage.get("threshold")
assert threshold is not None and abs(threshold - bucket) < 1e-12, \
    f"hit score {bucket} != the triage threshold {threshold} — the quantized bucket is not the admission threshold"
below_cut = triage.get("below_cut") or []
admitted_ids = {h["url"].rsplit(":", 1)[-1] for h in hits}
# The epsilon admission (eps_quota 0.1, recorded in the triage) admits a
# small below-threshold quota — an eps-admitted id legitimately appears
# in below_cut too (it was below the cut, then quota-admitted). The
# engagement fact is the REJECTS: distinct candidates that were neither
# admitted nor quota-admitted. Those are the ties the second key had to
# break against.
eps_ids = set(triage.get("eps_admits", []))
rejects = [i for i in set(below_cut) if i not in admitted_ids and i not in eps_ids]
assert len(rejects) > 0, \
    "no rejected candidates — the corpus search returned only the admitted hits, so no tie ever needed breaking"
print(f"round-1: {len(hits)} admitted hits, all scoring {bucket} (the quantized bucket = the triage threshold); "
      f"{len(below_cut)} below_cut candidates, {len(rejects)} distinct rejects "
      f"(+ {len(eps_ids & set(below_cut))} eps-quota admits) — admission was selective "
      "inside the tied bucket, so the second key decided it")

spec = importlib.util.spec_from_file_location("scorearms", str(arms / "score-arms.py"))
sa = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sa)

report = (run / "report.md").read_text()
windows = [json.loads(p.read_text()) for p in sorted(run.glob("evidence-window-*.json"))]
window_text = "\n".join(c["content"].lower() for w in windows for c in w["chunks"])

keys = sa.parse_v1_keys((root / "bank/v1/seeds.md").read_text())
rows = sa.score_keys(keys, report, window_text, None, sa.V1_CORRECTIONS)
by_key = {r["key"]: r for r in rows}
assert len(by_key) == 16, f"16 v1 keys scored, got {len(by_key)}"

PREDICTED_RECOVER = ["K1", "K2", "K4", "K5", "K6", "K7", "K10", "K11", "K12", "K15"]
CLASS_D = ["K3", "K9", "K13"]

covered = sorted(k for k, r in by_key.items() if r["covered"])
print("K/N per key (the scorer's decider, this flight):")
for k in ["K1", "K2", "K3", "K4", "K5", "K6", "K7", "K8", "K9", "K10",
          "K11", "K12", "K13", "K14", "K15", "K16"]:
    r = by_key[k]
    print(f"  {k}: {'covered' if r['covered'] else 'uncovered'} — {r['reason'][:80]}")

# The frozen Class-D ceiling: K9 is cannot-clear under the frozen scorer
# (V1_CORRECTIONS, arbiter journal) — it cannot clear on ANY flight.
k9 = by_key["K9"]
assert not k9["covered"] and "cannot clear" in k9["reason"], \
    f"K9 covered or cleared — the frozen-arbiter ceiling moved: {k9['reason']}"

# The journaled prediction outcome. The pre-registration predicted the
# 10 standing Class-C keys recover with the deterministic second key.
# Predicted, never assumed (§7.6): the strip verifies the artifacts and
# records the outcome — a failed prediction is the measurement, never
# silenced (Amendment C), and the bar verdict lives in bars.md (strip 4).
pred_recovered = [k for k in PREDICTED_RECOVER if by_key[k]["covered"]]
n_covered = len(covered)
print(f"K/N measured: {n_covered}/16 covered")
print("pre-registered prediction outcome (journaled, never silenced): "
      f"predicted {len(PREDICTED_RECOVER)} standing Class-C recoveries, "
      f"measured {len(pred_recovered)}/10 recovered — PREDICTION FAILED")
print(f"  covered keys: {covered} — neither K8 nor K14 was in the predicted set")
k3, k13 = by_key["K3"], by_key["K13"]
print(f"  Class-D K3/K9/K13: K9 cannot-clear held (frozen journal); "
      f"K3/K13 uncovered by the figure decider ({k3['reason'][:44]} | {k13['reason'][:44]}) — "
      "measured uncovered, not gated")
PY
verdict "tie-break K/N strip (3d)" $?

# --- 4. bars.md is the scorer's numbers ------------------------------
[ -f "$ARMS/score-report-t2c.json" ] || { echo "FAIL: score-report-t2c.json missing (score the battery first)"; exit 1; }
python3 - "$ARMS" <<'PY'
import json, pathlib, sys
arms = pathlib.Path(sys.argv[1])
report = json.load(open(arms / "score-report-t2c.json"))
bars = (arms.parent / "demo/demo10/bars.md").read_text()
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
report = json.load(open(arms / "score-report-t2c.json"))
bars = (arms.parent / "demo/demo10/bars.md").read_text()
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
  echo "=== DEMO-10 verify: $FAILURES strip(s) FAILED — the failures are the measurements (named above) ==="
  exit 1
fi
echo "=== DEMO-10 verify: all strips pass ==="
