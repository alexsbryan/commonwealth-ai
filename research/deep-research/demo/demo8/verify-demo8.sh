#!/usr/bin/env bash
# verify-demo8.sh — DEMO-8 egress-boundary strips (order deep-research-t2a).
#
# The demo's claim: the loop's rung-3 WEB leg is gated by ONE boundary —
# every egress decision (released or refused) is traced at debug, the
# refusal is typed and names what was withheld (default-deny without a
# consent grant), the grant is run-scoped and frozen into the charter +
# recorded in the manifest, every web-fetched chunk carries public-web
# custody, and the ONE SpendDecider journals every spend decision to an
# ICD ledger. This script checks the FLIGHT ARTIFACTS rather than the
# prose:
#
#   1. the refusal case: exit code 1, the typed default-deny refusal in
#      the transcript (what was withheld, the absent grant), the single
#      journaled attempt in the refusal ledger;
#   2. the consent case: the manifest's terminal state + the consent
#      grant record + every fetched source's public-web custody stamp +
#      the budget totals;
#   3. the consent case: the frozen charter carries the SAME grant
#      (run id + release floor) as the manifest, plus the custody
#      policy (stamp_required, unknown_refuses) and the budget
#      allowance — the run-scoped grant, frozen at launch;
#   4. the consent case: the egress trace — exactly 4 query releases
#      under the run grant (run id matching the manifest's) and 4
#      public-web url releases, zero refusals in the consent trace;
#   5. the consent case: every claim in the report is verdict-stamped
#      (a claim with no verdict is a silent number);
#   6. the consent case: the budget ledger journals 8 allow decisions
#      across both families and lands at exactly 0 remaining — the ONE
#      decider spent exactly the allowance;
#   7. bars.md carries the two met transitions (dr-egress,
#      dr-budget-one-decider) verbatim from quality/initiative-bars.toml
#      — never hand-typed.
#
# Optional live re-verification (requires the daemon + the rebuilt
# binary): DR8_REFUSAL_LIVE=1 re-runs the refusal case and checks exit
# 1 + the typed refusal; DR8_RUN_DIR=<run dir> checks the raw run
# artifacts (window chunks' public-web custody, hits engine=web).
#
# Exits non-zero with a named reason on any strip that fails.
set -u

FAILURES=0
verdict() { # <strip name> <exit code>
  if [ "$2" -eq 0 ]; then echo "PASS: $1"; else echo "FAIL: $1"; FAILURES=$((FAILURES + 1)); fi
}

DEMO_DIR="$(cd "$(dirname "$0")" && pwd)"
BARS_TOML="$(cd "$DEMO_DIR/../../../.." && pwd)/quality/initiative-bars.toml"
[ -f "$BARS_TOML" ] || { echo "FAIL: $BARS_TOML missing"; exit 1; }

MANIFEST="$DEMO_DIR/manifest.json"
CHARTER="$DEMO_DIR/charter.json"
TRACE="$DEMO_DIR/egress-trace.log"
REPORT="$DEMO_DIR/report-web.md"
REFUSAL_LOG="$DEMO_DIR/refusal-transcript.log"
REFUSAL_EXIT="$DEMO_DIR/refusal-exit.txt"
CONSENT_LEDGER="$DEMO_DIR/consent-budget-ledger.json"
REFUSAL_LEDGER="$DEMO_DIR/refusal-budget-ledger.json"

# --- 1. the refusal case --------------------------------------------
[ -f "$REFUSAL_EXIT" ] || { echo "FAIL: refusal-exit.txt missing"; exit 1; }
[ "$(cat "$REFUSAL_EXIT")" = "1" ] || { echo "FAIL: refusal exit code is not 1"; exit 1; }
python3 - "$REFUSAL_LOG" <<'PY'
import re, sys
log = open(sys.argv[1]).read()
# The typed refusal names what was withheld: the payload class, the
# destination, the absent grant, the default-deny posture.
for needle in [
    "egress refused: query with personal custody to tavily",
    "no run consent grant",
    "default-deny",
    "grant_present=false",
    "run failed",
]:
    assert needle in log, f"refusal transcript lacks {needle!r}"
print("refusal: exit 1, typed refusal naming what was withheld, default-deny")
PY
verdict "refusal case (1)" $?
python3 - "$REFUSAL_LEDGER" <<'PY'
import json, sys
l = json.load(open(sys.argv[1]))
entries = l["entries"]
assert len(entries) == 1, f"refusal ledger journals {len(entries)} decisions, expected 1 attempt"
e = entries[0]
assert e["family"] == "web-search" and e["key"] == "web", f"unexpected attempt: {e}"
assert e["decision"] == "allow", f"the attempt should be journaled as the consumed allowance: {e}"
assert l["remaining"].get("web-search:web") == 3, f"remaining {l['remaining']}"
print("refusal ledger: the single attempted spend journaled before the refusal (allowance consumed by the attempt)")
PY
verdict "refusal ledger (1b)" $?

# --- 2-3. the consent case: manifest + charter -----------------------
python3 - "$MANIFEST" "$CHARTER" <<'PY'
import json, sys
m = json.load(open(sys.argv[1]))
c = json.load(open(sys.argv[2]))
assert m["terminal_state"] in ("done", "done-partial"), f"terminal {m['terminal_state']}"
consent = m.get("consent")
assert consent, "manifest carries no consent grant record"
run_id = consent.get("run-id")
assert run_id and run_id == m["run_id"], f"consent run id {run_id} != manifest run id {m['run_id']}"
assert consent.get("release-floor") == "personal", f"release floor {consent.get('release-floor')}"
assert m["sources"]["fetched"], "no fetched sources"
custodies = {s["custody"] for s in m["sources"]["fetched"]}
assert custodies == {"public-web"}, f"fetched sources not all public-web: {custodies}"
# The charter froze the SAME grant at launch (FR-3), plus the custody
# policy and the budget allowance.
ch = c["charter"]
ch_consent = ch.get("consent")
assert ch_consent, "the frozen charter carries no consent grant"
assert ch_consent["run-id"] == run_id and ch_consent["release-floor"] == "personal", \
    f"charter consent {ch_consent} != manifest consent {consent}"
assert ch["custody"]["stamp_required"] is True and ch["custody"]["unknown_refuses"] is True, \
    f"custody policy not frozen: {ch['custody']}"
assert ch["budget"]["web_search_queries"] == 4 and ch["budget"]["web_fetch_pages"] == 4, \
    f"budget allowance not frozen: {ch['budget']}"
print(f"manifest: terminal {m['terminal_state']}; consent {consent}; "
      f"fetched custody {custodies}; charter froze the same grant + custody policy + allowance")
PY
verdict "manifest + charter consent (2-3)" $?

# --- 4. the egress trace ---------------------------------------------
python3 - "$TRACE" "$MANIFEST" <<'PY'
import json, re, sys
trace = open(sys.argv[1]).read()
m = json.load(open(sys.argv[2]))
run_id = m["consent"]["run-id"]
releases = re.findall(r"egress released — run consent grant (.*)", trace)
assert len(releases) == 4, f"{len(releases)} grant releases, expected 4"
for line in releases:
    assert f"run={run_id}" in line, f"grant release not under the run's grant: {line}"
    assert "release_floor=personal" in line, f"release floor missing: {line}"
    assert 'what="query"' in line and "custody=personal" in line, f"query payload class missing: {line}"
urls = re.findall(r"egress released — public-web custody (.*)", trace)
assert len(urls) == 4, f"{len(urls)} public-web releases, expected 4"
for line in urls:
    assert 'what="url"' in line and "custody=public-web" in line, f"url payload class missing: {line}"
assert "egress refused" not in trace, "the consent trace contains refusals"
print(f"egress trace: 4 query releases under run {run_id} (floor personal) + 4 public-web url releases; 0 refusals")
PY
verdict "egress trace (4)" $?

# --- 5. report verdict stamps ----------------------------------------
python3 - "$REPORT" <<'PY'
import re, sys
report = open(sys.argv[1]).read()
stamped = re.findall(r"\[(passed|failed|could-not-judge|never-ran)\]", report)
assert stamped, "no verdict-stamped claims — a claim with no verdict is a silent number"
print(f"report: {len(stamped)} verdict-stamped claims ({stamped.count('passed')} passed, "
      f"{stamped.count('could-not-judge')} could-not-judge)")
PY
verdict "report verdict stamps (5)" $?

# --- 6. the ONE decider's ledger -------------------------------------
python3 - "$CONSENT_LEDGER" <<'PY'
import json, sys
l = json.load(open(sys.argv[1]))
assert l["icd"] == "budget_ledger", f"not the budget-ledger ICD: {l['icd']}"
entries = l["entries"]
assert len(entries) == 8, f"{len(entries)} journaled decisions, expected 8"
assert all(e["decision"] == "allow" for e in entries), "a non-allow decision in the consent ledger"
families = sorted({e["family"] for e in entries})
assert families == ["web-fetch", "web-search"], f"families {families}"
assert l["remaining"] == {"web-search:web": 0, "web-fetch:pages": 0}, f"remaining {l['remaining']}"
print("consent ledger: 8 allow decisions journaled before each spend, both families, 0 remaining")
PY
verdict "one-decider ledger (6)" $?

# --- 7. bars.md carries the met transitions verbatim ------------------
python3 - "$BARS_TOML" "$DEMO_DIR" <<'PY'
import re, sys
toml = open(sys.argv[1]).read()
demo = sys.argv[2]
bars = open(f"{demo}/bars.md").read()
def met_transition(bar_id):
    m = re.search(r'\nid = "' + bar_id + r'"([\s\S]*?)(?=\n\[\[initiative\.bar\]\]|\Z)', toml)
    block = m.group(1)
    ts = re.findall(r'  \[\[initiative\.bar\.transition\]\][\s\S]*?(?=\n  \[\[initiative\.bar\.transition\]\]|\n\[\[initiative\.bar\]\]|\Z)', block)
    for t in ts:
        if 'to = "met"' in t:
            return t.rstrip()
    raise SystemExit(f"{bar_id}: no met transition in {sys.argv[1]}")
for bar_id in ("dr-egress", "dr-budget-one-decider"):
    t = met_transition(bar_id)
    assert t in bars, f"bars.md does not carry the {bar_id} met transition verbatim"
    assert 'on = "2026-08-16"' in t, f"{bar_id} met transition is not dated 2026-08-16"
print("bars.md carries both met transitions (2026-08-16) verbatim from initiative-bars.toml")
PY
verdict "bars/initiative consistency (7)" $?

# --- 8. live re-verification (opt-in) ---------------------------------
if [ -n "${DR8_REFUSAL_LIVE:-}" ]; then
  CLI="${DR8_CLI:-$DEMO_DIR/../../../../target/debug/sovereign-cli}"
  [ -x "$CLI" ] || { echo "FAIL: $CLI not executable (DR8_CLI override)"; exit 1; }
  TMP=$(mktemp -d)
  RUST_LOG=sovereign_core::egress=debug "$CLI" deep-research \
    "How did American cities change across four decades (1980-2024): gentrification, inequality, affordability, and displacement — every claim cited?" \
    --search-source web --run-dir "$TMP" 2> "$TMP/refusal.log"
  rc=$?
  if [ "$rc" -ne 1 ] || ! grep -q "egress refused: query with personal custody" "$TMP/refusal.log" || \
     ! grep -q "default-deny" "$TMP/refusal.log"; then
    echo "FAIL: live refusal re-run exited $rc without the typed default-deny refusal"
    tail -5 "$TMP/refusal.log"
    FAILURES=$((FAILURES + 1))
  else
    echo "PASS: live refusal re-run — exit 1, typed default-deny refusal"
  fi
  rm -rf "$TMP"
fi

if [ -n "${DR8_RUN_DIR:-}" ]; then
  python3 - "$DR8_RUN_DIR" <<'PY'
import json, pathlib, sys
run = pathlib.Path(sys.argv[1])
fl = json.load(open(run / "fetch-list-1.json"))
hits = fl["search_hits"]
assert hits and all(h.get("engine") == "web" for h in hits), \
    f"hits not all engine=web: {[h.get('engine') for h in hits]}"
assert all(h.get("custody") == "public-web" for h in hits), \
    f"hits not all public-web: {[h.get('custody') for h in hits]}"
windows = [json.loads(p.read_text()) for p in sorted(run.glob("evidence-window-*.json"))]
chunks = [c for w in windows for c in w["chunks"]]
assert chunks and all(c.get("custody") == "public-web" for c in chunks), \
    f"window chunks not all public-web: {[c.get('custody') for c in chunks]}"
print(f"live run: {len(hits)} hits engine=web custody=public-web; {len(chunks)} window chunks public-web")
PY
  verdict "live run artifacts (8)" $?
fi

if [ "$FAILURES" -gt 0 ]; then
  echo "=== DEMO-8 verify: $FAILURES strip(s) FAILED — the failures are the measurements (named above) ==="
  exit 1
fi
echo "=== DEMO-8 verify: all strips pass ==="
