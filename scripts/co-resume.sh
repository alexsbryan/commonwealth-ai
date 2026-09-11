#!/usr/bin/env bash
# co-resume.sh — point an agent at a campaign and it picks up exactly where
# the last one left off.
#
# WHY (operator direction 2026-09-10): "a very deterministic and locked down
# system such that I can just point an agent to a campaign and they pick up
# exactly where the last one left off." Campaign state was spread across six
# sources — the committed ladder, gitignored orders, the journal, session
# frames, notes, the backlog and git log — so every session ASSEMBLED it, and
# every session assembled it differently. Nobody can be deterministic reading
# six things in an order nobody specified.
#
# THE DESIGN RULE, and the only one that matters here: this command INVENTS
# NOTHING. Every line is either computed from an artifact (the cursor, the
# ladder, the log) or printed VERBATIM from the one file that owns it (the
# demos, the stop conditions). There is no summarizer in the middle, because a
# summary is where the drift gets in — and a resume brief that paraphrases is
# a resume brief that can be wrong twice. Where a source is unreachable it
# says so; absence is reported, never defaulted (§18.3).
#
# ONE OPEN ORDER IS THE PICKUP POINT. Zero open orders means the next action
# is to draft the next rung, and this says which. TWO open orders is ambiguous
# and it says that rather than picking one.
#
#   scripts/co-resume.sh <campaign-id>          # the pickup brief
#   scripts/co-resume.sh <campaign-id> --brief  # header + next action only
set -uo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
FEATURES="${CO_FEATURES:-$REPO/.sovereign/features}"
PY="$(command -v python3 || echo python3)"

usage() { awk '/^#   scripts\/co-resume\.sh /{p=1} p{print; if (/--brief/) exit}' "$0"; exit 2; }
[ $# -ge 1 ] || usage
ID="$1"; shift || true
BRIEF=0
[ "${1:-}" = "--brief" ] && BRIEF=1

CAMPAIGN_MD="$FEATURES/$ID/campaign.md"
BARS_TOML="$REPO/quality/campaigns/$ID.toml"

# Peer claims come from the daemon and the daemon can be down. Captured here,
# rendered below, and a failure is a named line rather than a missing section.
CLAIMS="$(cd "$REPO" && (sovereign tools call work_in_flight --scope="$ID" --match_mode=symbol 2>/dev/null \
          || sovereign-cli tools call work_in_flight --scope="$ID" --match_mode=symbol 2>/dev/null) | head -20)"
GITLOG="$(cd "$REPO" && git log -8 --format='%h|%ad|%s' --date=short --grep="$ID" 2>/dev/null)"
BACKLOG="$(cd "$REPO" && "$PY" scripts/co-backlog.py --open 2>/dev/null | head -1)"

REPO="$REPO" FEATURES="$FEATURES" ID="$ID" CAMPAIGN_MD="$CAMPAIGN_MD" \
BARS_TOML="$BARS_TOML" CLAIMS="$CLAIMS" GITLOG="$GITLOG" BRIEF="$BRIEF" \
BACKLOG="$BACKLOG" "$PY" - <<'PY'
import os, re, glob, sys

ID = os.environ["ID"]
import shlex
repo_q = shlex.quote(os.environ["REPO"])
FEAT = os.environ["FEATURES"]
BRIEF = os.environ["BRIEF"] == "1"
out = []
def w(s=""): out.append(s)

# ── header: the committed objective, never a restatement ────────────────
bars = os.environ["BARS_TOML"]
objective = status = ""
if os.path.exists(bars):
    t = open(bars, encoding="utf-8", errors="replace").read()
    objective = (re.search(r'^objective\s*=\s*"(.*?)"', t, re.M) or [None, ""])[1]
    status = (re.search(r'^status\s*=\s*"(.*?)"', t, re.M) or [None, ""])[1]
else:
    t = ""
w(f"CAMPAIGN {ID}" + (f"  [{status}]" if status else "  [NO quality/campaigns/%s.toml]" % ID))
if objective:
    w(f"  {objective}")
w()

camp = os.environ["CAMPAIGN_MD"]
camp_text = open(camp, encoding="utf-8", errors="replace").read() if os.path.exists(camp) else ""
def section(name, text=None):
    src = camp_text if text is None else text
    m = re.search(rf"^## {name}.*?$\n(.*?)(?=^## |\Z)", src, re.S | re.M)
    return re.sub(r"<!--.*?-->", "", m.group(1), flags=re.S).strip() if m else ""

if not camp_text:
    w("!! NO campaign.md — every ambiguity axis is unlisted, which means every")
    w("   one of them escalates and your depth is unbounded. Fix before working:")
    w(f"     scripts/co-campaign.sh new {ID}")
    w()
else:
    appr = (re.search(r"^approved: *(.+)$", camp_text, re.M) or [None, "?"])[1]
    if appr.strip() in ("pending", "?"):
        w(f"!! campaign.md is approved: {appr.strip()} — treat its policy as a DRAFT")
        w()

# ── the demos, verbatim from the one file that owns them ────────────────
demos = section("Demos")
if demos:
    w("── DEMOS (verbatim; this is what the campaign is judged on)")
    w(demos if not BRIEF else demos.split("\n\n")[0])
    w()

# ── ladder state, COMPUTED with the instrument's own method ─────────────
# cw-net-deletion greps this block for DONE; read it the same way or two
# readers of one fact disagree (§10.6).
# ── the ladder, from the SCHEMA; the frontier, verbatim ────────────────
#
# The rung block became a `[[rung]]` table on 2026-09-10 (scripts/
# co-ladder.py) because three attempts to read it as prose each produced a
# confident wrong answer. This reads the table or says there is none — it
# never falls back to parsing the comments, because a fallback that guesses
# is the substitution the schema was minted to end (§18.3).
rungs = []
if t:
    try:
        import tomllib
        rungs = tomllib.loads(t).get("rung", [])
    except Exception:
        rungs = []
if rungs:
    by = {}
    for r in rungs:
        by[r.get("status", "?")] = by.get(r.get("status", "?"), 0) + 1
    w(f"── LADDER ({len(rungs)} rungs: "
      + ", ".join(f"{v} {k}" for k, v in sorted(by.items())) + ")")
    if not BRIEF:
        for r in rungs:
            if r.get("status") not in ("done", "descoped"):
                w(f"   {r.get('status','?').upper():<9} {r.get('id',''):<5} "
                  f"{(r.get('demo') or '-'):<5} {(r.get('title') or '')[:54]}")
        w(f"   (verify it: scripts/co-ladder.py check {ID})")
    w()
elif os.path.exists(bars):
    w(f"── LADDER  no [[rung]] table in quality/campaigns/{ID}.toml — the rung")
    w(f"   list is still prose and is NOT read here. scripts/co-ladder.py"
      f" migrate {ID}")
    w()

ladder = section("Ladder")
if ladder:
    w("── LIVE FRONTIER (verbatim from campaign.md)")
    w(ladder if not BRIEF else "\n".join(ladder.splitlines()[:6]))
    w()

# ── THE PICKUP POINT: open orders under this campaign, and their cursors ──
orders = []
for p in sorted(glob.glob(os.path.join(FEAT, "*", "order.md"))):
    head = open(p, encoding="utf-8", errors="replace").read(4096)
    if not re.search(r"^status:\s*open", head, re.M):
        continue
    cid = (re.search(r"^(?:campaign|serves):\s*(\S+)", head, re.M) or [None, ""])[1]
    if cid != ID:
        continue
    orders.append((os.path.basename(os.path.dirname(p)), os.path.dirname(p), head))

w("── PICK UP HERE")
if len(orders) > 1:
    w(f"   AMBIGUOUS: {len(orders)} open orders under this campaign. Not picking one:")
    for oid, _, _ in orders:
        w(f"     - {oid}")
    w("   Close the landed ones (co-order.sh close <id> landed) or ask the operator.")
elif not orders:
    w("   No open order under this campaign. The next action is to DRAFT one.")
    w("   The rung it should cover is the first unfinished one in the live")
    w("   frontier above — that is a judgement, so make it explicitly.")
    w(f"   scripts/co-order.sh new {ID}-<rung> ; fill Demo + Steps ; check ; then journal.")
else:
    oid, odir, ohead = orders[0]
    jpath = os.path.join(odir, "journal.md")
    w(f"   order   {oid}")
    if not os.path.exists(jpath):
        w("   cursor  NONE — the order has no journal, so there is no record of")
        w(f"           where the last session stopped. scripts/co-journal.sh new {oid}")
    else:
        j = open(jpath, encoding="utf-8", errors="replace").read()
        cur = (re.search(r"^cursor: *(.+)$", j, re.M) or [None, "?"])[1]
        steps = re.findall(r"^- \[(.)\] (\d+)\. *(.*)$", j, re.M)
        log = re.findall(r"^- (\d{4}-\d\d-\d\dT\d\d:\d\d) +step (\d+) → (\w+)(?: — (.*))?$", j, re.M)
        w(f"   cursor  {cur}")
        nxt = next((s for s in steps if s[0] in (" ", ">")), None)
        if nxt:
            state = "IN FLIGHT" if nxt[0] == ">" else "NEXT"
            w(f"   {state:<7} step {nxt[1]}. {nxt[2]}")
        else:
            w("   ALL STEPS DONE — land it, then close the order and draft the next rung.")
        blocked = [s for s in steps if s[0] == "!"]
        for b in blocked:
            w(f"   BLOCKED step {b[1]}. {b[2]}")
        if log and not BRIEF:
            w("   last transitions:")
            for ts, n, st, note in log[-3:]:
                w(f"     {ts}  step {n} → {st}" + (f" — {note}" if note else ""))
        w(f"   full   scripts/co-journal.sh show {oid}")
w()

if BRIEF:
    print("\n".join(out)); sys.exit(0)

# ── what the last sessions actually landed ──────────────────────────────
gl = os.environ["GITLOG"].strip()
w("── LANDED (git log, grep " + ID + ")")
if gl:
    for line in gl.splitlines()[:6]:
        h, d, s = line.split("|", 2)
        w(f"   {h}  {d}  {s[:76]}")
else:
    w("   no commits whose subject names this campaign (they may name the rung instead)")
w()

# ── the do-nots, verbatim ───────────────────────────────────────────────
for name, title in (("Stop conditions", "STOP CONDITIONS (wake the operator)"),
                    ("Ambiguity policy", "AMBIGUITY POLICY (what you decide yourself)")):
    body = section(name)
    if body:
        w(f"── {title}")
        w(body)
        w()

dec = section("Decisions")
if dec:
    entries = [re.sub(r"^-\s*", "", d).strip() for d in dec.split("\n- ") if d.strip()]
    w(f"── DECISIONS ({len(entries)} on record; last 3 — the close-out read)")
    for d in entries[:3]:
        w("- " + " ".join(d.split())[:300])
    w()

# ── peer claims, and a named failure if the daemon is down ──────────────
cl = os.environ["CLAIMS"].strip()
w("── PEERS")
w("   " + (cl.replace("\n", "\n   ") if cl else
           "work_in_flight unreachable (daemon down?) — check before editing hot files"))
w()

_stale = os.popen(f"cd {repo_q} && ./scripts/co-close.sh --audit 2>/dev/null").read().strip()
if _stale and "no finished-but-open" not in _stale:
    w("── UNCLOSED")
    w("   " + _stale.replace("\n", "\n   "))
    w()

w("── STANDING (the contract you are picking up under)")
w("   Work the cursor IN ORDER. Stamp every transition:")
w("     scripts/co-journal.sh step <order-id> <n> done|wip|blocked [note]")
w("   Before any work not on the cursor: name which demo above it moves and how")
w("   the operator would SEE that. If you cannot, it is an invented problem —")
w("     scripts/co-backlog-producer.sh --key <what went wrong> --title <one line>"
  f" --objective {ID}")
w("   Return to the operator on exactly three things: every step done, a stop")
w("   condition above, budget out. Not a finding, not a green gate, not a step")
w("   boundary. One build-test cycle per sub-problem; past that it is banked.")
w("   When the last step is done, CLOSE — it is the only write path out, and")
w("   the state the next session reads is only as good as this one:")
w("     scripts/co-close.sh <order-id> --decision \"<what you decided and why>\"")
w("   If you split: this brief is your objective — do NOT re-author it into a")
w("   session frame. Under an order a frame carries invariants and dead ends")
w("   only; state and next live in the cursor, which is maintained by working.")

print("\n".join(out))
PY
