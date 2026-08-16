#!/usr/bin/env bash
# verify-demo6.sh — DEMO-6 corpus-source strips (order deep-research-t1g).
#
# The demo's claim: the v1 report-class question is rendered by a loop
# whose acquisition searches the ESTATE — the dr-demo6-v1 corpus built
# once from the FROZEN v1 deck bodies (svrn corpus ingest of the verbatim
# copy under demo/demo6/deck-extract/) — a CONCEPT query retrieves the
# value-bearing chunk through the corpus's own retrieval surface
# (CorpusIndex::search — vector+FTS hybrid) without the loop ever naming
# the bank's figures, and the report is every-number-attributable. This
# script checks the flight artifacts rather than the prose:
#
#   1. the v1 corpus flight exists (the battery's v1 run dir) and
#      terminated;
#   2. every claim in the report is verdict-stamped;
#   3. every FIGURE token in the report's [passed] claims is attributable
#      to the run's accumulated evidence window (a flagged claim's
#      absence is named by its stamp — never enforced, never removed).
#      Same decider as DEMO-5: the scorer's OWN boundary-protected
#      tokenizer (NUMERIC_TOKEN, loaded from score-arms.py — one
#      decider, one implementation, §10.6), the citation tail cut at
#      the first citation marker of either renderer, presence =
#      substring of the joined window text, the claim body joined
#      across the renderer's bullet+continuation lines.
#
#      Amendments (t1g, watched-fail -> fix, demo5 precedent):
#      A. the tail cut was "[Source:" only — the corpus renderer cites
#         with estate markdown links and backticked source refs
#         ("`estate-1` [estate:dr-demo6-v1:<chunk>](...)"), so the
#         chunk id inside the citation leaked into the claim body and
#         tokenized as a figure ("64" x3, measured on the v1 corpus
#         flight). The cut now takes the EARLIEST citation marker
#         ("`estate-", "[estate:", "[Source:") — chunk ids and source
#         refs are citation machinery, not claim content.
#      B. after A, the [passed] claim's era years ("1980", "2024" — the
#         question's own framing restated) trace NOWHERE. The decider
#         has no year exemption (its density row for this claim is
#         traces=false, nums_in_window=[] — score-report-t1g.json), so
#         the strip has none either: this flight VIOLATES the
#         passed-position honesty property and the strip FAILS, naming
#         the years. The failure is the measurement — the honesty leg
#         failed on both the letter and the passed-position property —
#         it is named, never exempted, never silenced.
#   3b. the acquisition source on THIS flight is the corpus (the
#      instrument change, pre-registered in pre-registration.md before
#      the re-measure):
#      a. every round-1 search hit is stamped engine "corpus" (the
#         source dispatch records itself — glassbox; zero mock-deck
#         hits on a corpus-source flight);
#      b. the admitted hits' locators are chunk-level estate locators —
#         `estate:dr-demo6-v1:<chunk_id>`, exactly two colons — and
#         the window chunks carry custody "personal" (the estate's
#         stamp, never re-stamped public-web);
#      c. the round-1 queries introduce no VALUE-SHAPED digit runs
#         beyond the question's own (the shape test from DEMO-5's
#         strip 3b, journaled: 3+ digits, not 4-digit 19xx/20xx era
#         years, not all-zero runs), AND an admitted chunk's content
#         carries value-shaped figure runs in none of those queries —
#         the concept -> value retrieval proof, through the corpus.
#   4. bars.md carries the scorer's per-question fractions and bar legs
#      verbatim (score-report-t1g.json) — never hand-typed;
#   5. the two-arm lift is the same scorer's, over the same pairs.
#
# Exits non-zero with a named reason on any strip that fails.
#
#      C. Amendment C (t1g): strips 3 and 3b carry MEASURED failures by
#         design on this flight (the passed-position violation; the
#         triage boundary's dead concept->value half — see README). A
#         fail-fast gate would report only the FIRST designed failure.
#         The verdicts accumulate: every strip runs, every verdict is
#         printed, the exit code is non-zero iff any strip failed.
#         Hard preconditions (deck-extract, the flight's existence)
#         still fail fast — absence is not a measurement.
set -u

FAILURES=0
verdict() { # <strip name> <exit code>
  if [ "$2" -eq 0 ]; then echo "PASS: $1"; else echo "FAIL: $1"; FAILURES=$((FAILURES + 1)); fi
}

DEMO_DIR="$(cd "$(dirname "$0")" && pwd)"
ARMS="$(cd "$DEMO_DIR/../../arms" && pwd)"
CORPUS_ID="dr-demo6-v1"
[ -d "$DEMO_DIR/deck-extract" ] || { echo "FAIL: verbatim deck-extract missing under $DEMO_DIR/deck-extract"; exit 1; }

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
    # Amendment A: cut at the EARLIEST citation marker of either
    # renderer — the mock renderer's "[Source:" and the corpus
    # renderer's "`estate-" backticked refs + "[estate:" markdown
    # links (a chunk id inside the tail tokenizes as a figure).
    claim_body = "\n".join(claim_parts)
    cuts = [p for m in ("`estate-", "[estate:", "[Source:") if (p := claim_body.find(m)) >= 0]
    claim_text = claim_body[:min(cuts)] if cuts else claim_body
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
verdict "attribution strips (2-3)" $?

# --- 3b. the acquisition source is the corpus (t1g rung 2) -----------
# The demo's claim is "the loop searched the ESTATE" — the flight's own
# artifacts must show, on THIS flight:
#   a. every round-1 search hit is engine "corpus";
#   b. the admitted hits' locators are chunk-level estate locators and
#      the window custody is "personal";
#   c. the round-1 queries introduce no VALUE-SHAPED digits beyond the
#      question's own, AND an admitted chunk's content carries
#      value-shaped figure runs in none of those queries — the
#      concept -> value retrieval proof through the corpus surface.
python3 - "$V1_RUN_DIR" <<'PY'
import json, pathlib, re, sys
run = pathlib.Path(sys.argv[1])

charter = json.loads((run / "charter.json").read_text())
question = charter["question"]

fl = json.loads((run / "fetch-list-1.json").read_text())
hits = fl["search_hits"]
assert hits, "round-1 fetch list carries no search hits — the corpus source retrieved nothing"
engines = {h.get("engine") for h in hits}
assert engines == {"corpus"}, \
    f"round-1 search hits are not all corpus-sourced: engines {engines}"
print(f"round-1 search hits: {len(hits)}, engine=corpus on every hit")

def value_runs(text):
    """VALUE-SHAPED digit runs: 3+ digits, not 4-digit era years
    (19xx/20xx — the draft's sub-questions legitimately name the era),
    not all-zero runs ("per 1,000" -> '000'). The DEMO-5 journaled
    calibration (pre-registration.md): the shape test catches 3+ digit
    value leaks (5469, 325, 476) with a 2-digit blind spot (7.87, 9.6)
    — the no-bank-vocabulary guarantee is the fold-in machinery's, not
    this strip's."""
    out = set()
    for d in re.findall(r"[0-9]+", text):
        if len(d) >= 3 and not (len(d) == 4 and 1900 <= int(d) <= 2099) and set(d) != {"0"}:
            out.add(d)
    return out

triage = fl.get("triage", {})
admitted = set(triage.get("code_set_k", [])) | set(triage.get("eps_admits", []))
assert admitted, "round-1 triage admits nothing — no concept->value retrieval to show"
queries = " ".join(q.get("text", "") for q in fl.get("queries", []))
q_value = value_runs(queries) - value_runs(question)
assert not q_value, f"round-1 queries introduce value-shaped digits the question did not: {sorted(q_value)}"
print("round-1 queries carry NO value-shaped digits beyond the question's own")

# b. chunk-level estate locators on the admitted hits + the window.
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

# c. concept -> value through the corpus: an admitted chunk's content
# carries value-shaped runs in none of the round-1 queries.
admitted_hits = [h for h in hits if h["id"] in admitted]
contents = {c.get("locator", ""): c.get("content", "") for c in window_chunks}
proof = []
for h in admitted_hits:
    content = contents.get(h.get("url", ""), "")
    distinctive = value_runs(content) - value_runs(question)
    leaked = distinctive & q_value
    if distinctive and not leaked:
        proof.append((h["id"], h["url"], sorted(distinctive)[:6], h["score"]))
assert proof, (
    "no admitted corpus chunk carries value-shaped figures in NO round-1 query — "
    "the concept->value retrieval proof through the corpus is missing"
)
print(f"concept->value through the corpus: {len(proof)} admitted chunk(s) whose value-shaped "
      f"figure runs appear in NO round-1 query — the query never names the bank's figures")
for p in proof[:3]:
    print("   ", p)
PY
verdict "corpus-source strip (3b)" $?

# --- 4. bars.md is the scorer's numbers ------------------------------
[ -f "$ARMS/score-report-t1g.json" ] || { echo "FAIL: score-report-t1g.json missing (score the battery first)"; exit 1; }
python3 - "$ARMS" <<'PY'
import json, pathlib, sys
arms = pathlib.Path(sys.argv[1])
report = json.load(open(arms / "score-report-t1g.json"))
bars = (arms.parent / "demo/demo6/bars.md").read_text()
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
report = json.load(open(arms / "score-report-t1g.json"))
bars = (arms.parent / "demo/demo6/bars.md").read_text()
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
  echo "=== DEMO-6 verify: $FAILURES strip(s) FAILED — the failures are the measurements (named above) ==="
  exit 1
fi
echo "=== DEMO-6 verify: all strips pass ==="
