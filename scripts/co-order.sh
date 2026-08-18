#!/usr/bin/env bash
# co-order.sh — comaintainer artifact 4: the work order.
#
# An order is ONE FILE: .sovereign/features/<id>/order.md (gitignored —
# per-host coordination, no PR ceremony). The director drafts it, the
# operator approves/edits it (that pair is a kind=order directive), a
# worker session picks it up on its first prompt. The session-boot hook
# shows one line per open order; SOVEREIGN_NO_ORDERS=1 hides even that.
#
# MESH SHADOW (order seat-durable-rail, an AMEND): `new` also writes a
# global decision note anchored order-seat — the order's mesh-visible
# shadow, so a seat on ANOTHER machine sees it through `list` (with
# node attribution from the notes daemon), and `close` retires that
# note so peers see the order gone. The FILE is still the truth:
# hand-editing it is still valid, a session without an order behaves
# exactly as before, and a notes daemon that is down produces a named
# notice, never a failure.
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
PY="$(command -v python3 || echo python3)"
CO_DIR="$(cd "$(dirname "$0")" && pwd)"

# Anchored on the command-list pattern, not line numbers — a header
# paragraph above it must not drift what `usage` prints.
usage() { awk '/^#   scripts\/co-order\.sh /{p=1} p{print; if (/close <id>/) exit}' "$0"; exit 2; }
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
# serves: <campaign-id> [<bar-id> ...] — WHICH DECLARED BARS THIS ORDER MOVES.
# The bars live in quality/campaigns/<id>.toml; scripts/co-lineage.py renders
# coverage against them. Same vocabulary as the backlog header's \`Objective:\`
# (scripts/BACKLOG.md) — one decider, one name, not a second "what this serves".
# Leaving it (unattributed) is LEGAL and stays VISIBLE in the rollup's
# unattributed count; it is never silently dropped. \`co-order.sh check\` says so.
serves: (unattributed)
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

## Engine

<!-- Who does the work — model + effort, per phase when the order has
     phases (operator practice, 2026-08-06: solid plan + brute-force
     coding = opus/medium; hard tech design = fable/high). A phase
     switch is a bank-frame-and-respawn, which routes through the
     operator ack like any restart. Spawned subagents honor the model
     via the Agent tool; effort control needs a full-session boot —
     the seat prepares frame+order and hands the operator the one
     command. -->

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
    echo "          set \`serves:\` if this order moves a declared initiative bar"
    echo "          (\`scripts/co-lineage.py list\`) — otherwise it renders unattributed."

    # Mesh write-through: the order's mesh-visible shadow (order
    # seat-durable-rail). The FILE is the truth — a daemon failure is
    # a named notice, exit stays 0.
    if CO_DIR="$CO_DIR" "$PY" "$CO_DIR/co_notes.py" write-note --kind decision \
        --scope global --related-entity order-seat \
        --content "$(printf 'order: %s\ntitle: %s\nstatus: open\nopened: %s\nfile: %s\n' \
            "$ID" "$TITLE" "$(date +%s)" ".sovereign/features/$ID/order.md")" >/dev/null 2>&1
    then
        echo "co-order: mesh shadow written (note anchored order-seat — any mesh seat can list it)"
    else
        echo "co-order: notes daemon unreachable — the FILE is written; mesh visibility starts on the next daemon (no action needed)" >&2
    fi
    ;;

  list)
    # Local files first — the file is the truth. Output is identical
    # to the pre-mesh script for the local-only case (gentle ramp).
    found=0
    LOCAL_OPEN_IDS=""
    for f in "$FEATURES"/*/order.md; do
      [ -e "$f" ] || continue
      status="$(sed -n 's/^status: *//p' "$f" | head -1)"
      [ "$status" = "open" ] || continue
      id="$(basename "$(dirname "$f")")"
      title="$(sed -n 's/^# Order: *//p' "$f" | head -1)"
      printf '%-28s %s\n' "$id" "$title"
      LOCAL_OPEN_IDS="$LOCAL_OPEN_IDS $id"
      found=1
    done

    # Mesh rows: orders opened by a seat on another machine. The
    # daemon stamps attribution, so a peer's order shows its machine,
    # not a guess (UC-D1 pass bar). An id with a local file is not
    # duplicated — the file is the truth. A down daemon is a named
    # notice; `list` still works.
    MESH_JSON="$("$PY" "$CO_DIR/co_notes.py" read-notes --include-operational \
        --kinds decision --limit 100 2>/dev/null)" || MESH_JSON=""
    if [ -n "$MESH_JSON" ]; then
      # The read-notes payload (up to 100 full notes) can exceed execve's
      # per-string limit (E2BIG at ~128KB) — a temp FILE is the channel,
      # never an env var (caught by the order seat-durable-rail verify).
      MESH_TMP="$(mktemp 2>/dev/null)" && printf '%s' "$MESH_JSON" > "$MESH_TMP" || MESH_TMP=""
      MESH_ROWS="$(LOCAL_OPEN_IDS="$LOCAL_OPEN_IDS" MESH_TMP="$MESH_TMP" "$PY" - <<'PY'
import json, os, re
rows = []
try:
    with open(os.environ["MESH_TMP"], encoding="utf-8") as fh:
        rows = json.load(fh).get("notes", [])
except (json.JSONDecodeError, OSError):
    pass
local = set(os.environ["LOCAL_OPEN_IDS"].split())
out = []
for n in rows:
    if (n.get("related_entity") or "").lower() != "order-seat":
        continue
    content = n.get("content") or ""
    m = re.search(r"^order:\s*(\S+)", content, re.M)
    if not m:
        continue
    oid = m.group(1)
    if oid in local:
        continue
    status = re.search(r"^status:\s*(\S+)", content, re.M)
    if status and status.group(1) != "open":
        continue
    title = re.search(r"^title:\s*(.+)$", content, re.M)
    author = n.get("author") or "unknown origin"
    out.append((oid, (title.group(1).strip() if title else oid), author))
for oid, title, author in sorted(out):
    print(f"{oid:<28} {title}   [{author}]")
PY
      )"
      rm -f "$MESH_TMP"
      if [ -n "$MESH_ROWS" ]; then
        while IFS= read -r line; do printf '%s\n' "$line"; done <<<"$MESH_ROWS"
        found=1
      fi
    else
      echo "co-order: notes daemon unreachable — local orders only (mesh visibility off)" >&2
    fi
    [ "$found" = 1 ] || echo "co-order: no open orders (that is a fine state — orders are opt-in)"
    ;;

  check)
    ID="${1:?usage: co-order.sh check <id>}"
    F="$FEATURES/$ID/order.md"
    [ -e "$F" ] || { echo "co-order: no such order $F"; exit 2; }
    python3 - "$F" "$CO_DIR" <<'PY'
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
for name in ("Lane", "Scope", "Engine", "Budget", "Seams"):
    s = section(name)
    if s in (None, "", "(none)"):
        nudges.append(f"{name} is (none) — fine, but a worker cannot be steered on what it doesn't say")

# `serves:` — lineage. Advisory like everything else here, but NEVER silent:
# an unattributed order is a legal state that must be SEEN (#6), and a serves
# line naming a bar nobody declared is a typo that would render as a gap.
# The declaration file being absent is could-not-judge, not a pass.
#
# The frontmatter parser and the declaration loader are IMPORTED from
# co-lineage.py, never re-implemented here (#8, one decider one name): two
# copies of "what does `serves:` mean" would drift the first time either side
# gained a form, and the drift would be silent on exactly the field whose
# whole job is to not be silent.
import importlib.util, pathlib
spec = importlib.util.spec_from_file_location("co_lineage", f"{sys.argv[2]}/co-lineage.py")
try:
    lineage = importlib.util.module_from_spec(spec)
    # Register BEFORE exec: co-lineage.py defines @dataclass types, and
    # dataclasses resolves each class's module through sys.modules. Skip this
    # and the decorator dies with a bare "'NoneType' object has no attribute
    # '__dict__'" — watched failing 2026-08-12 before the line was added.
    sys.modules[spec.name] = lineage
    spec.loader.exec_module(lineage)
except Exception as exc:
    lineage = None
    nudges.append(f"serves: could-not-judge — co-lineage.py unimportable ({exc})")

if lineage is not None:
    order = lineage.parse_order(pathlib.Path(sys.argv[1]))
    if order is None:
        problems.append("frontmatter is missing or malformed — no `---` block at the top")
    elif not order.attributed:
        nudges.append("serves: is (unattributed) — legal, and it will render that way in "
                      "`co-lineage.py`; set it if this order moves a declared initiative bar")
    else:
        try:
            _voc, inits, _raw = lineage.load_declaration()
        except lineage.DataError as exc:
            inits = None
            nudges.append(f"serves: names {order.serves_initiative} — could-not-judge: {exc}")
        if inits is not None:
            known = {i.id: {b.id for b in i.bars} for i in inits}
            if order.serves_initiative not in known:
                problems.append(f"serves: names campaign {order.serves_initiative!r}, which "
                                f"no file under quality/campaigns/ declares")
            else:
                unknown = [b for b in order.serves_bars if b not in known[order.serves_initiative]]
                if unknown:
                    problems.append(f"serves: names bar(s) {unknown} not declared for "
                                    f"{order.serves_initiative} — known: "
                                    f"{sorted(known[order.serves_initiative])}")
                elif not order.serves_bars:
                    nudges.append(f"serves: names {order.serves_initiative} but no bar — the rollup "
                                  "will show this order under the initiative with no bar moved")
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

    # Mesh write-through: retire the order's shadow note so the close
    # converges to peers (UC-D1 — "B closes; A sees it gone"; retire
    # tombstones, which is the propagation event). Pre-migration
    # orders have no note; a down daemon is a named notice. Either
    # way the FILE close stands.
    ORDER_ID="$ID" ORDER_STATE="$STATE" CO_DIR="$CO_DIR" "$PY" - <<'PY'
import os, re, sys
sys.path.insert(0, os.environ["CO_DIR"])
from co_notes import read_notes, retire_note, NotesDaemonError
try:
    env = read_notes(kinds=["decision"], limit=100, include_operational=True)
except Exception as exc:
    print(f"co-order: notes daemon unreachable — close stays local (a peer seat will still see the order open): {exc}",
          file=sys.stderr)
    sys.exit(0)
target = os.environ["ORDER_ID"]
state = os.environ["ORDER_STATE"]
for n in env.get("notes", []):
    if (n.get("related_entity") or "").lower() != "order-seat":
        continue
    m = re.search(r"^order:\s*(\S+)", n.get("content") or "", re.M)
    if m and m.group(1) == target:
        try:
            retire_note(n["id"], f"order {target} closed: {state}")
            print(f"co-order: retired mesh note {n['id']} — peers will see the order gone", file=sys.stderr)
        except NotesDaemonError as exc:
            print(f"co-order: could not retire mesh note {n['id']}: {exc}", file=sys.stderr)
        break
PY
    ;;

  *) usage ;;
esac
