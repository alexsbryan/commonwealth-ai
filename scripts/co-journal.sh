#!/usr/bin/env bash
# co-journal.sh — comaintainer artifact 5: the worker's CURSOR.
#
# WHY (operator direction 2026-09-10): an order is read once, at boot,
# and never again. A hundred thousand tokens later what is actually in
# working attention is the last failing test, not the objective — so a
# worker drifts into whatever it most recently touched, and every turn
# boundary looks like a plausible place to stop and ask. The order is
# the CONTRACT and does not change; this is the CURSOR and changes
# constantly. Re-anchoring costs one `show` (~25 lines) instead of a
# re-read of a 200-line order, which is the only reason it happens at
# all.
#
# Same contract as co-order.sh and co-campaign.sh: ONE FILE
# (.sovereign/features/<id>/journal.md, gitignored alongside the order),
# hand-editing is always valid, `check` is advisory and nothing gates on
# it, and a worker without a journal behaves exactly as it does today.
#
# THIS IS NOT A FINDINGS STORE (§19 inventory, §10.6 one decider). An
# off-order discovery BANKS:
#
#   scripts/co-backlog-producer.sh --key <what went wrong, never a run id> \
#       --title <one line> --objective <the order's serves:>
#
# That store already dedups by key and is already the operator's
# close-out read. A second ledger here would be two implementations of
# one thing, and the one nobody triages would be this one.
#
#   scripts/co-journal.sh new <order-id>     # derive the cursor from the order
#   scripts/co-journal.sh show <order-id>    # THE RE-ANCHOR READ
#   scripts/co-journal.sh step <order-id> <n> <done|wip|blocked> [note…]
#   scripts/co-journal.sh check <order-id>   # advisory: is the cursor moving?
set -uo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
# Same test/harness override as co-order.sh, for the same reason: a
# throwaway journal must not land where `list` shows it to the operator.
FEATURES="${CO_FEATURES:-$REPO/.sovereign/features}"
PY="$(command -v python3 || echo python3)"

usage() { awk '/^#   scripts\/co-journal\.sh /{p=1} p{print; if (/check <order-id>/) exit}' "$0"; exit 2; }
[ $# -ge 1 ] || usage
CMD="$1"; shift || true

_order() { echo "$FEATURES/$1/order.md"; }
_file()  { echo "$FEATURES/$1/journal.md"; }

case "$CMD" in
  new)
    ID="${1:?usage: co-journal.sh new <order-id>}"
    O="$(_order "$ID")"; F="$(_file "$ID")"
    [ -e "$O" ] || { echo "co-journal: no order at $O — the journal is derived from one"; exit 2; }
    [ -e "$F" ] && { echo "co-journal: $F already exists — edit it directly, or \`show\` it"; exit 2; }
    ORDER="$O" OUT="$F" ID="$ID" "$PY" - <<'PY'
import os, re, datetime

order = open(os.environ["ORDER"], encoding="utf-8").read()
oid = os.environ["ID"]

title = (re.search(r"^# Order: *(.+)$", order, re.M) or [None, oid])[1]
serves = (re.search(r"^serves: *(.+?) *(?:#.*)?$", order, re.M) or [None, "(unattributed)"])[1]
campaign = (re.search(r"^campaign: *(.+?) *$", order, re.M) or [None, ""])[1]
if serves.startswith("(unattributed") and campaign:
    serves = campaign   # the cw-lift orders key on `campaign:`, not `serves:`

def section(name):
    m = re.search(rf"^## {name}\s*$\n(.*?)(?=^## |\Z)", order, re.S | re.M)
    return m.group(1) if m else ""

# The objective's own prose, comments stripped, capped. A worker holding
# only this is still oriented — SESSION_CONTINUITY §2.1, the same
# contract co-order.sh's template names.
obj = re.sub(r"<!--.*?-->", "", section("Objective"), flags=re.S).strip()
paras = [p.strip() for p in obj.split("\n\n") if p.strip()]
objective = "\n\n".join(paras[:2])[:1200]

# Done-when is written two ways in this tree: an inline `Done when:` line
# in Objective (co-order.sh's template) and a `## Done when` section (the
# cw-lift orders). Read both — a cursor that cannot state the finish line
# is the artifact failing at its one job.
done = " ".join(
    l.strip() for l in obj.splitlines() if l.strip().lower().startswith("done when")
)
if len(done) < 13:
    dw = re.sub(r"<!--.*?-->", "", section("Done when"), flags=re.S).strip()
    done = "Done when: " + " ".join(dw.split()) if dw else "(the order names no Done-when)"
stop = " ".join(
    l.strip() for l in obj.splitlines() if l.strip().lower().startswith("not worth continuing")
)

# THE DEMO — operator direction 2026-09-10, "get our demos humming
# perfectly". An objective stated as architecture can be satisfied by
# work no one can watch, which is how hours go into fixes for invented
# problems. The demo is the done-when an operator can SEE run. An order
# with no `## Demo` falls back to its Done-when, and if that names no
# runnable thing the journal says so rather than inventing one.
# NO FALLBACK, deliberately (§18.3). A Done-when reading "cargo test green
# + boundary-gate 7/7" is precisely what a demo is not, and filling this
# slot with it would dress a build gate as a thing a person can watch —
# the silent substitution, in the one place this artifact exists to
# prevent. Absent is reported, never defaulted.
demo = re.sub(r"<!--.*?-->", "", section("Demo"), flags=re.S).strip()

# Steps, cheapest source first. An explicit `## Steps` list is the
# intended one; the fallbacks exist so the four cw-lift orders — written
# before this artifact and shaped as Waves/Lanes — get a cursor without
# being rewritten.
steps = []
body = section("Steps")
if body:
    steps = [re.sub(r"^\s*(?:[-*]|\d+[.)])\s*(?:\[.\]\s*)?", "", l).strip()
             for l in body.splitlines() if re.match(r"^\s*(?:[-*]|\d+[.)])\s+", l)]
if not steps:
    # `### Wave 1 · Lane A — the crate, the codec, the seal`
    steps = [m.strip() for m in re.findall(r"^### +(.+?)\s*$", order, re.M)]
if not steps:
    dw = section("Done when") or re.search(r"^## Done when\s*$\n(.*?)(?=^## |\Z)", order, re.S | re.M)
    steps = [re.sub(r"^\s*[-*]\s*", "", l).strip()
             for l in (section("Done when")).splitlines() if re.match(r"^\s*[-*]\s+", l)]
steps = [s for s in steps if s][:20]

today = datetime.date.today().isoformat()
lines = [
    "---",
    "schema: work-journal/v1",
    f"order: {oid}",
    f"campaign: {campaign}" if campaign else None,
    f"serves: {serves}",
    f"opened: {today}",
    f"cursor: 0/{len(steps)}",
    "---",
    "",
    f"# Journal: {title}",
    "",
    "<!-- THE CURSOR, not a second order. The order is the contract and does",
    "     not change here. Update this at the end of every batch in which a",
    "     step moved: `co-journal.sh step <id> <n> done|wip|blocked [note]`.",
    "     Re-read it with `show` whenever you re-orient. Hand-editing is fine. -->",
    "",
    "## Demo — what the operator watches to know this landed",
    "",
    demo or (
        "**THE ORDER NAMED NO DEMO.** Write one here before starting: name the "
        "campaign demo this rung moves (`.sovereign/features/<campaign>/"
        "campaign.md` §Demos) and what the operator would watch. If the rung "
        "genuinely is not demo-visible, say so and name the demo it unblocks — "
        "that is a legal answer. A step that moves no demo and unblocks none "
        "is banked, not built. A build gate is not a demo."
    ),
    "",
    "## Objective (copied from the order — read this before deciding anything)",
    "",
    objective or "(see the order)",
    "",
    f"**{done}**",
]
if stop:
    lines += ["", f"**{stop}**"]
lines += [
    "",
    "## Steps",
    "",
]
if steps:
    lines += [f"- [ ] {i}. {s}" for i, s in enumerate(steps, 1)]
else:
    lines += [
        "<!-- The order named no steps and none could be derived. Write them",
        "     now, ordered, before starting: a plan with no cursor is why the",
        "     objective gets lost. -->",
        "- [ ] 1. (write the ordered step list here)",
    ]
lines += [
    "",
    "## Log",
    "",
    "<!-- One line per step transition, appended by `step`. This is the",
    "     close-out read: where the time went, and what blocked. -->",
    "",
]

with open(os.environ["OUT"], "w", encoding="utf-8") as fh:
    fh.write("\n".join(l for l in lines if l is not None) + "\n")
print(f"co-journal: wrote {os.environ['OUT']} with {len(steps)} step(s)")
PY
    echo "          re-anchor with: scripts/co-journal.sh show $ID"
    echo "          off-order findings BANK, they do not go here (see this script's header)"
    ;;

  show)
    ID="${1:?usage: co-journal.sh show <order-id>}"
    F="$(_file "$ID")"
    [ -e "$F" ] || { echo "co-journal: no journal for $ID — \`co-journal.sh new $ID\`"; exit 2; }
    JOURNAL="$F" ID="$ID" "$PY" - <<'PY'
import os, re
text = open(os.environ["JOURNAL"], encoding="utf-8").read()
text = re.sub(r"<!--.*?-->", "", text, flags=re.S)

def sect(name):
    m = re.search(rf"^## {name}.*?$\n(.*?)(?=^## |\Z)", text, re.S | re.M)
    return (m.group(1).strip() if m else "")

cursor = (re.search(r"^cursor: *(.+)$", text, re.M) or [None, "?"])[1]
serves = (re.search(r"^serves: *(.+)$", text, re.M) or [None, "(unattributed)"])[1]
title = (re.search(r"^# Journal: *(.+)$", text, re.M) or [None, os.environ["ID"]])[1]

print(f"── {title}   cursor {cursor}")
print()
print("── DEMO (what the operator watches)")
print(sect("Demo") or "(none named — see the journal)")
print()
print("── Objective")
print(sect("Objective"))
print()
print("── Steps")
for l in sect("Steps").splitlines():
    if not l.strip():
        continue
    mark = "  " if "[ ]" in l else (" ✓" if "[x]" in l else (" ▸" if "[>]" in l else " ✗"))
    print(f"{mark} {re.sub(r'^- \[.\] ', '', l)}")
log = [l for l in sect("Log").splitlines() if l.strip()]
if log:
    print()
    print("── Log (last 3)")
    for l in log[-3:]:
        print(f"   {l}")
print()
print("── Standing")
print("   Before any step you did not plan: which demo above does it move,")
print("   and how would the operator SEE that? If you cannot say, it is an")
print("   invented problem — bank it and return to the cursor.")
print("   Off-order finding? BANK it and keep marching:")
print(f"     scripts/co-backlog-producer.sh --key <what went wrong> \\")
print(f"         --title <one line> --objective {serves}")
print("   Return to the seat ONLY on: every step done · a campaign stop")
print("   condition · budget out. Ambiguity outside the campaign's policy →")
print("   take the smaller change, log the call, continue.")
PY
    ;;

  step)
    ID="${1:?usage: co-journal.sh step <order-id> <n> <done|wip|blocked> [note…]}"; shift
    N="${1:?step number}"; shift
    STATE="${1:?done|wip|blocked}"; shift || true
    NOTE="${*:-}"
    F="$(_file "$ID")"
    [ -e "$F" ] || { echo "co-journal: no journal for $ID — \`co-journal.sh new $ID\`"; exit 2; }
    case "$STATE" in done|wip|blocked) ;; *) echo "co-journal: state must be done|wip|blocked"; exit 2 ;; esac
    JOURNAL="$F" N="$N" STATE="$STATE" NOTE="$NOTE" "$PY" - <<'PY'
import os, re, datetime, sys
p = os.environ["JOURNAL"]
text = open(p, encoding="utf-8").read()
n, state, note = int(os.environ["N"]), os.environ["STATE"], os.environ["NOTE"]
box = {"done": "x", "wip": ">", "blocked": "!"}[state]

hit = [False]
def repl(m):
    if int(m.group(2)) == n:
        hit[0] = True
        return f"- [{box}] {m.group(2)}."
    return m.group(0)

body = re.sub(r"^- \[(.)\] (\d+)\.", repl, text, flags=re.M)
if not hit[0]:
    sys.exit(f"co-journal: no step {n} in {p}")

steps = re.findall(r"^- \[(.)\] \d+\.", body, flags=re.M)
body = re.sub(r"^cursor: .*$", f"cursor: {steps.count('x')}/{len(steps)}", body, flags=re.M)

stamp = datetime.datetime.now().strftime("%Y-%m-%dT%H:%M")
entry = f"- {stamp}  step {n} → {state}" + (f" — {note}" if note else "")
body = body.rstrip("\n") + "\n" + entry + "\n"
open(p, "w", encoding="utf-8").write(body)
print(f"co-journal: step {n} → {state}   cursor {steps.count('x')}/{len(steps)}")
if state == "blocked":
    print("            blocked ≠ escalate. Is it a campaign stop condition? Escalate.")
    print("            Otherwise bank it, take the smaller path, and move to the next step.")
PY
    ;;

  check)
    ID="${1:?usage: co-journal.sh check <order-id>}"
    F="$(_file "$ID")"
    [ -e "$F" ] || { echo "co-journal: no journal for $ID (legal — orders work without one)"; exit 0; }
    JOURNAL="$F" ID="$ID" "$PY" - <<'PY'
import os, re, datetime
text = open(os.environ["JOURNAL"], encoding="utf-8").read()
print(f"co-journal check {os.environ['ID']} (advisory — nothing gates on this)")
steps = re.findall(r"^- \[(.)\] (\d+)\. *(.*)$", text, flags=re.M)
if not steps:
    print("  UNSET  Steps — a plan with no cursor is the failure this artifact exists for")
else:
    print(f"  set    Steps — {sum(1 for s,_,_ in steps if s=='x')}/{len(steps)} done, "
          f"{sum(1 for s,_,_ in steps if s=='>')} in flight, "
          f"{sum(1 for s,_,_ in steps if s=='!')} blocked")
log = re.findall(r"^- (\d{4}-\d\d-\d\dT\d\d:\d\d) +step (\d+) → (\w+)", text, flags=re.M)
if not log:
    print("  UNSET  Log — no step has moved; the cursor is decorative until one does")
else:
    last = datetime.datetime.strptime(log[-1][0], "%Y-%m-%dT%H:%M")
    mins = int((datetime.datetime.now() - last).total_seconds() // 60)
    print(f"  set    Log — {len(log)} transition(s), last {mins} min ago")
    # THE DEPTH BOUND, made visible rather than remembered (§7). A step
    # that has been in flight for an hour is the rabbit hole, and the
    # worker inside it is the last to notice.
    wip = [(t, s) for t, s, st in log if st == "wip"]
    if wip and mins >= 45:
        print(f"  WARN   step {wip[-1][1]} has been in flight {mins} min with no transition.")
        print("         Depth bound: one build-test cycle per sub-problem. Past that it")
        print("         is a banked finding, not a task — take the smaller path and move on.")
PY
    ;;

  *) usage ;;
esac
