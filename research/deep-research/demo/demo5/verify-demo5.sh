#!/usr/bin/env bash
# verify-demo5.sh — DEMO-5 retrieval-instrument strips (order deep-research-t1f).
#
# The demo's claim: the v1 report-class question is rendered by a loop
# whose MOCK retrieval is TERM-RANKED (T1.9) — a CONCEPT query retrieves
# the value-bearing document without the loop ever naming the bank's
# figures — and the report is every-number-attributable. This script
# checks the flight artifacts rather than the prose:
#
#   1. the v1 flight exists (the battery's run dir) and terminated;
#   2. every claim in the report is verdict-stamped;
#   3. every FIGURE token in the report's [passed] claims is attributable
#      to the run's accumulated evidence window (a flagged claim's
#      absence is named by its stamp — never enforced, never removed).
#      AMENDMENT 2026-08-15 (watched-fail -> fix, journaled, never
#      silent): the committed version checked `token in bodies` — list
#      membership against the chunk strings — so NO token ever matched
#      and every figure-bearing passed claim failed (watched: exit 1,
#      "figures in passed claims absent from the evidence window", on
#      the t1f v1 flight). demo4's flight never fired it: its passed
#      claims carry no figures on the stamped line. Now: the scorer's
#      OWN boundary-protected tokenizer (NUMERIC_TOKEN, loaded from
#      score-arms.py — one decider, one implementation, §10.6), the
#      citation tail cut at "[Source:", presence = substring of the
#      joined window text ("1990" traces inside "the 1990s" — the
#      window carries the deck verbatim), and the claim body joined
#      across the renderer's bullet+continuation lines.
#   3b. the retrieval mechanics on THIS flight are term-ranked (the
#      instrument change, pre-registered in pre-registration.md before
#      the re-measure):
#      a. the ROUND-1 fetch list's search hits carry DISTINCT relevance
#         scores — the old exact-value instrument returned flat
#         0.9-score ties, the term index returns per-hit relevance
#         counts;
#      b. at least one ADMITTED hit (triage code_set_k + eps_admits)
#         whose VALUE-SHAPED figure runs — derived from the FROZEN deck
#         at verify time — appear in NO round-1 query, AND the queries
#         introduce no value-shaped digits beyond the question's own:
#         the query never names the bank's figures, yet the value-
#         bearing document is retrieved and admitted. (Reads the deck
#         READ-ONLY: running the frozen bank is the battery, never an
#         edit; the strip is shape-generic, deriving "distinctive
#         figures" from the deck, not from any bank key.)
#         AMENDMENT 2026-08-15 (watched-fail -> fix, journaled): the
#         committed version compared RAW digit runs, and the flight's
#         round-1 queries legitimately carry era years (1970..2023 —
#         the R1 prompt asks the draft to name years) and generic
#         descriptors ("15-year-old homes", "per 1,000 renters") that
#         also occur in the value-bearing bodies — over-strict, failed
#         on the real flight. VALUE-SHAPED runs = 3+ digits, not
#         4-digit 19xx/20xx era years, not all-zero runs: catches
#         3+ digit value leaks (5469, 325, 476) with a journaled
#         2-digit blind spot (7.87, 9.6, 95/20 — the structural
#         no-bank-vocabulary guarantee lives in the fold-in machinery,
#         which this strip does not duplicate).
#   4. bars.md carries the scorer's per-question fractions and bar legs
#      verbatim (score-report-t1f.json) — never hand-typed;
#   5. the two-arm lift is the same scorer's, over the same pairs.
#
# Exits non-zero with a named reason on any strip that fails.
set -u

DEMO_DIR="$(cd "$(dirname "$0")" && pwd)"
ARMS="$(cd "$DEMO_DIR/../../arms" && pwd)"
DECK="$DEMO_DIR/../../bank/v1/deck"
[ -f "$DECK/deck.toml" ] || { echo "FAIL: frozen v1 deck missing at $DECK"; exit 1; }

# The battery's v1 flight is the NEWEST (epochs accumulate under v1/).
V1_RUN_DIR="$(ls -dt "$ARMS"/runs/loop/v1/dr-* 2>/dev/null | head -1)"
[ -n "$V1_RUN_DIR" ] || { echo "FAIL: v1 run dir missing under $ARMS/runs/loop/v1/ (run the battery first)"; exit 1; }

# --- 1. the flight terminated ---------------------------------------
[ -f "$V1_RUN_DIR/report.md" ] || { echo "FAIL: no report.md in $V1_RUN_DIR"; exit 1; }
MANIFEST_TERMINAL="$(python3 -c "import json,sys; m=json.load(open('$V1_RUN_DIR/manifest.json')); print(m.get('terminal_state') or m.get('state') or 'missing')" 2>/dev/null)"
echo "v1 flight: $V1_RUN_DIR (terminal: $MANIFEST_TERMINAL)"

# --- 2-3. claims verdict-stamped; figures attributable --------------
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

flag_pattern = re.compile(r"\[(passed|failed|could-not-judge|never-ran)\]")
missing = []
lines = report.splitlines()
i, n = 0, len(lines)
while i < n:
    line = lines[i]
    m = flag_pattern.search(line)
    if not m:
        i += 1
        continue  # prose (title, run line, section header): no claim, nothing to attribute
    # the claim body: the stamped bullet line plus the renderer's
    # continuation lines (the stamp sits on the bullet; the claim
    # text may continue on the next line(s) until the next bullet,
    # header, or blank line).
    claim_parts = [line.split("]", 1)[1]]
    j = i + 1
    while j < n and not lines[j].startswith("- ") and not lines[j].startswith("#") and lines[j].strip():
        claim_parts.append(lines[j])
        j += 1
    # the citation tail (chunk ids, source names) is not claim content.
    claim_text = "\n".join(claim_parts).split("[Source:", 1)[0]
    for f in sa.NUMERIC_TOKEN.findall(claim_text):
        f_l = f.lower()
        if f_l in window_text:
            continue
        if m.group(1) != "passed":
            continue  # absence named on a flagged claim
        missing.append((f, line[:80]))
    i = j
assert not missing, f"figures in passed claims absent from the evidence window: {missing[:5]}"
print(f"report: {len(stamped)} verdict-stamped claims; all figures attributable or on flagged claims")
PY
[ $? -eq 0 ] || { echo "FAIL: attribution strips (2-3)"; exit 1; }

# --- 3b. the retrieval mechanics are term-ranked (T1.9) --------------
# The demo's claim is "rendered by a loop whose MOCK retrieval is
# term-ranked" — the flight's own artifacts must show, on THIS flight:
#   a. the round-1 fetch list carries DISTINCT relevance scores (the
#      old exact-value instrument's flat 0.9 ties are gone);
#   b. the round-1 queries introduce no VALUE-SHAPED digits beyond the
#      question's own, AND an admitted hit carries value-shaped figure
#      runs in none of those queries — the concept -> value retrieval
#      proof. The query names no bank figure; the value-bearing
#      document still retrieves. (Amendment journaled in the header:
#      era years and generic descriptors are not bank figures.)
python3 - "$V1_RUN_DIR" "$DECK" <<'PY'
import json, pathlib, re, sys, tomllib
run = pathlib.Path(sys.argv[1])
deck = pathlib.Path(sys.argv[2])

raw = tomllib.loads((deck / "deck.toml").read_text())
url2body = {h["url"]: h["body"] for h in raw["hit"]}
bodies = {h["body"]: (deck / h["body"]).read_text() for h in raw["hit"]}

charter = json.loads((run / "charter.json").read_text())
question = charter["question"]

def value_runs(text):
    """VALUE-SHAPED digit runs: 3+ digits, not 4-digit era years
    (19xx/20xx — the draft's sub-questions legitimately name the era),
    not all-zero runs ("per 1,000" -> '000'). Journaled amendment
    2026-08-15: the committed raw-digit test tripped on era years and
    generic descriptors that also occur in the value-bearing bodies;
    the shape test catches 3+ digit value leaks (5469, 325, 476) with
    a 2-digit blind spot (7.87, 9.6) — the no-bank-vocabulary
    guarantee is the fold-in machinery's, not this strip's."""
    out = set()
    for d in re.findall(r"[0-9]+", text):
        if len(d) >= 3 and not (len(d) == 4 and 1900 <= int(d) <= 2099) and set(d) != {"0"}:
            out.add(d)
    return out

fl = json.loads((run / "fetch-list-1.json").read_text())
hits = fl["search_hits"]
scores = [h["score"] for h in hits]
assert len(scores) > 1, "round-1 fetch list carries no search hits to score"
assert len(set(scores)) > 1, \
    f"round-1 scores are all equal ({scores}) — the old flat-tie instrument, not term-ranked"
print(f"round-1 distinct relevance scores: {sorted(set(scores))} (relevance counts, not flat 0.9s)")

triage = fl.get("triage", {})
admitted = set(triage.get("code_set_k", [])) | set(triage.get("eps_admits", []))
assert admitted, "round-1 triage admits nothing — no concept->value retrieval to show"
queries = " ".join(q.get("text", "") for q in fl.get("queries", []))
q_value = value_runs(queries) - value_runs(question)
assert not q_value, f"round-1 queries introduce value-shaped digits the question did not: {sorted(q_value)}"
print(f"round-1 queries carry NO value-shaped digits beyond the question's own")

proof = []
for h in hits:
    if h["id"] not in admitted:
        continue
    body = bodies.get(url2body.get(h["url"], ""), "")
    distinctive = value_runs(body) - value_runs(question)  # the hit's figures the question did NOT supply
    leaked = distinctive & q_value
    if distinctive and not leaked:
        proof.append((h["id"], h["url"], sorted(distinctive)[:6], h["score"]))
assert proof, (
    "no admitted hit carries value-shaped figures in NO round-1 query — "
    "the concept->value retrieval proof is missing"
)
print(f"concept->value retrieval: {len(proof)} admitted hit(s) whose value-shaped figure runs "
      f"appear in NO round-1 query — the query never names the bank's figures")
for p in proof[:3]:
    print("   ", p)
PY
[ $? -eq 0 ] || { echo "FAIL: term-ranked retrieval strip (3b)"; exit 1; }

# --- 4. bars.md is the scorer's numbers ------------------------------
[ -f "$ARMS/score-report-t1f.json" ] || { echo "FAIL: score-report-t1f.json missing (score the battery first)"; exit 1; }
python3 - "$ARMS" <<'PY'
import json, pathlib, sys
arms = pathlib.Path(sys.argv[1])
report = json.load(open(arms / "score-report-t1f.json"))
bars = (arms.parent / "demo/demo5/bars.md").read_text()
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
report = json.load(open(arms / "score-report-t1f.json"))
bars = (arms.parent / "demo/demo5/bars.md").read_text()
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

echo "=== DEMO-5 verify: all strips pass ==="
