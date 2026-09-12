#!/usr/bin/env bash
# feed_replay_bank.sh — put THIS WEEK'S episodes in the judge-replay bank.
#
# WHY. `svrn bench judge-replay` prices a judge change against recorded
# (claim, evidence window) pairs, and every one of them was recorded on
# 2026-08-13/14. A registered replay over a month-old bank prices a change
# against a world that has moved, and reports it with full confidence — the
# bar is `vl-bank-live` in quality/campaigns/verifier-loop.toml, and its
# denominator is the CLOCK, which nobody authors.
#
# THIS IS A SCHEDULING CHANGE, NOT A RETENTION ONE. The replayable half comes
# from bench runs with `SOVEREIGN_GATE_AUDIT_FORENSICS` pointed at a file —
# which is how all 1,049 existing records were made. Nothing about the privacy
# defaults moves: the flag stays unset everywhere else, and the ledger it
# writes stays on the machine that produced it (the records carry verbatim
# corpus text, so they are local-only by the same rule that made the flag
# default-unset).
#
# THE FLAG MUST BE SET IN THE PROCESS THAT RUNS THE GATE. `svrn chat ask` will
# NOT do — its turn runs on the daemon, so the knob would have to be exported
# where the daemon is launched (its own --help says so). `bench chaos-monkey
# run` drives the pipeline IN THIS PROCESS, which is why the feed is a bench
# run and why it needs no daemon restart.
#
#   sovereign/bench/chaos_monkey/feed_replay_bank.sh [--limit N] [--corpus ID]
#
# Exit 0 fed · 1 ran and recorded nothing · 4 could not run. The verdict is
# also said on the last stdout line (scripts/lib/judgement.py).
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../../.." && pwd)"
RESULTS="$HERE/results"
LIMIT=6
CORPUS="${CHAOS_CORPUS:-chaos-secret-agent}"

while [ $# -gt 0 ]; do
  case "$1" in
    --limit) LIMIT="$2"; shift 2 ;;
    --corpus) CORPUS="$2"; shift 2 ;;
    -h|--help) sed -n '2,30p' "$0"; exit 0 ;;
    *) echo "feed_replay_bank: unknown flag $1" >&2; exit 2 ;;
  esac
done

judge() { python3 "$REPO/scripts/lib/judgement.py" --subject judge-replay-bank-feed \
            --verdict "$1" --reason "$2"; }

BIN=""
for c in "$REPO/target/debug/sovereign-cli" "$(command -v svrn 2>/dev/null)" \
         "$(command -v sovereign 2>/dev/null)"; do
  [ -n "$c" ] && [ -x "$c" ] && { BIN="$c"; break; }
done
if [ -z "$BIN" ]; then
  judge never-ran "no sovereign-cli on this host, so no turn was run and the bank was not fed"
  exit 4
fi

STAMP="$(date -u +%Y%m%d)"
LEDGER="$RESULTS/gate_audit_forensics_${STAMP}_feed.jsonl"
OUT="$(mktemp -t chaos-feed-XXXXXX.jsonl)"
BEFORE=0
[ -f "$LEDGER" ] && BEFORE="$(wc -l < "$LEDGER" | tr -d ' ')"

echo "feed_replay_bank: $LIMIT question(s) from secret_agent.toml against $CORPUS"
echo "                  forensics -> ${LEDGER#$REPO/}"
SOVEREIGN_GATE_AUDIT_FORENSICS="$LEDGER" \
  "$BIN" bench chaos-monkey run \
    --bank "$HERE/secret_agent.toml" \
    --manifest "$HERE/manifest.toml" \
    --corpus "$CORPUS" \
    --limit "$LIMIT" \
    --out "$OUT"
rc=$?
rm -f "$OUT"

if [ ! -f "$LEDGER" ]; then
  # The bench exit code alone cannot tell "the corpus is missing" from "every
  # turn took the short path" — both leave no ledger, and neither is a pass.
  judge could-not-judge "the bench exited $rc and wrote no forensics ledger; nothing was added to the bank"
  exit 4
fi
AFTER="$(wc -l < "$LEDGER" | tr -d ' ')"
ADDED=$(( AFTER - BEFORE ))
EPISODES="$(python3 - "$LEDGER" <<'PY'
import json, sys
n = 0
for line in open(sys.argv[1], encoding="utf-8"):
    line = line.strip()
    if not line:
        continue
    try:
        if json.loads(line).get("kind") == "audit":
            n += 1
    except ValueError:
        pass
print(n)
PY
)"
if [ "$ADDED" -le 0 ] || [ "$EPISODES" -le 0 ]; then
  # A ledger with rows but no `audit` record carries no evidence window, so the
  # replay can resolve no case from it — recorded is not the same as replayable.
  judge could-not-judge "the ledger gained $ADDED row(s) and holds $EPISODES audit episode(s); the replay resolves cases from audit records, so the bank did not grow"
  exit 1
fi
judge passed "$EPISODES audit episode(s) in ${LEDGER##*/} (+$ADDED row(s) this run) — the replay's newest episode is now today's"
exit 0
