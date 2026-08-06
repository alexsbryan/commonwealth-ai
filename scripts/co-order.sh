#!/usr/bin/env bash
# co-order.sh — comaintainer artifact 4: the work order.
#
# An order is ONE FILE: .sovereign/features/<id>/order.md (gitignored —
# per-host coordination, no PR ceremony). The director drafts it, the
# operator approves/edits it (that pair is a kind=order directive), a
# worker session picks it up on its first prompt. The session-boot hook
# shows one line per open order; SOVEREIGN_NO_ORDERS=1 hides even that.
#
# GENTLE BY DESIGN (operator direction 2026-08-06): a session without
# an order behaves exactly as today; only the Objective section is
# load-bearing — every other section may read "(none)"; `check` is
# advisory and nothing anywhere gates on it; editing the file by hand
# is always valid — this script is convenience, the file is the truth.
#
#   scripts/co-order.sh new <id> [title…]     # write the template
#   scripts/co-order.sh list                  # open orders, one line each
#   scripts/co-order.sh check <id>            # advisory completeness read
#   scripts/co-order.sh close <id> [landed|abandoned]   # default landed
set -uo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
FEATURES="$REPO/.sovereign/features"

usage() { sed -n '16,20p' "$0"; exit 2; }
[ $# -ge 1 ] || usage
CMD="$1"; shift || true

case "$CMD" in
  new)
    ID="${1:?usage: co-order.sh new <id> [title…]}"; shift || true
    TITLE="${*:-$ID}"
    F="$FEATURES/$ID/order.md"
    [ -e "$F" ] && { echo "co-order: $F already exists — edit it directly"; exit 2; }
    mkdir -p "$FEATURES/$ID"
    cat > "$F" <<EOF
---
schema: work-order/v1
id: $ID
status: open
drafted: $(date +%F)
approved: pending
---

# Order: $TITLE

## Objective

<!-- Initiative altitude, the SESSION_CONTINUITY §2.1 contract. The one
     load-bearing section: a worker holding only this is still oriented. -->

Done when:
Not worth continuing if:

## Lane

<!-- The measurement that proves the work. "(none)" is honest for work
     with no lane yet — but then say what would prove it. -->

(none)

## Scope

<!-- Paths/symbols the worker claims via declare_scope at start.
     Convention: if the work touches the daemon (restarts, model swaps),
     ALSO claim ~/.sovereign/config.toml — it is the shared-resource
     proxy that work_in_flight and the pre-commit hook can both see. -->

(none)

## Budget

<!-- Runs, model calls, sessions. House default worth restating here:
     one full test run at initiative end, not per-change. -->

(none)

## Seams

<!-- Contracts the worker must not renegotiate without the director.
     e.g. "daemon restarts route through the director"; "the holdout
     stays frozen"; "this feature's surface is X — charter/bench
     iteration beyond N runs without touching X is off-order".
     STANDING (safety switch, operator directive 2026-08-06): at the
     yellow cutoff, bank your frame — the seat reads it for alignment;
     at the hard cut, park (frame banked, claims released) — no split
     or respawn without operator ack through the seat. -->

(none)
EOF
    echo "co-order: drafted $F"
    echo "          fill Objective (the only required section), then have the operator approve."
    ;;

  list)
    found=0
    for f in "$FEATURES"/*/order.md; do
      [ -e "$f" ] || continue
      status="$(sed -n 's/^status: *//p' "$f" | head -1)"
      [ "$status" = "open" ] || continue
      id="$(basename "$(dirname "$f")")"
      title="$(sed -n 's/^# Order: *//p' "$f" | head -1)"
      printf '%-28s %s\n' "$id" "$title"
      found=1
    done
    [ "$found" = 1 ] || echo "co-order: no open orders (that is a fine state — orders are opt-in)"
    ;;

  check)
    ID="${1:?usage: co-order.sh check <id>}"
    F="$FEATURES/$ID/order.md"
    [ -e "$F" ] || { echo "co-order: no such order $F"; exit 2; }
    python3 - "$F" <<'PY'
import re, sys
text = open(sys.argv[1], encoding="utf-8", errors="replace").read()
body = re.sub(r"<!--.*?-->", "", text, flags=re.S)
def section(name):
    m = re.search(rf"^## {name}\n(.*?)(?=^## |\Z)", body, re.M | re.S)
    return (m.group(1).strip() if m else None)
problems, nudges = [], []
obj = section("Objective")
if not obj:
    problems.append("Objective section missing")
else:
    # [ \t]* not \s*: the template stacks the two labels on adjacent
    # lines, and \s* walks across the newline into the next label —
    # an empty 'Done when:' then reads as filled (watched failing).
    if not re.search(r"Done when:[ \t]*\S", obj):
        problems.append("Objective has an empty 'Done when:' — it is not falsifiable yet")
    if not re.search(r"Not worth continuing if:[ \t]*\S", obj):
        problems.append("Objective has an empty 'Not worth continuing if:'")
for name in ("Lane", "Scope", "Budget", "Seams"):
    s = section(name)
    if s in (None, "", "(none)"):
        nudges.append(f"{name} is (none) — fine, but a worker cannot be steered on what it doesn't say")
if problems:
    print("NOT READY (advisory — nothing gates on this):")
    for p in problems: print(f"  - {p}")
for n in nudges: print(f"  nudge: {n}")
if not problems:
    print("ready: objective is load-bearing and falsifiable" +
          ("" if not nudges else f" ({len(nudges)} optional section(s) empty)"))
sys.exit(1 if problems else 0)
PY
    ;;

  close)
    ID="${1:?usage: co-order.sh close <id> [landed|abandoned]}"
    STATE="${2:-landed}"
    F="$FEATURES/$ID/order.md"
    [ -e "$F" ] || { echo "co-order: no such order $F"; exit 2; }
    python3 - "$F" "$STATE" <<'PY'
import re, sys
p, state = sys.argv[1:3]
t = open(p, encoding="utf-8").read()
t2 = re.sub(r"^status: .*$", f"status: {state}", t, count=1, flags=re.M)
open(p, "w").write(t2)
print(f"co-order: {p} -> status: {state}")
PY
    ;;

  *) usage ;;
esac
