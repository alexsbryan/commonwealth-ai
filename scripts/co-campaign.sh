#!/usr/bin/env bash
# co-campaign.sh — the artifact above the order: one spec, one approval.
#
# WHY IT EXISTS (operator, 2026-08-16): "I spend 99% of my attention just
# rubber stamping the seat because it's doing pretty well." The seat's
# intake ran PER ORDER, so a six-rung spec cost six interviews and six
# approvals, five of which carried no new information — the spec had
# already answered them. The campaign moves the supervision moment UP: the
# operator approves the LADDER once, and the orders under it are drafted,
# spawned, steered and landed against that approval.
#
# It is the same shape as the order (co-order.sh) one altitude higher, and
# it obeys the same three house rules:
#   - ONE FILE is the truth. .sovereign/features/<id>/campaign.md, gitignored
#     per-host coordination like orders. Hand-editing is always valid.
#   - GENTLE. A session without a campaign behaves exactly as before this
#     script existed. `check` is advisory; nothing gates on it.
#   - THE BARS LIVE ELSEWHERE. quality/initiative-bars.toml declares them
#     (with floor/target/lane/noise_band); co-lineage.py renders them. This
#     file POINTS at bar ids — it never restates a threshold, because two
#     copies of one threshold is #8 with extra steps.
#
# WHAT THE OPERATOR IS ACTUALLY APPROVING, and why each section is here:
#   Ladder            the rungs, in order, each naming the bars it moves.
#   Ambiguity policy  the axes this spec is genuinely ambiguous on and WHICH
#                     PRINCIPLE decides each. Without it a worker has two
#                     moves — guess (drift) or ask (your attention) — and
#                     "resolve it with the principles of the app" is not
#                     actionable. Every call made under it appends to the
#                     Decisions log for close-out triage.
#   Tuning            the bounded loop a worker may run on a near miss
#                     WITHOUT asking: iteration cap, dev/holdout split, and
#                     the knob whitelist. A knob outside the list is a design
#                     change, not a tune — that escalates.
#   Stop conditions   what wakes the operator. This is the whole difference
#                     between autonomous and unsupervised: you should be able
#                     to state, before it starts, exactly what ends it.
#   Banking           where in-flight findings go instead of the operator's
#                     console (the backlog, via co-backlog-producer.sh).
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
# serves: <initiative-id> — the initiative in quality/initiative-bars.toml
# whose bars this campaign's rungs move. Same vocabulary as an order's
# \`serves:\`; the campaign names the INITIATIVE, each rung names the BARS.
serves: (unattributed)
---

# Campaign: $TITLE

## Spec

<!-- The committed document this campaign executes. If it declares no
     falsifiable bars, THAT is the first finding — say so here and fix the
     spec before drafting rungs. Bars are transcribed, never invented. -->

(none)

## Ladder

<!-- The rungs, in landing order. One line each: id, what it delivers, the
     bar ids it moves. A rung that moves no declared bar is either
     scaffolding (say so) or a bar nobody declared (fix the bars file).
     The operator approves THIS — the orders under it are drafted from it
     without a second approval each. -->

| # | rung id | delivers | serves bars |
|---|---------|----------|-------------|
| 1 |         |          |             |

## Ambiguity policy

<!-- The axes this spec does NOT settle, and which principle decides each.
     Two or three, not twenty — an axis that never comes up costs a line,
     an axis that comes up unlisted costs an escalation. Example shape:
       - Storage vs. recompute -> ARCH §19 (inventory outranks the plan):
         reuse the existing store unless a measurement says it cannot serve.
       - Absence handling -> the eleven #6: refuse and name it; never default.
     Every call made under this policy appends to Decisions below. -->

(none — every ambiguity escalates)

## Tuning

<!-- The bounded loop a worker may run on a near miss WITHOUT asking.
     Cap:        iterations or wall-clock, spent from the rung's budget
     Split:      which set is dev, which is holdout. Tuning on the set that
                 renders the verdict is fitting the bar, and it makes
                 yellow->green meaningless.
     Knobs:      the whitelist. Anything outside it is a design change.
     Terminates with one of four, always: reached-target / stalled-at-floor
     (emit the curve) / instrument-is-the-problem / floor-breached. A
     documented stall is a RESULT; stopping to ask is not. -->

Cap:    (none — no tuning without asking)
Split:  (none)
Knobs:  (none)

## Near-miss protocol

<!-- STANDING, do not edit per campaign — copied into every spawn prompt.
     Order matters; step 0 is first for a reason.
     0. Is the delta inside the lane's noise_band (bars file / RUNBOOK §6)?
        If yes it is weather, not a miss: verdict could-not-judge, re-run
        n=3 (§18.5 — one run is not a measurement), proceed.
     1. Above floor, below target? Run the bounded tune above. Then record
        \`met\` or \`met-floor\` + file the debt, and PROCEED to the next rung.
     2. Below floor? Stop. Escalate with the curve and the evidence.
     3. Instrument cannot resolve it? could-not-judge; escalate the
        INSTRUMENT, not the result (§18.4).
     A worker NEVER moves a target. That is operator-only (§18.6) and it is
     the only reason yellow is safe to grant. -->

standing (see quality/initiative-bars.toml header + .claude/skills/comaintainer/SKILL.md)

## Banking

<!-- Where in-flight findings go instead of the operator. Default:
     scripts/co-backlog-producer.sh --key <essence> ... — filed, scored,
     unvetted, one item per key. Anything outside the rung's Scope banks;
     it is deferred, not suppressed, and it is the close-out triage list.
     Only the Stop conditions below reach the operator mid-campaign. -->

backlog (co-backlog-producer.sh), keyed by essence

## Stop conditions

<!-- What wakes the operator. Be able to state this BEFORE it starts.
     House defaults, edit freely:
       - the spec's premise is falsified by evidence
       - a bar needs re-registering, or a target needs moving
       - a floor is breached
       - N consecutive rungs land yellow on the same bar, or a yellow goes
         past its review_by
       - budget exhausted
       - a commons/irreversible action (daemon wedge, force-push, spend cap)
     -->

(house defaults)

## Budget

<!-- Sessions, model calls, wall-clock, spend. The campaign's, not a rung's. -->

(none)

## Engine ladder

<!-- Model + effort per rung. Recorded taste: solid plan + brute-force ->
     opus/medium; hard design -> fable/high. Edits here are training data. -->

(none)

## Decisions

<!-- Appended by the seat as the campaign runs: every call made under the
     Ambiguity policy, one line each, dated, with the principle cited. This
     is what the operator reads at close-out instead of re-deriving the
     campaign. It is a LOG of what happened, not a plan. -->

## Close-out

<!-- Filled at the end: bars met / yellow (with debts) / failed /
     could-not-judge, backlog items filed, decisions taken, overrides. One
     page. The polish triage. -->
EOF
    echo "co-campaign: wrote $F"
    echo "  next: fill Spec + Ladder, then show the operator ONE draft (kind=order,"
    echo "        co-directive-log.sh) — the campaign approval replaces the per-rung ones."
    ;;

  list)
    found=0
    for f in "$FEATURES"/*/campaign.md; do
      [ -e "$f" ] || continue
      id=$(basename "$(dirname "$f")")
      status=$(awk -F': *' '/^status:/{print $2; exit}' "$f")
      appr=$(awk -F': *' '/^approved:/{print $2; exit}' "$f")
      title=$(awk '/^# Campaign: /{sub(/^# Campaign: /,""); print; exit}' "$f")
      [ "$status" = "open" ] || continue
      found=1
      printf '%-28s %-10s approved:%-12s %s\n' "$id" "$status" "$appr" "$title"
    done
    [ "$found" = 0 ] && echo "co-campaign: no open campaigns"
    ;;

  check)
    ID="${1:?usage: co-campaign.sh check <id>}"
    F="$(_file "$ID")"
    [ -e "$F" ] || { echo "co-campaign: no campaign $ID"; exit 2; }
    echo "co-campaign check $ID (advisory — nothing gates on this)"
    # A section left "(none)" is LEGAL and stays VISIBLE. The point is that
    # the operator sees which supervision they are declining, not that the
    # script blocks them (#6: absence reported, never defaulted).
    # The prose between <!-- and --> is INSTRUCTIONS, not content. A
    # line-at-a-time filter drops only the delimiters and then reads the
    # template's own guidance back as a filled-in section — a check that
    # cannot report UNSET is not a check (#5).
    python3 - "$F" <<'PY'
import re, sys
text = open(sys.argv[1], encoding="utf-8").read()
text = re.sub(r"<!--.*?-->", "", text, flags=re.S)
sections = dict(re.findall(r"^## (.+?)$\n(.*?)(?=^## |\Z)", text, flags=re.S | re.M))
for sec in ("Spec", "Ladder", "Ambiguity policy", "Tuning", "Stop conditions", "Budget"):
    body = " ".join(sections.get(sec, "").split())
    # A ladder whose only row is the empty template row is not a ladder.
    empty = (not body) or "(none" in body or re.fullmatch(r"\|[\s|#-]*\|[\s|-]*\|[\s|]*", body or "")
    if sec == "Ladder" and body:
        rows = [r for r in sections["Ladder"].strip().splitlines()
                if r.startswith("|") and not set(r) <= set("|- ") and "rung id" not in r]
        empty = not any(len(c.strip()) for r in rows for c in r.split("|")[2:3])
    print(f"  {'UNSET  ' if empty else 'set    '} {sec}"
          + (" — legal, and it means every call on this axis escalates" if empty else ""))
PY
    serves=$(awk -F': *' '/^serves:/{print $2; exit}' "$F")
    echo "  serves: ${serves:-(absent)}"
    if [ "${serves:-}" != "(unattributed)" ] && [ -n "${serves:-}" ]; then
      python3 "$REPO/scripts/co-lineage.py" coverage "$serves" 2>/dev/null | sed -n '5,9p' | sed 's/^/  | /'
    fi
    ;;

  close)
    ID="${1:?usage: co-campaign.sh close <id> [landed|abandoned]}"; shift || true
    HOW="${1:-landed}"
    F="$(_file "$ID")"
    [ -e "$F" ] || { echo "co-campaign: no campaign $ID"; exit 2; }
    python3 - "$F" "$HOW" "$TODAY" <<'PY'
import sys, re
path, how, today = sys.argv[1], sys.argv[2], sys.argv[3]
text = open(path, encoding="utf-8").read()
text = re.sub(r"^status: .*$", f"status: {how}", text, count=1, flags=re.M)
text = text.rstrip() + f"\n\nclosed: {today} ({how})\n"
open(path, "w", encoding="utf-8").write(text)
PY
    echo "co-campaign: $ID closed ($HOW)"
    echo "  the bars do NOT close with it — transition them in quality/initiative-bars.toml,"
    echo "  and remember a yellow bar stays OPEN carrying its debt."
    ;;

  *) usage ;;
esac
