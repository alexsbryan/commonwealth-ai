#!/usr/bin/env bash
# ci-spend-audit.sh — where did the GitHub Actions budget actually go?
#
# The companion to `sovereign cache-audit` (which does this for context
# tokens). Same principle: a budget you cannot measure is a budget you will
# blow, and you will blow it in a way that is invisible until something stops
# working. On 2026-07-24 this repo hit a billing hard-stop and every CI job
# began aborting in 4 seconds with "The job was not started because recent
# account payments have failed" — which reads, on a PR page, almost exactly
# like a gate that ran and had nothing to say.
#
# GitHub's own /timing endpoint reports `billable.*.total_ms` as 0 on this
# repo, so this script does NOT trust it. It sums real per-job wall time from
# the jobs API and applies the documented runner multipliers itself.
#
# Usage:
#   scripts/ci-spend-audit.sh                  # last 30 days
#   scripts/ci-spend-audit.sh --since 2026-07-01
#   scripts/ci-spend-audit.sh --since 2026-07-01 --repo owner/name
#
# Requires: gh (authenticated), python3.
set -euo pipefail

SINCE=""
REPO=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --since) SINCE="$2"; shift 2 ;;
        --repo)  REPO="$2";  shift 2 ;;
        -h|--help) sed -n '2,25p' "$0" | sed 's/^# \?//'; exit 0 ;;
        *) echo "unknown flag: $1" >&2; exit 2 ;;
    esac
done

command -v gh >/dev/null || { echo "ci-spend-audit: gh not found" >&2; exit 1; }

if [[ -z "$REPO" ]]; then
    REPO="$(gh repo view --json nameWithOwner -q .nameWithOwner)"
fi
if [[ -z "$SINCE" ]]; then
    SINCE="$(python3 -c 'import datetime;print((datetime.date.today()-datetime.timedelta(days=30)).isoformat())')"
fi

echo "ci-spend-audit: ${REPO}, runs created since ${SINCE}" >&2
echo "ci-spend-audit: fetching run list…" >&2

runs="$(gh api "/repos/${REPO}/actions/runs" --paginate \
    -q ".workflow_runs[] | select(.created_at > \"${SINCE}\") | [.id, .name, .event] | @tsv")"

n_runs="$(printf '%s\n' "$runs" | grep -c . || true)"
echo "ci-spend-audit: ${n_runs} runs; fetching per-job timing (this takes a minute)…" >&2

jobs_tsv="$(mktemp)"
trap 'rm -f "$jobs_tsv"' EXIT

while IFS=$'\t' read -r id name event; do
    [[ -z "${id:-}" ]] && continue
    gh api "/repos/${REPO}/actions/runs/${id}/jobs?per_page=100" \
        --jq ".jobs[] | [\"${name}\",\"${event}\",.name,(.labels[0] // \"?\"),.started_at,.completed_at,.conclusion] | @tsv" \
        2>/dev/null >> "$jobs_tsv" || true
done <<< "$runs"

SINCE="$SINCE" python3 - "$jobs_tsv" <<'PY'
import sys, collections, datetime, math, os

# GitHub's published per-minute multipliers for hosted runners. SELF-HOSTED
# RUNNERS ARE FREE — counting them as billable (the obvious first mistake)
# overstates spend by whatever share of the fleet is your own hardware.
MULT = {
    'ubuntu-latest': 1, 'ubuntu-22.04': 1, 'ubuntu-24.04': 1, 'ubuntu-20.04': 1,
    'macos-latest': 10, 'macos-15': 10, 'macos-14': 10, 'macos-13': 10, 'macos-12': 10,
    'windows-latest': 2, 'windows-2022': 2, 'windows-2019': 2,
    'self-hosted': 0, '?': 0,
}

def ts(s):
    if not s or s == 'null':
        return None
    return datetime.datetime.fromisoformat(s.replace('Z', '+00:00'))

wf = collections.defaultdict(lambda: [0.0, 0])   # billed, jobs
job = collections.defaultdict(lambda: [0.0, 0])
runner = collections.defaultdict(float)
byday = collections.defaultdict(float)
aborted = []
unknown = set()
days = set()

for line in open(sys.argv[1]):
    p = line.rstrip('\n').split('\t')
    if len(p) < 7:
        continue
    name, event, jobname, label, started, completed, _concl = p
    # Dependabot's update runs each get a unique per-PR name; collapse them or
    # the table is 40 rows of noise instead of one line worth acting on.
    if name.startswith(('cargo in', 'github_actions in', 'npm in', 'pip in')):
        name = 'Dependabot Updates'
    a, b = ts(started), ts(completed)
    if not a or not b:
        continue
    secs = (b - a).total_seconds()
    # NEVER-STARTED JOBS ARE NOT BILLED — do not count them.
    #
    # When an account hits its spending limit, GitHub still creates a job
    # record and marks it `failure` a few seconds later ("The job was not
    # started because recent account payments have failed"). Those look like
    # ordinary short jobs in this API. Counting them overstates spend AND —
    # worse — hides the shape of the real curve behind a flat wall of noise.
    # The first version of this script did exactly that and reported 5,302
    # minutes for July 2026 where the true figure was 4,369, with 843 of
    # 1,199 job-runs never having started at all.
    #
    # 12s is comfortably above the observed 2-4s abort and below any real job.
    if secs <= 12 and _concl == 'failure':
        aborted.append((a.date().isoformat(), name, label))
        days.add(a.date())
        continue
    days.add(a.date())
    # GitHub bills per job, rounded UP to the whole minute.
    mins = math.ceil(secs / 60)
    if label not in MULT:
        unknown.add(label)
    billed = mins * MULT.get(label, 1)
    wf[name][0] += billed;  wf[name][1] += 1
    job[(name, jobname)][0] += billed; job[(name, jobname)][1] += 1
    runner[label] += billed
    byday[a.date().isoformat()] += billed

total = sum(v[0] for v in wf.values())
if not total and not wf:
    print("no jobs found in range"); sys.exit(0)

span = max(len(days), 1)
print()
print(f"{'workflow':<30}{'jobs':>7}{'billed min':>12}{'share':>8}")
print('-' * 57)
for name, (b, c) in sorted(wf.items(), key=lambda x: -x[1][0]):
    share = (100 * b / total) if total else 0
    print(f"{name[:29]:<30}{c:>7}{b:>12.0f}{share:>7.1f}%")
print('-' * 57)
print(f"{'TOTAL':<30}{sum(v[1] for v in wf.values()):>7}{total:>12.0f}")
print()
print(f"observed over {span} distinct day(s) → ~{total/span*30:.0f} billed min / 30 days")
print("private-repo allowances: Free 2,000 · Pro 3,000 · Team 3,000 · Enterprise 50,000")

print()
print("By runner class:")
for k, v in sorted(runner.items(), key=lambda x: -x[1]):
    mult = MULT.get(k, 1)
    note = '  (self-hosted — free)' if mult == 0 else f'  ({mult}x)'
    print(f"  {k:<20}{v:>9.0f}{note}")
if unknown:
    print(f"  NOTE: unrecognised runner label(s), assumed 1x: {', '.join(sorted(unknown))}")

print()
print("Top 12 jobs (this is where to aim):")
for (w, j), (b, c) in sorted(job.items(), key=lambda x: -x[1][0])[:12]:
    per = b / c if c else 0
    print(f"  {(w + ' / ' + j)[:56]:<58}{c:>5} runs {b:>8.0f} min  ({per:>5.1f}/run)")

# The daily curve, because the total hides the failure mode. Measured spend on
# this repo was not a drift — two days in July 2026 consumed the entire monthly
# allowance while every other day was near-idle. A budget blown by BURSTS needs
# different fixes (cancel-in-progress, local gates, cheap iteration) than one
# blown by a high floor.
peak = max(byday.values()) if byday else 0
if byday:
    print()
    print("Daily curve (bursts are the thing to look for):")
    running = 0
    for d in sorted(byday):
        running += byday[d]
        bar = '█' * int(byday[d] / peak * 44) if peak else ''
        print(f"  {d}  {byday[d]:>7.0f}  (cum {running:>7.0f})  {bar}")

if aborted:
    print()
    print(f"NOT BILLED — {len(aborted)} job(s) never started (<=12s failures).")
    print("  This is the signature of a spending-limit or payment hard-stop:")
    print("  jobs are created, fail in seconds, and on a PR page look like")
    print("  checks that ran and passed. Confirm with: gh run view <run-id>")
    bydate = collections.Counter(d for d, _, _ in aborted)
    first = min(bydate)
    print(f"  First seen {first}; worst day {max(bydate, key=bydate.get)} "
          f"({max(bydate.values())} jobs).")
PY
