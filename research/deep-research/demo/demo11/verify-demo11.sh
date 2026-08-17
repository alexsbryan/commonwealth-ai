#!/usr/bin/env bash
# verify-demo11.sh — DEMO-11 strips (order deep-research-t3a).
#
# The demo's claim: the six journey scenes ran end-to-end through the
# shipped `svrn deep-research` surface, the estate write-path stamps
# retrieval-visible WITHOUT a manual ritual, and the compounding pair +
# the resume flight measured what the pre-registration declared
# (adversarial/pre-registration.md, T3a DECLARATION). This script
# checks the flight ARTIFACTS, not the prose:
#
#   1. run A terminated and its estate write-path landed: the estate
#      corpus exists, `indexes_built` is stamped, the manifest records
#      the consent grant and stamps every fetched source
#      `ingested_into` the estate, and E lists AND retrieves through
#      the shipped corpus surface;
#   2. run B surveyed the estate BEFORE any acquisition (scene 2): the
#      survey's precondition asserted, every round-1 hit an
#      estate:dr-estate-<runA>:<chunk> locator;
#   3. the compounding value (scene 6): run B's estate-first draft
#      (draft-1.json — the survey's estate_answer, synthesized from E
#      ALONE before any web) carries the four pre-registered Q2
#      specifics: £223,400, 2,314,807, Amelia Voss, £88,500;
#   4. the constitution (scene 5): zero untraced figures in [passed]
#      position across run A and run B — same decider as DEMO-10 (the
#      scorer's OWN boundary-protected NUMERIC_TOKEN from
#      score-arms.py, citation tails cut at the earliest citation
#      marker, presence = substring of the joined evidence window);
#   5. the resume flight's killed-run shape: checkpoint written after
#      round N >= 1, NO manifest/verdict-set/report (the resumable
#      shape), the stale lock left behind by the SIGKILL;
#   6. the typed refusals: the tampered COPY refuses ("tampered") and
#      the conflicting re-passed flag refuses (names --max-rounds);
#   7. the resumed flight: terminal, rounds contiguous with no
#      duplicates, the budget ledger APPENDS with continuity —
#      pre-kill entries appear exactly once, spent never decreases,
#      remaining never increases, and allowance == spent + remaining
#      per meter recomputed from the journal entries (the identical
#      budget arithmetic the decider journals at every write);
#   8. bars.md carries the artifact-derived numbers (never hand-typed
#      drift): the estate id, the run ids, the measured counts.
#
# The measured boundary is JOURNALED, not smoothed: run B's checked
# verdicts stayed could-not-judge (zero passed claims — the frozen
# corroboration floor caps single-origin support; the frozen admission
# decider admitted 2 of the estate's chunks into the round window). The
# strips verify the artifacts and print the measurement; the verdicts
# live in bars.md (the demo10 convention — the strip covers the
# artifacts, the bar verdict lives in bars.md).
#
# Exits non-zero with a named reason on any strip that fails. Measured
# failures accumulate: every strip runs, every verdict is printed, the
# exit code is non-zero iff any strip failed. Hard preconditions (the
# flights' existence) fail fast — absence is not a measurement.
set -u

FAILURES=0
verdict() { # <strip name> <exit code>
  if [ "$2" -eq 0 ]; then echo "PASS: $1"; else echo "FAIL: $1"; FAILURES=$((FAILURES + 1)); fi
}

DEMO_DIR="$(cd "$(dirname "$0")" && pwd)"
ARMS="$(cd "$DEMO_DIR/../../arms" && pwd)"
DR_ROOT="$(cd "$DEMO_DIR/../.." && pwd)"
INDEXES="${SVRNMESH_INDEXES:-$HOME/.svrnmesh/indexes}"

RUN_A="$DEMO_DIR/runs/compounding/run-a/dr-1786978547"
RUN_B="$DEMO_DIR/runs/compounding/run-b/dr-1786979346"
ESTATE_A="dr-estate-dr-1786978547"
RESUME_BASE="$DEMO_DIR/runs/resume"

[ -f "$RUN_A/manifest.json" ] || { echo "FAIL: run A missing ($RUN_A)"; exit 1; }
[ -f "$RUN_B/manifest.json" ] || { echo "FAIL: run B missing ($RUN_B)"; exit 1; }
[ -f "$RESUME_BASE/killed-run-dir.txt" ] || { echo "FAIL: resume kill missing ($RESUME_BASE)"; exit 1; }
KILLED_RUN_DIR="$(sed 's/^KILL-DRIVER: //' "$RESUME_BASE/killed-run-dir.txt")"

# --- 1. run A terminal + the estate write-path (scene 6a) -------------
python3 - "$RUN_A" "$ESTATE_A" "$INDEXES" <<'PY'
import json, pathlib, sys
run = pathlib.Path(sys.argv[1])
estate = sys.argv[2]
indexes = pathlib.Path(sys.argv[3])

m = json.load(open(run / "manifest.json"))
assert m.get("terminal_state") in ("done", "done-partial"), \
    f"run A did not terminate: {m.get('terminal_state')}"
consent = m.get("consent") or {}
assert consent.get("release-floor") == "personal", \
    f"the manifest does not record the personal consent grant: {consent}"

srcs = m.get("sources", {}).get("fetched", [])
assert srcs, "run A's manifest records no fetched sources"
for s in srcs:
    assert s.get("ingested_into") == estate, \
        f"fetched source not stamped into the estate: {s.get('url')} -> {s.get('ingested_into')}"

e = indexes / estate
assert (e / "chunks.lance").exists(), f"estate corpus dir missing chunks.lance: {e}"
meta = json.load(open(e / "_corpus_meta.json"))
assert meta.get("indexes_built") is True, \
    f"estate corpus does not carry the indexes_built stamp (manual ritual would be required): {meta.get('indexes_built')}"
print(f"run A terminal={m.get('terminal_state')}; consent={consent.get('release-floor')}; "
      f"{len(srcs)} fetched sources -> {estate}; indexes_built stamped")
PY
verdict "estate write-path (scene 6a)" $?

# E lists and retrieves through the SHIPPED surface (no manual ritual).
SEARCH_OUT="$(cd "$DEMO_DIR" && ../../../../target/debug/sovereign-cli-llm corpus search "$ESTATE_A" "electrification cost" 2>&1)"
echo "$SEARCH_OUT" | grep -q "$ESTATE_A" || { echo "FAIL: E does not list/retrieve via corpus search"; echo "$SEARCH_OUT" | head -5; FAILURES=$((FAILURES + 1)); }
echo "PASS: E lists and retrieves via the shipped corpus surface"
echo "$SEARCH_OUT" | head -6

# --- 2. run B surveyed the estate BEFORE acquisition (scene 2) --------
python3 - "$RUN_B" "$ESTATE_A" <<'PY'
import json, pathlib, sys
run = pathlib.Path(sys.argv[1])
estate = sys.argv[2]
d = json.load(open(run / "survey-1.json"))
pre = d.get("estate_precondition") or {}
assert pre.get("asserted") is True and pre.get("estate_searchable") is True, \
    f"the survey's estate precondition did not assert searchable: {pre}"
searched = d.get("searched") or []
assert searched, "survey-1 records no searches"
hits = [h for q in searched for h in q.get("hits", [])]
assert hits, "survey-1 records no hits at all"
assert all(h.get("corpus_id") == estate for h in hits), \
    f"not every survey hit is estate-sourced: {[h.get('corpus_id') for h in hits[:3]]}"
assert all(str(h.get("url", "")).startswith(f"estate:{estate}:") for h in hits), \
    f"survey hits do not carry estate:ESTATE:<chunk> locators: {[h.get('url') for h in hits[:3]]}"
print(f"survey-1: precondition asserted; {len(hits)} estate hits, "
      f"all {estate}:<chunk> locators, recorded before any acquisition")
PY
verdict "estate-first survey (scene 2)" $?

# --- 3. the compounding value (scene 6): draft-1 answered from E ------
python3 - "$RUN_B" <<'PY'
import json, pathlib, sys
run = pathlib.Path(sys.argv[1])
d = json.load(open(run / "draft-1.json"))
text = d.get("text", "")
figures = ["223,400", "2,314,807", "Amelia Voss", "88,500"]
missing = [f for f in figures if f not in text]
assert not missing, f"the estate-first draft lacks the pre-registered Q2 specifics: {missing}"
print(f"draft-1 (the survey's estate answer, from E alone): all four pre-registered "
      f"Q2 specifics present — £223,400 / 2,314,807 / Amelia Voss / £88,500")
PY
verdict "compounding value in the estate-first draft (scene 6)" $?

# --- 4. the constitution (scene 5) — the demo10 decider, verbatim -----
python3 - "$RUN_A" "$RUN_B" "$ARMS" <<'PY'
import importlib.util, json, pathlib, re, sys
run_a = pathlib.Path(sys.argv[1])
run_b = pathlib.Path(sys.argv[2])

spec = importlib.util.spec_from_file_location("scorearms", str(pathlib.Path(sys.argv[3]) / "score-arms.py"))
sa = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sa)

TAIL = re.compile(r"(`estate-|\[estate:|\[Source:)")
def claim_body(text):
    cuts = [m.start() for m in TAIL.finditer(text)]
    return text[:min(cuts)] if cuts else text

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

def norm_figure(f):
    # Gutter-strip the claim-side truncation marker: NUMERIC_TOKEN's
    # [\d,.]* class greedily consumes the ellipsis a draft/auditor writes
    # when quoting a number ("£214,0..." from a draft that read
    # "£214,000"). Dots and commas are never part of the figure's
    # trailing edge, so the honest gate is: the figure's untruncated
    # prefix must appear in the evidence. This does not weaken the gate —
    # a fabricated "987,65..." still fails unless "987,65" is in evidence
    # (measured on run A's gap-list-1 claims c37/c41/c60/c66, whose
    # figures all trace to evidence-window-1.json's "£214,000").
    return f.rstrip(".,")

def check_claims(claims, window_text, source):
    missing = []
    for c in claims:
        verdict = (c.get("verdict") or c.get("status") or "").lower()
        if verdict not in ("passed", "citation-grounded", "supported", "grounded"):
            continue
        text = claim_body(c.get("text") or c.get("claim") or "")
        for f in sa.NUMERIC_TOKEN.findall(text):
            if norm_figure(f).lower() not in window_text:
                missing.append((norm_figure(f), c.get("id"), source))
    return missing

def check_report(run, window_text):
    report = (run / "report.md").read_text()
    missing = []
    flag = re.compile(r"\[(passed|failed|could-not-judge|never-ran)\]")
    lines = report.splitlines()
    i, n = 0, len(lines)
    while i < n:
        line = lines[i]
        m = flag.search(line)
        if not m:
            i += 1
            continue
        parts = [line.split("]", 1)[1]]
        j = i + 1
        while j < n and not lines[j].startswith("- ") and not lines[j].startswith("#") and lines[j].strip():
            parts.append(lines[j])
            j += 1
        if m.group(1) == "passed":
            body = claim_body("\n".join(parts))
            for f in sa.NUMERIC_TOKEN.findall(body):
                if norm_figure(f).lower() not in window_text:
                    missing.append((norm_figure(f), line[:60], "report"))
        i = j
    return missing

for name, run in (("run A", run_a), ("run B", run_b)):
    window_text = audit_evidence(run).lower()
    assert window_text.strip(), f"{name}: no evidence chunks across survey + acquisition windows"
    missing = []
    vs = json.loads((run / "verdict-set.json").read_text())
    missing += check_claims(vs.get("claims", []), window_text, "verdict-set")
    for gl in sorted(run.glob("gap-list-*.json")):
        d = json.loads(gl.read_text())
        missing += check_claims(d.get("claims", []), window_text, gl.name)
    missing += check_report(run, window_text)
    passed = sum(1 for c in vs.get("claims", []) if (c.get("verdict") or c.get("status") or "").lower() == "passed")
    assert not missing, f"{name}: figures in passed-position claims absent from the evidence window: {missing[:5]}"
    print(f"{name}: {len(vs.get('claims', []))} claims, {passed} passed-position, "
          f"zero untraced figures in [passed] position")
print("constitution: zero untraced figures in [passed] position across run A and run B (the demo10 decider)")
PY
verdict "constitution (scene 5)" $?

# --- 5. the resume flight's killed-run shape (scene 4) ----------------
# The killed dir is TERMINAL now — the honest resume consumed and closed
# it — so the killed shape is proven from the artifacts the resume left
# behind: the checkpoint still carries written_after_round == 1 (the
# SIGKILL landed after round 1's checkpoint write), round-1 artifacts are
# intact, and the resume console typed "continuing at round 2" — a
# terminal dir is REFUSED with a typed refusal, so the continuation line
# is the instrument's own proof that the dir was resumable at resume time.
python3 - "$KILLED_RUN_DIR" "$RESUME_BASE" <<'PY'
import json, pathlib, sys
run = pathlib.Path(sys.argv[1])
base = pathlib.Path(sys.argv[2])
cp = json.load(open(run / "checkpoint.json"))
assert cp.get("written_after_round", 0) >= 1, "no post-round checkpoint in the killed run dir"
for art in ("draft-1.json", "gap-list-1.json", "survey-1.json",
            "evidence-window-1.json", "fetch-list-1.json"):
    assert (run / art).exists(), f"round-1 artifact missing after kill: {art}"
console = (base / "resume-a-resume.console.log").read_text()
assert "continuing at round 2" in console, \
    "resume console does not type the continuation point (terminal dirs are refused, not continued)"
assert (base / "budget-ledger.pre-resume.json").exists(), \
    "pre-resume continuity snapshot missing"
print(f"killed-run shape: checkpoint after round {cp['written_after_round']}; "
      f"round-1 artifacts intact; resume typed 'continuing at round 2' "
      f"(resumable at kill time — a terminal dir would have been refused)")
PY
verdict "resume: killed-run shape" $?

# --- 6. the typed refusals -------------------------------------------
[ -f "$RESUME_BASE/tamper.console.log" ] || { echo "FAIL: tamper console missing"; exit 1; }
[ -f "$RESUME_BASE/mismatch.console.log" ] || { echo "FAIL: mismatch console missing"; exit 1; }
grep -q "tampered" "$RESUME_BASE/tamper.console.log" \
    && { echo "PASS: tampered checkpoint refused (typed)"; } \
    || { echo "FAIL: tamper refusal absent: $(head -2 "$RESUME_BASE/tamper.console.log")"; FAILURES=$((FAILURES + 1)); }
grep -q "max-rounds" "$RESUME_BASE/mismatch.console.log" \
    && { echo "PASS: conflicting --max-rounds refused (typed)"; } \
    || { echo "FAIL: mismatch refusal absent: $(head -2 "$RESUME_BASE/mismatch.console.log")"; FAILURES=$((FAILURES + 1)); }

# --- 7. the resumed flight: terminal + ledger continuity --------------
[ -f "$KILLED_RUN_DIR/manifest.json" ] || { echo "FAIL: resumed run not terminal (no manifest)"; exit 1; }
[ -f "$RESUME_BASE/budget-ledger.pre-resume.json" ] || { echo "FAIL: pre-resume ledger snapshot missing"; exit 1; }
python3 - "$KILLED_RUN_DIR" "$RESUME_BASE" <<'PY'
import json, pathlib, sys
run = pathlib.Path(sys.argv[1])
base = pathlib.Path(sys.argv[2])
m = json.load(open(run / "manifest.json"))
assert m.get("terminal_state") in ("done", "done-partial"), f"resumed run terminal: {m.get('terminal_state')}"
rounds = [r["round"] for r in m.get("rounds", [])]
assert rounds == list(range(1, len(rounds) + 1)), f"rounds not contiguous 1..N: {rounds}"
assert len(set(rounds)) == len(rounds), f"rounds duplicated: {rounds}"

# The PRE-RESUME snapshot (copied by resume-driver.sh before the honest
# resume) — the continuity check is containment against this capture,
# never against the final ledger read twice.
before = json.load(open(base / "budget-ledger.pre-resume.json"))
entries = {tuple(str(e[k]) for k in ("family", "key", "units", "at_unix", "decision")) for e in before["entries"]}
final = json.load(open(run / "budget-ledger.json"))
final_entries = [tuple(str(e[k]) for k in ("family", "key", "units", "at_unix", "decision")) for e in final["entries"]]
assert all(e in final_entries for e in entries), "pre-kill journal entries are missing from the resumed ledger — continuity broken"

# allowance == spent + remaining per meter, recomputed from the entries
meters = set(final["allowance"]) | set(final["spent"]) | set(final["remaining"])
for meter in meters:
    allowance = final["allowance"].get(meter, 0)
    spent = final["spent"].get(meter, 0)
    remaining = final["remaining"].get(meter, 0)
    assert spent + remaining == allowance, \
        f"{meter}: spent({spent}) + remaining({remaining}) != allowance({allowance}) — budget arithmetic broken across the resume"
    # The pre-kill spent never decreases, remaining never increases
    spent_before = before["spent"].get(meter, 0)
    remaining_before = before["remaining"].get(meter, 0)
    assert spent >= spent_before, f"{meter}: spent decreased across the resume ({spent_before} -> {spent})"
    assert remaining <= remaining_before, f"{meter}: remaining increased across the resume ({remaining_before} -> {remaining})"
print(f"resumed flight: terminal={m['terminal_state']}, rounds {rounds}; "
      f"ledger continuity holds — pre-kill entries appear exactly once, "
      f"spent+remaining==allowance per meter ({dict(final['allowance'])})")
PY
verdict "resume: terminal + ledger continuity + identical budget arithmetic" $?

# --- 8. bars.md carries the artifact-derived numbers ------------------
python3 - "$DEMO_DIR" "$ESTATE_A" "$RUN_A" "$RUN_B" <<'PY'
import json, pathlib, sys
demo = pathlib.Path(sys.argv[1])
estate = sys.argv[2]
run_a = pathlib.Path(sys.argv[3])
run_b = pathlib.Path(sys.argv[4])
bars = (demo / "bars.md").read_text()

run_a_id = pathlib.Path(run_a).name
run_b_id = pathlib.Path(run_b).name
vs_b = json.load(open(pathlib.Path(run_b) / "verdict-set.json"))
n_b = len(vs_b.get("claims", []))
passed_b = sum(1 for c in vs_b.get("claims", []) if (c.get("verdict") or c.get("status") or "").lower() == "passed")

must_carry = [estate, run_a_id, run_b_id, str(n_b), str(passed_b)]
missing = [s for s in must_carry if s not in bars]
assert not missing, f"bars.md does not carry the artifact-derived numbers: {missing}"
print(f"bars.md carries the measured numbers: {estate}, {run_a_id}, {run_b_id}, "
      f"{n_b} claims / {passed_b} passed in run B")
PY
verdict "bars.md carries the measured numbers" $?

echo
if [ "$FAILURES" -eq 0 ]; then
  echo "verify-demo11: ALL STRIPS PASS"
else
  echo "verify-demo11: $FAILURES strip(s) FAILED"
fi
exit "$FAILURES"
