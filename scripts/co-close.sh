#!/usr/bin/env bash
# co-close.sh — the ONE write path out of campaign work.
#
# WHY (operator, 2026-09-10): "if we don't keep the write clean how do we
# expect the system to stay clean?" co-resume.sh is deterministic only because
# the state it reads is maintained, and a deterministic read over stale state
# is WORSE than no read — it hands the next session wrong facts with a
# confident face. Everything this repo has watched decay decayed the same way:
# the journal SKILL.md mandated and nobody wrote, the banking clause every
# spawn carries with zero items filed, the campaign file two campaigns out of
# many have.
#
# THE RULE THIS ENCODES. An artifact stays current when producing it is a
# BYPRODUCT of doing the work, and rots when it is a report ABOUT the work.
# The cursor stays current because `show` is how you re-orient — you need it,
# so you keep it. The commit stays current because you cannot land without it.
# The rung row and the Decisions line are pure overhead and will rot, so they
# get ONE command at the only moment anyone is motivated to run it (the end),
# and a ratchet that catches the sessions that skip it. Do not add a fifth
# thing to remember; add it here or accept that it decays.
#
# It REFUSES rather than guessing: an incomplete cursor, a rung with no
# resolvable sha, a campaign whose Decisions section it cannot find. Absence
# is reported, never defaulted (§18.3).
#
#   scripts/co-close.sh <order-id> --decision "<what you decided and why>" [--sha <sha>]
#   scripts/co-close.sh <order-id> --abandon --decision "<why>"
#   scripts/co-close.sh --audit                 # orders finished but never closed
set -uo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
FEATURES="${CO_FEATURES:-$REPO/.sovereign/features}"
PY="$(command -v python3 || echo python3)"

usage() { awk '/^#   scripts\/co-close\.sh /{p=1} p{print; if (/--audit/) exit}' "$0"; exit 2; }
[ $# -ge 1 ] || usage

if [ "$1" = "--audit" ]; then
    # THE RATCHET. A close that is optional is a close that stops happening,
    # so this is what the boot block and co-resume both ask. Cheap: one read
    # of each open order's journal.
    FEATURES="$FEATURES" "$PY" - <<'PY'
import os, re, glob
feat = os.environ["FEATURES"]
stale = []
for p in sorted(glob.glob(os.path.join(feat, "*", "order.md"))):
    head = open(p, encoding="utf-8", errors="replace").read(4096)
    if not re.search(r"^status:\s*open", head, re.M):
        continue
    j = os.path.join(os.path.dirname(p), "journal.md")
    if not os.path.exists(j):
        continue
    body = open(j, encoding="utf-8", errors="replace").read()
    steps = re.findall(r"^- \[(.)\] \d+\.", body, re.M)
    if steps and all(s == "x" for s in steps):
        stale.append((os.path.basename(os.path.dirname(p)), len(steps)))
if not stale:
    print("co-close --audit: no finished-but-open orders")
else:
    print(f"co-close --audit: {len(stale)} order(s) finished and never closed —")
    print("  the cursor says every step is done and the order still reads open,")
    print("  so the next session picks up work that is already landed.")
    for oid, n in stale:
        print(f"    {oid}  ({n}/{n} steps)  ->  scripts/co-close.sh {oid} --decision \"...\"")
PY
    exit 0
fi

ID="$1"; shift
DECISION=""; SHA=""; ABANDON=0
while [ $# -gt 0 ]; do
    case "$1" in
        --decision) DECISION="${2:?--decision needs text}"; shift 2 ;;
        --sha)      SHA="${2:?--sha needs a sha}"; shift 2 ;;
        --abandon)  ABANDON=1; shift ;;
        *) usage ;;
    esac
done

ORDER="$FEATURES/$ID/order.md"
[ -e "$ORDER" ] || { echo "co-close: no order at $ORDER"; exit 2; }
[ -n "$DECISION" ] || { echo "co-close: --decision is required. It is the one thing"; \
    echo "          nothing else can derive, and the close-out read is made of these."; exit 2; }
SHA_EXPLICIT=0
[ -n "$SHA" ] && SHA_EXPLICIT=1
[ -n "$SHA" ] || SHA="$(cd "$REPO" && git rev-parse --short=9 HEAD 2>/dev/null)"

REPO="$REPO" FEATURES="$FEATURES" ID="$ID" DECISION="$DECISION" SHA="$SHA" \
ABANDON="$ABANDON" SHA_EXPLICIT="$SHA_EXPLICIT" "$PY" - <<'PY'
import os, re, sys, datetime, subprocess

repo, feat, oid = os.environ["REPO"], os.environ["FEATURES"], os.environ["ID"]
decision, sha = os.environ["DECISION"], os.environ["SHA"]
abandon = os.environ["ABANDON"] == "1"
odir = os.path.join(feat, oid)
order = open(os.path.join(odir, "order.md"), encoding="utf-8").read()

def field(name, text=order):
    m = re.search(rf"^{name}: *(.+?) *(?:#.*)?$", text, re.M)
    return m.group(1) if m else ""

campaign = field("campaign") or (field("serves").split() or [""])[0]
rung = field("rung")
today = datetime.date.today().isoformat()

# 1. THE CURSOR MUST BE COMPLETE. Refusing here is the point: a close that
#    accepts a half-done cursor teaches everyone the cursor is decoration.
jp = os.path.join(odir, "journal.md")
if os.path.exists(jp) and not abandon:
    j = open(jp, encoding="utf-8").read()
    steps = re.findall(r"^- \[(.)\] (\d+)\. *(.*)$", j, re.M)
    open_steps = [s for s in steps if s[0] != "x"]
    if open_steps:
        print(f"co-close: REFUSED — {len(open_steps)} step(s) not done:")
        for st, n, ttl in open_steps[:5]:
            print(f"    [{st}] {n}. {ttl[:70]}")
        print("  Finish them, or close with --abandon and say why in --decision.")
        sys.exit(1)

# 2. THE SHA MUST RESOLVE. A rung row whose sha does not exist is the defect
#    this campaign's own file records twice; do not add a tenth.
# Abandoned work has no landing sha, so it stamps none — but a sha the
# caller DID pass is validated either way. Skipping the check under
# --abandon wrote `deadbeef1` into a campaign's Decisions log during this
# script's own refusal test: the unchecked branch is the one that bites.
explicit = os.environ.get("SHA_EXPLICIT") == "1"
if abandon and not explicit:
    sha = ""
if sha:
    r = subprocess.run(["git", "-C", repo, "cat-file", "-e", f"{sha}^{{commit}}"],
                       capture_output=True)
    if r.returncode != 0:
        sys.exit(f"co-close: REFUSED — sha {sha} does not resolve. Pass --sha explicitly.")

wrote = []

# 3. The rung row, into the campaign's schema.
bars = os.path.join(repo, "quality", "campaigns", f"{campaign}.toml")
if rung and os.path.exists(bars) and not abandon:
    t = open(bars, encoding="utf-8").read()
    if re.search(rf'^\[\[rung\]\]\nid     = "{re.escape(rung)}"', t, re.M):
        print(f"co-close: rung {rung} already has a row — not duplicating it")
    elif "[[rung]]" not in t:
        print(f"co-close: {campaign}.toml has no [[rung]] table "
              f"(scripts/co-ladder.py migrate {campaign}) — rung row NOT written")
    else:
        title = (re.search(r"^# Order: *(.+)$", order, re.M) or [None, oid])[1]
        title = re.sub(r"^.*?—\s*", "", title).strip().replace('"', "'")
        demo = ""
        dm = re.search(r"^## Demo\s*$\n+\**(D\d)", order, re.M)
        if dm:
            demo = dm.group(1)
        with open(bars, "a", encoding="utf-8") as fh:
            fh.write(f'\n[[rung]]\nid     = "{rung}"\ntitle  = "{title[:90]}"\n'
                     f'status = "done"\nsha    = ["{sha}"]\ndemo   = "{demo}"\n')
        wrote.append(f"rung {rung} -> quality/campaigns/{campaign}.toml")

# 4. The Decisions line, into the campaign file. This is the close-out read.
camp = os.path.join(feat, campaign, "campaign.md")
if os.path.exists(camp):
    c = open(camp, encoding="utf-8").read()
    if re.search(r"^## Decisions\s*$", c, re.M):
        verb = "ABANDONED" if abandon else "LANDED"
        stamp = f" ({sha})" if sha else ""
        line = f"\n- {today} — **{oid} {verb}**{stamp}. " + " ".join(decision.split())
        c = c.rstrip("\n") + line + "\n"
        open(camp, "w", encoding="utf-8").write(c)
        wrote.append(f"decision -> {campaign}/campaign.md")
    else:
        print(f"co-close: {campaign}/campaign.md has no '## Decisions' section — "
              "decision NOT written")
else:
    print(f"co-close: no campaign.md for {campaign} — decision NOT written. "
          f"scripts/co-campaign.sh new {campaign}")

for line in wrote:
    print(f"co-close: wrote {line}")
print(f"co-close: now run  scripts/co-order.sh close {oid} "
      f"{'abandoned' if abandon else 'landed'}")
print("          and release any scope you claimed (release_scope).")
PY
