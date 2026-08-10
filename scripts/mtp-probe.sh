#!/usr/bin/env bash
# MTP throughput probe — drives N free-form chat completions against the
# primary slot, then aggregates `mtp: end-of-generation` lines from the
# daemon log for accept_rate / tok_per_s.
#
# Use after swapping primary to an MTP gguf. Skips the full SEP eval
# (40 GB in-process atlas-graph loader), so it's safe to run on a
# memory-tight host.
#
# Usage:
#   ./scripts/mtp-probe.sh                 # default: 5 prompts, 200 max_tokens each
#   ./scripts/mtp-probe.sh --n 10          # 10 prompts
#   ./scripts/mtp-probe.sh --max-tokens 500
#   SOVEREIGN_MTP_DISABLE=1 ./scripts/mtp-probe.sh   # A/B: non-MTP run
set -euo pipefail

DAEMON_URL="${DAEMON_URL:-http://localhost:9741}"
DAEMON_LOG="${DAEMON_LOG:-$HOME/.svrnmesh/logs/daemon.out}"
N_PROMPTS=5
MAX_TOKENS=200

while [[ $# -gt 0 ]]; do
    case "$1" in
        --n) N_PROMPTS="$2"; shift 2;;
        --max-tokens) MAX_TOKENS="$2"; shift 2;;
        -h|--help) sed -n '2,16p' "$0"; exit 0;;
        *) echo "unknown arg: $1" >&2; exit 2;;
    esac
done

PROMPTS=(
    "List the three branches of the U.S. government in one sentence."
    "Explain the principle of least action in classical mechanics for a physics undergraduate."
    "Summarize the plot of Tolstoy's War and Peace in one paragraph, naming the main characters."
    "Describe how mitochondrial DNA inheritance differs from nuclear inheritance, with one concrete example."
    "What is the relationship between the Riemann hypothesis and the distribution of prime numbers?"
    "Compare and contrast monolithic and microservice architectures, with one production tradeoff each."
    "Walk through the steps of preparing a chocolate ganache from scratch."
    "Explain Bayesian inference using a coin-flip example a high-schooler can follow."
    "Describe the formation of the Himalayas through plate tectonics."
    "What is the cultural significance of the Diné Bahane' creation narrative?"
)

# Capture log position BEFORE we start, so we only aggregate fresh events.
LOG_OFFSET=$(wc -c < "$DAEMON_LOG")

echo "probe: $N_PROMPTS prompts × max_tokens=$MAX_TOKENS against $DAEMON_URL"
echo "probe: aggregating mtp telemetry from $DAEMON_LOG (offset $LOG_OFFSET)"

# Drive the prompts. Use only what fits in PROMPTS[]; cycle if N > 10.
SCRIPT_START_MS=$(date +%s%3N)
for i in $(seq 1 "$N_PROMPTS"); do
    idx=$(( (i - 1) % ${#PROMPTS[@]} ))
    P="${PROMPTS[$idx]}"
    # Build the JSON body via python3 (always present) with the prompt
    # passed through env to dodge shell-quoting hazards (apostrophes,
    # backticks, etc.). jq is not assumed installed.
    body=$(PROMPT="$P" MT="$MAX_TOKENS" python3 -c '
import json, os
print(json.dumps({
    "model": "primary",
    "messages": [{"role": "user", "content": os.environ["PROMPT"]}],
    "max_tokens": int(os.environ["MT"]),
    "temperature": 0,
}))')
    REQ_START_MS=$(date +%s%3N)
    OUT=$(curl -s --max-time 600 -X POST "$DAEMON_URL/v1/chat/completions" \
        -H "Content-Type: application/json" -d "$body")
    REQ_END_MS=$(date +%s%3N)
    # Quick HTTP error check
    if echo "$OUT" | grep -q '"error"'; then
        printf 'request %d/%d: ERROR — %s\n' "$i" "$N_PROMPTS" \
            "$(echo "$OUT" | head -c 200)" >&2
        continue
    fi
    elapsed_ms=$(( REQ_END_MS - REQ_START_MS ))
    printf 'request %d/%d: wall=%dms\n' "$i" "$N_PROMPTS" "$elapsed_ms"
done
SCRIPT_END_MS=$(date +%s%3N)

# Aggregate mtp telemetry from log lines emitted AFTER our start offset.
echo ""
echo "=== mtp telemetry (per-request) ==="
tail -c +"$((LOG_OFFSET + 1))" "$DAEMON_LOG" \
    | grep "mtp: end-of-generation" \
    | sed -E 's/.*accept_rate="?([0-9.]+)"?.*n_generated=([0-9]+).*tok_per_s="?([0-9.]+)"?.*has_schema=(true|false).*/accept_rate=\1 n_gen=\2 tok_per_s=\3 schema=\4/' \
    || true

echo ""
echo "=== aggregate ==="
python3 - "$DAEMON_LOG" "$LOG_OFFSET" <<'PY'
import re, sys
log_path, offset = sys.argv[1], int(sys.argv[2])
with open(log_path, "rb") as f:
    f.seek(offset)
    tail = f.read().decode("utf-8", errors="replace")
# Strip ANSI escape sequences — the daemon logs them around every
# field name and value, e.g. `\x1b[3mn_draft_calls\x1b[0m\x1b[2m=\x1b[0m6`.
tail = re.sub(r'\x1b\[[0-9;]*m', '', tail)
ev = re.compile(
    r'mtp: end-of-generation.*?'
    r'n_draft_calls=(?P<calls>\d+).*?'
    r'drafts_proposed=(?P<proposed>\d+).*?'
    r'drafts_accepted=(?P<accepted>\d+).*?'
    r'accept_rate="?(?P<rate>[\d.]+)"?.*?'
    r'n_generated=(?P<gen>\d+).*?'
    r'elapsed_ms=(?P<ms>\d+).*?'
    r'tok_per_s="?(?P<tps>[\d.]+)"?.*?'
    r'has_schema=(?P<sch>true|false)'
)
rows = [m.groupdict() for m in ev.finditer(tail)]
if not rows:
    print("no MTP events captured — primary slot may not have served any free-form requests")
    sys.exit(0)
n = len(rows)
total_proposed = sum(int(r['proposed']) for r in rows)
total_accepted = sum(int(r['accepted']) for r in rows)
total_gen      = sum(int(r['gen']) for r in rows)
total_ms       = sum(int(r['ms'])  for r in rows)
agg_rate = (total_accepted / total_proposed) if total_proposed else 0.0
agg_tps  = (total_gen * 1000.0 / total_ms) if total_ms else 0.0
print(f"requests:          {n}")
print(f"tokens generated:  {total_gen}")
print(f"wall (decode):     {total_ms} ms ({total_ms/1000:.1f} s)")
print(f"agg tok/s:         {agg_tps:.1f}")
print(f"drafts proposed:   {total_proposed}")
print(f"drafts accepted:   {total_accepted}")
print(f"agg accept_rate:   {agg_rate:.3f}")
n_schema = sum(1 for r in rows if r['sch'] == 'true')
print(f"requests w/ schema: {n_schema} / {n}")
PY
