#!/usr/bin/env bash
# co-campaign.sh — the artifact above the order: one spec, one approval.
#
# WHY (operator, 2026-08-16): per-order intake made a six-rung spec cost six
# interviews and six approvals, five carrying nothing the spec had not
# already said. The campaign moves the supervision moment up: the operator
# approves the LADDER once, and the orders under it run against it.
#
# Same contract as co-order.sh: one file
# (.sovereign/features/<id>/campaign.md, gitignored), hand-editing always
# valid, `check` advisory, a session without one behaves as before.
#
# FIVE SECTIONS, and each one is here because leaving it out costs the
# operator attention: Ladder (what the approval covers), Ambiguity policy
# (which principle decides an axis the spec left open — without it a worker
# can only guess or ask), Tuning (the bounded loop it may run on a near miss
# without asking), Stop conditions (what wakes you), Decisions (what you read
# at close-out). Thresholds are NOT here — bars live in
# quality/campaigns/<id>.toml and this file points at their ids (#8). The
# near-miss protocol and the banking rule are standing, not per campaign:
# .claude/skills/comaintainer/SKILL.md holds them.
#
#   scripts/co-campaign.sh new <id> [title…]   # write the template
#   scripts/co-campaign.sh list                # open campaigns, one line each
#   scripts/co-campaign.sh check <id>          # advisory completeness read
#   scripts/co-campaign.sh close <id> [landed|abandoned]
set -uo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
FEATURES="$REPO/.sovereign/features"
TODAY="$(date +%F)"

usage() { awk '/^#   scripts\/co-campaign\.sh /{p=1} p{print; if (/close <id>/) exit}' "$0"; exit 2; }
[ $# -ge 1 ] || usage
CMD="$1"; shift || true

_file() { echo "$FEATURES/$1/campaign.md"; }

case "$CMD" in
  new)
    ID="${1:?usage: co-campaign.sh new <id> [title…]}"; shift || true
    TITLE="${*:-$ID}"
    F="$(_file "$ID")"
    [ -e "$F" ] && { echo "co-campaign: $F already exists — edit it directly"; exit 2; }
    mkdir -p "$FEATURES/$ID"
    cat > "$F" <<EOF
---
schema: campaign/v1
id: $ID
status: open
drafted: $TODAY
approved: pending
serves: (unattributed)   # campaign id in quality/campaigns/
spec: (none)             # the committed doc this executes
budget: (none)           # sessions / wall-clock / spend
---

# Campaign: $TITLE

## Ladder

<!-- Rungs in landing order, each naming the bar ids it moves. This is what
     the operator approves; orders under it need no second approval. A rung
     moving no declared bar is scaffolding (say so) or a missing bar. -->

(none)

## Ambiguity policy

<!-- axis -> the principle that decides it. Two or three, not twenty.
     An unlisted axis escalates. Every call made here appends to Decisions.
     e.g. storage vs recompute -> §19: reuse unless a measurement says it
     cannot serve. -->

(none)

## Tuning

<!-- The bounded loop a worker may run on a near miss WITHOUT asking. Knobs
     outside the list are design changes and escalate. Tune on dev, judge on
     holdout. The near-miss protocol itself is standing (SKILL.md). -->

Cap: (none)   Split: (none)   Knobs: (none)

## Stop conditions

<!-- What wakes the operator. House defaults: premise falsified by evidence;
     a bar needs re-registering or a target moved; a floor breached; a yellow
     past review_by; budget out; a commons or irreversible action. -->

(house defaults)

## Decisions

<!-- Appended as it runs: date — the call — the principle cited. This is the
     close-out read. -->
EOF
    echo "co-campaign: wrote $F"
    echo "  fill Ladder + Ambiguity policy, then show the operator ONE draft"
    echo "  (co-directive-log.sh --kind order) — it replaces the per-rung approvals."
    ;;

  list)
    found=0
    for f in "$FEATURES"/*/campaign.md; do
      [ -e "$f" ] || continue
      status=$(awk -F': *' '/^status:/{print $2; exit}' "$f")
      [ "$status" = "open" ] || continue
      found=1
      printf '%-28s approved:%-10s %s\n' \
        "$(basename "$(dirname "$f")")" \
        "$(awk -F': *' '/^approved:/{print $2; exit}' "$f")" \
        "$(awk '/^# Campaign: /{sub(/^# Campaign: /,""); print; exit}' "$f")"
    done
    [ "$found" = 0 ] && echo "co-campaign: no open campaigns"
    ;;

  check)
    ID="${1:?usage: co-campaign.sh check <id>}"
    F="$(_file "$ID")"
    [ -e "$F" ] || { echo "co-campaign: no campaign $ID"; exit 2; }
    echo "co-campaign check $ID (advisory — nothing gates on this)"
    # The prose between <!-- and --> is INSTRUCTIONS. A line filter that drops
    # only the delimiters reads the template's own guidance back as content,
    # and a check that cannot report UNSET is not a check (#5).
    python3 - "$F" <<'PY'
import re, sys
text = re.sub(r"<!--.*?-->", "", open(sys.argv[1], encoding="utf-8").read(), flags=re.S)
head = dict(re.findall(r"^(serves|spec|budget): *(.+?) *(?:#.*)?$", text, flags=re.M))
body = dict(re.findall(r"^## (.+?)$\n(.*?)(?=^## |\Z)", text, flags=re.S | re.M))
# Decisions is a LOG, not a field — empty is its correct start state.
body.pop("Decisions", None)
rows = list(head.items()) + [(k, " ".join(v.split())) for k, v in body.items()]
unset = [n for n, v in rows if not v or "(none" in v or "(unattributed" in v]
for name, val in rows:
    print(f"  {'UNSET' if name in unset else 'set  '}  {name}")
if unset:
    print("  UNSET is legal and visible: those calls escalate instead of being")
    print("  pre-authorized by the approval. That is the trade, stated.")
PY
    ;;

  close)
    ID="${1:?usage: co-campaign.sh close <id> [landed|abandoned]}"; shift || true
    HOW="${1:-landed}"
    F="$(_file "$ID")"
    [ -e "$F" ] || { echo "co-campaign: no campaign $ID"; exit 2; }
    python3 - "$F" "$HOW" "$TODAY" <<'PY'
import re, sys
path, how, today = sys.argv[1:4]
text = open(path, encoding="utf-8").read()
text = re.sub(r"^status: .*$", f"status: {how}", text, count=1, flags=re.M)
open(path, "w", encoding="utf-8").write(text.rstrip() + f"\n\nclosed: {today} ({how})\n")
PY
    serves=$(awk -F': *' '/^serves:/{print $2}' "$F" | awk '{print $1}')
    echo "co-campaign: $ID closed ($HOW)"
    echo "  bars do NOT close with it — they move by measurement rows"
    echo "  (co-lineage.py measure); a yellow bar stays OPEN carrying its debt."
    [ -n "${serves:-}" ] && [ "$serves" != "(unattributed)" ] &&
      echo "  close-out read: scripts/co-lineage.py postmortem $serves"
    ;;

  *) usage ;;
esac
