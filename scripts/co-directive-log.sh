#!/usr/bin/env bash
# co-directive-log.sh — the director's supervision log (M0).
#
# Every directive the director wants to send passes through the
# operator as a typed draft; the (draft, final) pair lands here. The
# per-kind EDIT RATE is the disengagement metric that flips M1
# (docs/COMAINTAINER.md §7; ledger row "Comaintainer director M0").
#
#   scripts/co-directive-log.sh --worker W --kind order|steer|review|briefing \
#       --draft "text" --final "text" [--citations "ARCH §14,note ab12cd34"] \
#       [--edit-class scope|tone|content|none]
#   scripts/co-directive-log.sh --stats     # per-kind edit rate (one home)
set -uo pipefail

LOG="$HOME/.sovereign/comaintainer/directives.jsonl"
mkdir -p "$(dirname "$LOG")"

if [ "${1:-}" = "--stats" ]; then
  python3 - "$LOG" <<'PY'
import json, sys, collections
from pathlib import Path
log = Path(sys.argv[1])
if not log.exists():
    print("no directives logged yet"); raise SystemExit(0)
rows = [json.loads(l) for l in log.read_text().splitlines() if l.strip()]
by_kind = collections.defaultdict(lambda: [0, 0])
for r in rows:
    by_kind[r.get("kind", "?")][0] += 1
    by_kind[r.get("kind", "?")][1] += 1 if r.get("edited") else 0
print(f"{'kind':<10} {'n':>4} {'edited':>7} {'edit rate':>10}")
for k in sorted(by_kind):
    n, e = by_kind[k]
    print(f"{k:<10} {n:>4} {e:>7} {100*e/n:>9.1f}%")
n = len(rows); e = sum(1 for r in rows if r.get("edited"))
print(f"{'ALL':<10} {n:>4} {e:>7} {100*e/n:>9.1f}%")
PY
  exit 0
fi

WORKER="" KIND="" DRAFT="" FINAL="" CITATIONS="" EDIT_CLASS=""
while [ $# -gt 0 ]; do
  case "$1" in
    --worker) WORKER="${2:?}"; shift 2 ;;
    --kind) KIND="${2:?}"; shift 2 ;;
    --draft) DRAFT="${2:?}"; shift 2 ;;
    --final) FINAL="${2:?}"; shift 2 ;;
    --citations) CITATIONS="${2:-}"; shift 2 ;;
    --edit-class) EDIT_CLASS="${2:-}"; shift 2 ;;
    *) echo "co-directive-log: unknown arg $1" >&2; exit 2 ;;
  esac
done
case "$KIND" in order|steer|review|briefing) ;; *)
  echo "co-directive-log: --kind must be order|steer|review|briefing" >&2; exit 2 ;;
esac
[ -n "$DRAFT" ] && [ -n "$FINAL" ] || {
  echo "co-directive-log: --draft and --final are required" >&2; exit 2; }

REPO="$(cd "$(dirname "$0")/.." && pwd)"
WORKER="$WORKER" KIND="$KIND" DRAFT="$DRAFT" FINAL="$FINAL" \
CITATIONS="$CITATIONS" EDIT_CLASS="$EDIT_CLASS" LOG="$LOG" \
CO_CHARTER="${CO_CHARTER:-$REPO/gym/comaintainer/CHARTER.md}" python3 - <<'PY'
import json, os, datetime, hashlib
from pathlib import Path
draft, final = os.environ["DRAFT"], os.environ["FINAL"]
charter_p = Path(os.environ["CO_CHARTER"])
sha = hashlib.sha256(charter_p.read_bytes()).hexdigest() if charter_p.exists() else None
rec = {"ts": datetime.datetime.now(datetime.timezone.utc).isoformat(),
       "worker": os.environ["WORKER"] or None,
       "kind": os.environ["KIND"],
       "draft": draft, "final": final,
       "edited": draft.strip() != final.strip(),
       "edit_class": os.environ["EDIT_CLASS"] or None,
       "citations": [c.strip() for c in os.environ["CITATIONS"].split(",") if c.strip()],
       "charter_sha256": sha}
with open(os.environ["LOG"], "a") as fh:
    fh.write(json.dumps(rec, ensure_ascii=False) + "\n")
print(f"logged {rec['kind']} directive (edited={rec['edited']}) -> {os.environ['LOG']}")
PY
