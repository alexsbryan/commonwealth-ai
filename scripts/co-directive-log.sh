#!/usr/bin/env bash
# co-directive-log.sh — the director's supervision log (M0).
#
# Every directive the director wants to send passes through the
# operator as a typed draft; the (draft, final) pair lands here. The
# per-kind EDIT RATE is the disengagement metric that flips M1
# (docs/COMAINTAINER.md §7; ledger row "Comaintainer director M0").
#
# THE EDIT VERDICT IS DECLARED, NOT INFERRED.
#   Until 2026-08-10 `edited` was computed as `draft.strip() != final.strip()`.
#   Seat practice writes the final as a RESOLUTION SUMMARY ("approved
#   verbatim, no edits"), so string inequality measured the seat's prose
#   convention, not the operator: 77 of 79 rows read edited, of which at
#   least 34 said in words that the operator changed nothing (seat
#   evaluation note e10b02a8). One decider, one name (ARCH §10.6): the
#   seat STATES the verdict with an explicit flag and `--stats` reads
#   only that. There is no default — a resolve without a flag is an
#   error, because the seat is the only party that knows.
#
#   scripts/co-directive-log.sh --worker W --kind order|steer|review|briefing|decision \
#       --draft "text" --final "text" (--edited|--unedited|--no-decision) \
#       [--citations "ARCH §14,note ab12cd34"] [--edit-class scope|tone|content|none]
#
# Draft-time logging (artifact D, docs/FIELD_VERDICTS.md §4 loop 1 —
# the queue's substrate, and the decision-to-send latency metric):
#   scripts/co-directive-log.sh --pending --kind order --draft "text" \
#       [--worker W] [--citations ...]          # prints the directive id
#   scripts/co-directive-log.sh --resolve <id> --final "text" \
#       (--edited|--unedited|--no-decision) [--edit-class ...]
# The one-shot (--draft + --final) form keeps working; records without a
# `status` field read as resolved-at-write.
#
# The three verdicts the seat may state (a closed set, ARCH §2):
#   --unedited     the operator let the drafted directive through as put
#                  forward (including ratifying the seat's recommendation)
#   --edited       the operator changed its substance, scope or direction
#   --no-decision  no operator decision was taken on this row at all
#                  (superseded before resolve, placeholder, seat self-check)
#                  — kept so a row with no decision is never forced into a
#                  false binary (ARCH §18.3)
#
#   scripts/co-directive-log.sh --stats           # per-kind edit rate + pending
#                                                 # + decision-to-send latency
#   scripts/co-directive-log.sh --unclassified    # rows carrying no verdict yet
#   scripts/co-directive-log.sh --annotate <key> --verdict <v> \
#       [--rationale "why"] [--method "how"]      # verdict for a row logged
#                                                 # before the flag existed
#
# THE LOG IS A RECORD: rows are appended, never rewritten. A verdict for
# a historical row therefore lands in a SIDECAR of annotation records
# (directive-edit-verdicts.jsonl) keyed by the row's identity — the
# directive id, or a content hash for the legacy one-shot rows that
# carry none. A later annotation for the same key supersedes an earlier
# one, and an annotation overrides a flag (it is the deliberate
# correction). The sidecar is separate from directives.jsonl so that the
# other reader of the record — scripts/co-closeout.py — cannot mistake
# an annotation for a directive.
set -uo pipefail

# Overridable so tests never contaminate the real edit-rate metric, and
# non-default homes are a supported shape, not a patch.
LOG="${CO_DIRECTIVE_LOG:-$HOME/.sovereign/comaintainer/directives.jsonl}"
LOGDIR="$(dirname "$LOG")"
ANNOT="${CO_DIRECTIVE_ANNOTATIONS:-$LOGDIR/directive-edit-verdicts.jsonl}"
mkdir -p "$LOGDIR"

# --- read side: --stats and --unclassified share one join -------------------
case "${1:-}" in
  --stats|--unclassified)
    CO_MODE="${1#--}" LOG="$LOG" ANNOT="$ANNOT" python3 - <<'PY'
import collections
import datetime
import hashlib
import json
import os
import sys
from pathlib import Path

MODE = os.environ["CO_MODE"]
log = Path(os.environ["LOG"])
annot = Path(os.environ["ANNOT"])

# Closed set (ARCH §2). "unclassified" is not in it: it is the ABSENCE
# of a verdict, reported as its own column and never folded into one.
VERDICTS = ("unedited", "edited", "no-decision", "indeterminate")


def read(path):
    """Malformed lines are reported, never silently dropped (ARCH §18.3)."""
    rows = []
    if not path.exists():
        return rows
    text = path.read_text(encoding="utf-8", errors="replace")
    for lineno, line in enumerate(text.splitlines(), 1):
        if not line.strip():
            continue
        try:
            rows.append(json.loads(line))
        except json.JSONDecodeError as exc:
            print(f"co-directive-log: {path}:{lineno} malformed JSON ({exc})",
                  file=sys.stderr)
    return rows


def row_key(row):
    """Identity from essence, never an address (ARCH §7.5). Resolved and
    pending records carry the directive id; the legacy one-shot records
    carry none, so their key is a hash of the content that IS the row."""
    if row.get("id"):
        return row["id"]
    seed = "|".join([row.get("ts", ""), row.get("kind", ""),
                     row.get("draft", ""), row.get("final", "")])
    return hashlib.sha256(seed.encode("utf-8")).hexdigest()[:12]


rows = read(log)
pend = {r["id"]: r for r in rows if r.get("status") == "pending" and "id" in r}
res = [r for r in rows if r.get("status") == "resolved"]
completed = [r for r in rows if "status" not in r] + res
open_ids = set(pend) - {r.get("id") for r in res}

# Annotations: append-only, last write for a key wins.
ann = {}
for a in read(annot):
    if a.get("key") and a.get("verdict") in VERDICTS:
        ann[a["key"]] = a


def verdict(row):
    """Four verdicts, not two (ARCH §18.2). An annotation is the
    deliberate correction and outranks the flag; `edited` is trusted
    ONLY when the record says it came from an explicit flag, because
    every row written before 2026-08-10 computed it from string
    inequality and that number means nothing."""
    a = ann.get(row_key(row))
    if a:
        return a["verdict"]
    if row.get("edited_source") == "flag":
        v = row.get("edit_verdict")
        if v in VERDICTS:
            return v
        return "edited" if row.get("edited") else "unedited"
    return "unclassified"


def kind_of(row):
    return row.get("kind") or pend.get(row.get("id"), {}).get("kind", "?")


if MODE == "unclassified":
    n = 0
    for r in completed:
        if verdict(r) != "unclassified":
            continue
        n += 1
        final = " ".join((r.get("final") or "").split())[:96]
        print(f"{row_key(r)}\t{kind_of(r):<9}\t{r.get('ts','')[:19]}\t{final}")
    print(f"# {n} of {len(completed)} completed rows carry no edit verdict",
          file=sys.stderr)
    raise SystemExit(0)

# --- stats ------------------------------------------------------------------
lat = collections.defaultdict(list)  # kind -> [seconds pending->resolved]
for r in res:
    p = pend.get(r.get("id"))
    if p:
        try:
            dt = (datetime.datetime.fromisoformat(r["ts"])
                  - datetime.datetime.fromisoformat(p["ts"])).total_seconds()
            lat[p.get("kind", "?")].append(dt)
        except ValueError:
            pass

tally = collections.defaultdict(collections.Counter)  # kind -> verdict counts
for r in completed:
    tally[kind_of(r)][verdict(r)] += 1
pending_by_kind = collections.Counter(
    pend[i].get("kind", "?") for i in open_ids)


def med(xs):
    if not xs:
        return "-"
    xs = sorted(xs)
    m = xs[len(xs) // 2]
    return f"{m:.0f}s" if m < 600 else f"{m/60:.0f}m"


def row_out(label, c, pending, latency):
    decided = c["edited"] + c["unedited"]
    rate = f"{100*c['edited']/decided:>9.1f}%" if decided else f"{'-':>10}"
    print(f"{label:<10} {decided:>4} {c['edited']:>7} {rate} "
          f"{c['indeterminate']:>6} {c['no-decision']:>6} "
          f"{c['unclassified']:>8} {pending:>8} {latency:>9}")


print(f"{'kind':<10} {'n':>4} {'edited':>7} {'edit rate':>10} "
      f"{'indet':>6} {'nodec':>6} {'unclass':>8} {'pending':>8} "
      f"{'latency~':>9}")
for k in sorted(set(tally) | set(pending_by_kind)):
    row_out(k, tally.get(k, collections.Counter()),
            pending_by_kind.get(k, 0), med(lat.get(k, [])))
total = collections.Counter()
for c in tally.values():
    total.update(c)
row_out("ALL", total, len(open_ids),
        med([x for xs in lat.values() for x in xs]))
print()
print("n = rows with a stated verdict (edited + unedited); the edit rate's "
      "denominator.")
print("indet / nodec / unclass are EXCLUDED from n and shown so the "
      "denominator is never quietly widened:")
print("  indet   — classified, but the record cannot say whether the "
      "operator changed it")
print("  nodec   — no operator decision on the row (superseded, "
        "placeholder, seat self-check)")
print("  unclass — no verdict at all yet; `--unclassified` lists them")
PY
    exit $?
    ;;
esac

WORKER="" KIND="" DRAFT="" FINAL="" CITATIONS="" EDIT_CLASS=""
MODE="" RESOLVE_ID="" VERDICT="" ANN_KEY="" RATIONALE="" METHOD=""
while [ $# -gt 0 ]; do
  case "$1" in
    --worker) WORKER="${2:?}"; shift 2 ;;
    --kind) KIND="${2:?}"; shift 2 ;;
    --draft) DRAFT="${2:?}"; shift 2 ;;
    --final) FINAL="${2:?}"; shift 2 ;;
    --citations) CITATIONS="${2:-}"; shift 2 ;;
    --edit-class) EDIT_CLASS="${2:-}"; shift 2 ;;
    --pending) MODE="pending"; shift ;;
    --resolve) MODE="resolve"; RESOLVE_ID="${2:?}"; shift 2 ;;
    --annotate) MODE="annotate"; ANN_KEY="${2:?}"; shift 2 ;;
    --verdict) VERDICT="${2:?}"; shift 2 ;;
    --rationale) RATIONALE="${2:-}"; shift 2 ;;
    --method) METHOD="${2:-}"; shift 2 ;;
    --edited) VERDICT="edited"; shift ;;
    --unedited) VERDICT="unedited"; shift ;;
    --no-decision) VERDICT="no-decision"; shift ;;
    *) echo "co-directive-log: unknown arg $1" >&2; exit 2 ;;
  esac
done

# The seat, resolving live, may state three verdicts. A retrospective
# annotation may state a fourth — `indeterminate` — because a classifier
# reading a record written years-of-sessions ago can honestly fail to
# tell, and the seat resolving in the moment cannot (ARCH §18.2).
require_verdict() {
  case "$VERDICT" in
    edited|unedited|no-decision) ;;
    indeterminate)
      [ "$MODE" = "annotate" ] || {
        echo "co-directive-log: --verdict indeterminate is for --annotate only — at resolve time the seat was there and knows" >&2; exit 2; } ;;
    "") echo "co-directive-log: $1 requires one of --edited | --unedited | --no-decision (no default: only the seat knows whether the operator changed the draft)" >&2; exit 2 ;;
    *) echo "co-directive-log: --verdict must be edited|unedited|no-decision (or indeterminate, for --annotate); got '$VERDICT'" >&2; exit 2 ;;
  esac
}

case "$MODE" in
  pending)
    case "$KIND" in order|steer|review|briefing|decision) ;; *)
      echo "co-directive-log: --kind must be order|steer|review|briefing|decision" >&2; exit 2 ;;
    esac
    [ -n "$DRAFT" ] || { echo "co-directive-log: --pending requires --draft" >&2; exit 2; }
    [ -z "$FINAL" ] || { echo "co-directive-log: --pending takes no --final (that is --resolve's job)" >&2; exit 2; }
    [ -z "$VERDICT" ] || { echo "co-directive-log: --pending takes no edit verdict (the operator has not decided yet)" >&2; exit 2; }
    ;;
  resolve)
    [ -n "$FINAL" ] || { echo "co-directive-log: --resolve requires --final" >&2; exit 2; }
    [ -z "$DRAFT" ] || { echo "co-directive-log: --resolve takes no --draft (it is on the pending record)" >&2; exit 2; }
    require_verdict "--resolve"
    ;;
  annotate)
    require_verdict "--annotate"
    ;;
  *)
    case "$KIND" in order|steer|review|briefing|decision) ;; *)
      echo "co-directive-log: --kind must be order|steer|review|briefing|decision" >&2; exit 2 ;;
    esac
    [ -n "$DRAFT" ] && [ -n "$FINAL" ] || {
      echo "co-directive-log: --draft and --final are required" >&2; exit 2; }
    require_verdict "the one-shot form"
    ;;
esac

REPO="$(cd "$(dirname "$0")/.." && pwd)"
MODE="$MODE" RESOLVE_ID="$RESOLVE_ID" VERDICT="$VERDICT" \
ANN_KEY="$ANN_KEY" RATIONALE="$RATIONALE" METHOD="$METHOD" \
WORKER="$WORKER" KIND="$KIND" DRAFT="$DRAFT" FINAL="$FINAL" \
CITATIONS="$CITATIONS" EDIT_CLASS="$EDIT_CLASS" LOG="$LOG" ANNOT="$ANNOT" \
CO_CHARTER="${CO_CHARTER:-$REPO/gym/comaintainer/CHARTER.md}" python3 - <<'PY'
import json, os, datetime, hashlib, sys
from pathlib import Path
mode = os.environ["MODE"]
log = os.environ["LOG"]
now = datetime.datetime.now(datetime.timezone.utc).isoformat()
charter_p = Path(os.environ["CO_CHARTER"])
sha = hashlib.sha256(charter_p.read_bytes()).hexdigest() if charter_p.exists() else None
cits = [c.strip() for c in os.environ["CITATIONS"].split(",") if c.strip()]
verdict = os.environ["VERDICT"]

# `edited` is a DERIVED MIRROR of the verdict, kept because
# scripts/co-closeout.py renders it off the resolved record. The verdict
# is the decider; --stats never reads this field except through
# `edited_source` (ARCH §10.6 — one decider, one name).
edited_mirror = {"edited": True, "unedited": False}.get(verdict)


def append(path, rec):
    with open(path, "a") as fh:
        fh.write(json.dumps(rec, ensure_ascii=False) + "\n")


if mode == "annotate":
    rec = {"ts": now, "key": os.environ["ANN_KEY"], "verdict": verdict,
           "method": os.environ["METHOD"] or None,
           "rationale": os.environ["RATIONALE"] or None}
    append(os.environ["ANNOT"], rec)
    print(f"annotated {rec['key']} verdict={verdict} -> {os.environ['ANNOT']}")
elif mode == "pending":
    draft = os.environ["DRAFT"]
    did = hashlib.sha256(
        (now + os.environ["KIND"] + draft).encode()).hexdigest()[:8]
    rec = {"id": did, "ts": now, "status": "pending",
           "worker": os.environ["WORKER"] or None,
           "kind": os.environ["KIND"], "draft": draft,
           "citations": cits, "charter_sha256": sha}
    append(log, rec)
    print(did)
elif mode == "resolve":
    did = os.environ["RESOLVE_ID"]
    rows = []
    if Path(log).exists():
        rows = [json.loads(l) for l in Path(log).read_text().splitlines()
                if l.strip()]
    pend = next((r for r in rows
                 if r.get("status") == "pending" and r.get("id") == did), None)
    if pend is None:
        print(f"co-directive-log: no pending directive with id {did}",
              file=sys.stderr)
        raise SystemExit(2)
    if any(r.get("status") == "resolved" and r.get("id") == did for r in rows):
        print(f"co-directive-log: directive {did} is already resolved",
              file=sys.stderr)
        raise SystemExit(2)
    final = os.environ["FINAL"]
    rec = {"id": did, "ts": now, "status": "resolved",
           "kind": pend.get("kind"), "final": final,
           "edit_verdict": verdict, "edited": edited_mirror,
           "edited_source": "flag",
           "edit_class": os.environ["EDIT_CLASS"] or None}
    append(log, rec)
    print(f"resolved {rec['kind']} directive {did} "
          f"(verdict={verdict}) -> {log}")
else:
    draft, final = os.environ["DRAFT"], os.environ["FINAL"]
    rec = {"ts": now,
           "worker": os.environ["WORKER"] or None,
           "kind": os.environ["KIND"],
           "draft": draft, "final": final,
           "edit_verdict": verdict, "edited": edited_mirror,
           "edited_source": "flag",
           "edit_class": os.environ["EDIT_CLASS"] or None,
           "citations": cits,
           "charter_sha256": sha}
    append(log, rec)
    print(f"logged {rec['kind']} directive (verdict={verdict}) -> {log}")
PY
