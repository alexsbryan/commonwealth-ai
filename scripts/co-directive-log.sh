#!/usr/bin/env bash
# co-directive-log.sh — the director's supervision log (M0).
#
# Every directive the director wants to send passes through the
# operator as a typed draft; the (draft, final) pair lands here. The
# per-kind EDIT RATE is the disengagement metric that flips M1
# (docs/COMAINTAINER.md §7; ledger row "Comaintainer director M0").
#
#   scripts/co-directive-log.sh --worker W --kind order|steer|review|briefing|decision \
#       --draft "text" --final "text" [--citations "ARCH §14,note ab12cd34"] \
#       [--edit-class scope|tone|content|none]
#
# Draft-time logging (artifact D, docs/FIELD_VERDICTS.md §4 loop 1 —
# the queue's substrate, and the decision-to-send latency metric):
#   scripts/co-directive-log.sh --pending --kind order --draft "text" \
#       [--worker W] [--citations ...]          # prints the directive id
#   scripts/co-directive-log.sh --resolve <id> --final "text" \
#       [--edit-class ...]                      # operator acted on the draft
# The one-shot (--draft + --final) form keeps working; records without a
# `status` field read as resolved-at-write.
#
#   scripts/co-directive-log.sh --stats     # per-kind edit rate + pending
#                                           # + decision-to-send latency
set -uo pipefail

# Overridable so tests never contaminate the real edit-rate metric, and
# non-default homes are a supported shape, not a patch.
LOG="${CO_DIRECTIVE_LOG:-$HOME/.sovereign/comaintainer/directives.jsonl}"
mkdir -p "$(dirname "$LOG")"

if [ "${1:-}" = "--stats" ]; then
  python3 - "$LOG" <<'PY'
import json, sys, collections
from pathlib import Path
log = Path(sys.argv[1])
if not log.exists():
    print("no directives logged yet"); raise SystemExit(0)
rows = [json.loads(l) for l in log.read_text().splitlines() if l.strip()]
# Three record shapes share the file: legacy one-shot (no status) =
# resolved-at-write; status=pending; status=resolved (joins on id).
pend = {r["id"]: r for r in rows if r.get("status") == "pending" and "id" in r}
res = [r for r in rows if r.get("status") == "resolved"]
completed = [r for r in rows if "status" not in r] + res
open_ids = set(pend) - {r.get("id") for r in res}
lat = collections.defaultdict(list)  # kind -> [seconds pending->resolved]
import datetime
for r in res:
    p = pend.get(r.get("id"))
    if p:
        try:
            dt = (datetime.datetime.fromisoformat(r["ts"])
                  - datetime.datetime.fromisoformat(p["ts"])).total_seconds()
            lat[p.get("kind", "?")].append(dt)
        except ValueError:
            pass
by_kind = collections.defaultdict(lambda: [0, 0])
for r in completed:
    k = r.get("kind") or pend.get(r.get("id"), {}).get("kind", "?")
    by_kind[k][0] += 1
    by_kind[k][1] += 1 if r.get("edited") else 0
pending_by_kind = collections.defaultdict(int)
for i in open_ids:
    pending_by_kind[pend[i].get("kind", "?")] += 1


def med(xs):
    if not xs:
        return "-"
    xs = sorted(xs)
    m = xs[len(xs) // 2]
    return f"{m:.0f}s" if m < 600 else f"{m/60:.0f}m"


print(f"{'kind':<10} {'n':>4} {'edited':>7} {'edit rate':>10} "
      f"{'pending':>8} {'latency~':>9}")
for k in sorted(set(by_kind) | set(pending_by_kind)):
    n, e = by_kind.get(k, (0, 0))
    rate = f"{100*e/n:>9.1f}%" if n else f"{'-':>10}"
    print(f"{k:<10} {n:>4} {e:>7} {rate} "
          f"{pending_by_kind.get(k, 0):>8} {med(lat.get(k, [])):>9}")
n = len(completed); e = sum(1 for r in completed if r.get("edited"))
all_rate = f"{100*e/n:>9.1f}%" if n else f"{'-':>10}"
all_lat = med([x for xs in lat.values() for x in xs])
print(f"{'ALL':<10} {n:>4} {e:>7} {all_rate} {len(open_ids):>8} {all_lat:>9}")
PY
  exit 0
fi

WORKER="" KIND="" DRAFT="" FINAL="" CITATIONS="" EDIT_CLASS=""
MODE="" RESOLVE_ID=""
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
    *) echo "co-directive-log: unknown arg $1" >&2; exit 2 ;;
  esac
done

case "$MODE" in
  pending)
    case "$KIND" in order|steer|review|briefing|decision) ;; *)
      echo "co-directive-log: --kind must be order|steer|review|briefing|decision" >&2; exit 2 ;;
    esac
    [ -n "$DRAFT" ] || { echo "co-directive-log: --pending requires --draft" >&2; exit 2; }
    [ -z "$FINAL" ] || { echo "co-directive-log: --pending takes no --final (that is --resolve's job)" >&2; exit 2; }
    ;;
  resolve)
    [ -n "$FINAL" ] || { echo "co-directive-log: --resolve requires --final" >&2; exit 2; }
    [ -z "$DRAFT" ] || { echo "co-directive-log: --resolve takes no --draft (it is on the pending record)" >&2; exit 2; }
    ;;
  *)
    case "$KIND" in order|steer|review|briefing|decision) ;; *)
      echo "co-directive-log: --kind must be order|steer|review|briefing|decision" >&2; exit 2 ;;
    esac
    [ -n "$DRAFT" ] && [ -n "$FINAL" ] || {
      echo "co-directive-log: --draft and --final are required" >&2; exit 2; }
    ;;
esac

REPO="$(cd "$(dirname "$0")/.." && pwd)"
MODE="$MODE" RESOLVE_ID="$RESOLVE_ID" \
WORKER="$WORKER" KIND="$KIND" DRAFT="$DRAFT" FINAL="$FINAL" \
CITATIONS="$CITATIONS" EDIT_CLASS="$EDIT_CLASS" LOG="$LOG" \
CO_CHARTER="${CO_CHARTER:-$REPO/gym/comaintainer/CHARTER.md}" python3 - <<'PY'
import json, os, datetime, hashlib, sys
from pathlib import Path
mode = os.environ["MODE"]
log = os.environ["LOG"]
now = datetime.datetime.now(datetime.timezone.utc).isoformat()
charter_p = Path(os.environ["CO_CHARTER"])
sha = hashlib.sha256(charter_p.read_bytes()).hexdigest() if charter_p.exists() else None
cits = [c.strip() for c in os.environ["CITATIONS"].split(",") if c.strip()]

if mode == "pending":
    draft = os.environ["DRAFT"]
    did = hashlib.sha256(
        (now + os.environ["KIND"] + draft).encode()).hexdigest()[:8]
    rec = {"id": did, "ts": now, "status": "pending",
           "worker": os.environ["WORKER"] or None,
           "kind": os.environ["KIND"], "draft": draft,
           "citations": cits, "charter_sha256": sha}
    with open(log, "a") as fh:
        fh.write(json.dumps(rec, ensure_ascii=False) + "\n")
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
           "edited": pend.get("draft", "").strip() != final.strip(),
           "edit_class": os.environ["EDIT_CLASS"] or None}
    with open(log, "a") as fh:
        fh.write(json.dumps(rec, ensure_ascii=False) + "\n")
    print(f"resolved {rec['kind']} directive {did} "
          f"(edited={rec['edited']}) -> {log}")
else:
    draft, final = os.environ["DRAFT"], os.environ["FINAL"]
    rec = {"ts": now,
           "worker": os.environ["WORKER"] or None,
           "kind": os.environ["KIND"],
           "draft": draft, "final": final,
           "edited": draft.strip() != final.strip(),
           "edit_class": os.environ["EDIT_CLASS"] or None,
           "citations": cits,
           "charter_sha256": sha}
    with open(log, "a") as fh:
        fh.write(json.dumps(rec, ensure_ascii=False) + "\n")
    print(f"logged {rec['kind']} directive (edited={rec['edited']}) -> {log}")
PY
