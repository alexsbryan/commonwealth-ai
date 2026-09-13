#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# cw-work-offload-share.sh — the instrument for the `cw-work-ci-offload` bar
# (quality/campaigns/closed/cw-lift.toml). It answers ONE question about the newest
# `svrn quality check` run:
#
#   what share of this repository's own CI ran on a node that did not submit it?
#
#   scripts/cw-work-offload-share.sh [--summary <path>] [--json]
#
# ── The formula, and why it is the same one on both paths ───────────────────
#
# A `quality-check/v1` document carries a doc-level `submitted_by` and one
# `node` per lane row (cw-lift 5e, `quality_check_cmd/report.rs::write_summary`).
#
#   numerator    rows whose `node` is NON-NULL and differs from `submitted_by`
#   denominator  EVERY lane row in the document
#
# Both halves matter and the denominator is the one that can lie. A split run
# that silently drops a shard reports a smaller, greener table than a local
# run, and a share computed over "rows that came back" would score that as a
# win — the bar's own block says so. Counting every selected row instead means
# a dropped shard COSTS the share.
#
# The `node`-is-non-null clause is the other half of the same rule: a
# distributed unit the cohort could not place is a row with `node: null`
# against a real `submitted_by`, and a naive `!=` would count that absence as
# an offload. It is in the denominator and never in the numerator.
#
# A LOCAL run writes `null` for both, so it reads exactly 0.0 — the bar's
# floor, MEASURED (the lanes ran; none ran elsewhere) rather than asserted.
#
# ── The four verdicts (ARCH §18.2, §18.3) ───────────────────────────────────
#
# `scripts/co-lineage.py::measure_bar` maps rc + last stdout line onto a row:
#
#   rc 0, stdout {"value": N}   MEASURED share in [0,1]
#   rc 3                        could-not-judge: artifact-absent. No summary
#                               exists, or the newest one is unreadable, or it
#                               is not `quality-check/v1`, or it holds no lane
#                               rows. NOTHING WAS MEASURED.
#   rc 2                        usage
#   rc 127                      instrument-missing (the runner's own reading)
#
# rc 3 and `{"value": 0}` are the two readings this script exists to keep
# apart, and the bar's block names the confusion by hand: "when 5e writes it,
# it must exit 3 on an absent summary — artifact-absent — rather than printing
# this floor back as though a run had measured it". A zero written by a
# missing artifact is a substitution (ARCH §18.3) and is indistinguishable
# from a real all-local run, which is the reading the bar is actually trying
# to move.
#
# NOT `set -e`: an absent artifact is a VERDICT to classify, never an abort.
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUNS_DIR="$REPO/target/quality-check"
SUMMARY=""
AS_JSON=0

say() { printf '%s\n' "$*" >&2; }

# An abstention. Distinct from `value 0` on purpose: it makes no claim.
abstain() {
  say ""
  say "COULD-NOT-JUDGE — $1"
  exit 3
}

usage() {
  say "usage: scripts/cw-work-offload-share.sh [--summary <path>] [--json]"
  say ""
  say "  Reads the newest target/quality-check/*/summary.json (or --summary)"
  say "  and prints the share of lane rows that ran on a node other than the"
  say "  one that submitted them. Exit 3 when there is no summary to read."
  exit 2
}

while [ $# -gt 0 ]; do
  case "$1" in
    --summary) SUMMARY="${2:-}"; [ -n "$SUMMARY" ] || usage; shift 2 ;;
    --json)    AS_JSON=1; shift ;;
    -h|--help) usage ;;
    *)         say "unknown argument: $1"; usage ;;
  esac
done

command -v python3 >/dev/null 2>&1 || abstain "python3 is not on this host, so the summary cannot be read"

# The newest run that is a SUBMISSION, by directory mtime. `ls -t` rather than
# sorting the stamp text: the stamp is local time and a DST fold sorts two runs
# backwards once a year, which is exactly the kind of quiet wrong answer this
# file is about.
#
# "that is a submission" is the 2026-09-11 correction and it is a §18.3 fix, not
# a refinement. This bar asks whether CI ran on a node that did not submit it.
# Every quality-check run writes `quality-check/v1` into the same directory —
# `pre-push.sh` included — so taking the newest document meant an unrelated
# LOCAL gate run became the subject, and the bar read 0.0: "no unit ran
# elsewhere". That is a true sentence about the wrong run, and it demoted a met
# bar (1.0 at 746104439, 4 of 4 rows on a non-submitting node) two hours after
# it was stamped. A document with no `submitted_by` was never a cohort
# submission and cannot answer this question at all, so it is skipped, and if
# none survives we ABSTAIN. Absence of a distributed run is not a measurement
# of zero distribution (§18.2).
if [ -z "$SUMMARY" ]; then
  [ -d "$RUNS_DIR" ] || abstain "no $RUNS_DIR — this repo has no quality-check run to read"
  for d in $(ls -1dt "$RUNS_DIR"/*/ 2>/dev/null); do
    [ -f "$d/summary.json" ] || continue
    if python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); sys.exit(0 if d.get("submitted_by") else 1)' \
         "$d/summary.json" 2>/dev/null; then
      SUMMARY="$d/summary.json"; break
    fi
    say "skipped $d — no \`submitted_by\`, so it is a local run and not a submission"
  done
  [ -n "$SUMMARY" ] || abstain "no run under $RUNS_DIR carries a \`submitted_by\` — this repo has no cohort submission to read, which is the absence of a distributed run, not a measurement of zero"
fi
[ -f "$SUMMARY" ] || abstain "no summary at $SUMMARY — no run has written one"

say "summary: $SUMMARY"

# ONE reader, in python, because the rule has three clauses and a jq-free
# shell version of it would be a second implementation of the formula the
# comment above states (ARCH §10.6).
python3 - "$SUMMARY" "$AS_JSON" <<'PY'
import json, sys

path, as_json = sys.argv[1], sys.argv[2] == "1"

try:
    doc = json.loads(open(path, encoding="utf-8").read())
except Exception as e:                      # noqa: BLE001 — every failure is one verdict
    print(f"COULD-NOT-JUDGE — {path} is not readable JSON: {e}", file=sys.stderr)
    sys.exit(3)

# The SCHEMA is checked, not assumed. A document from a build before cw-lift
# 5e has no `node` field at all, and reading its absence as "ran locally"
# would report a measured 0 about a run that could not have said.
schema = doc.get("schema")
if schema != "quality-check/v1":
    print(f"COULD-NOT-JUDGE — {path} declares schema {schema!r}, not quality-check/v1",
          file=sys.stderr)
    sys.exit(3)

lanes = doc.get("lanes")
if not isinstance(lanes, list) or not lanes:
    print(f"COULD-NOT-JUDGE — {path} holds no lane rows, so there is no share to take",
          file=sys.stderr)
    sys.exit(3)

# A row from a pre-5e writer has no `node` KEY. That is not `node: null` — one
# says "nothing ran there", the other says "this writer could not tell you" —
# and only the second makes the share unmeasurable.
missing = [r.get("id") for r in lanes if "node" not in r]
if missing:
    print(f"COULD-NOT-JUDGE — {len(missing)} row(s) carry no `node` field at all "
          f"({', '.join(str(m) for m in missing[:3])}…): this summary was written by a build "
          f"before the column existed", file=sys.stderr)
    sys.exit(3)

submitted_by = doc.get("submitted_by")
elsewhere = [r for r in lanes
             if r.get("node") is not None and r.get("node") != submitted_by]
unplaced = [r for r in lanes if r.get("node") is None]
share = len(elsewhere) / len(lanes)

def short(k):
    return "—" if k is None else (k[:12] + "…" if len(k) > 12 else k)

print(f"  submitted_by : {short(submitted_by)}", file=sys.stderr)
print(f"  lane rows    : {len(lanes)}", file=sys.stderr)
print(f"  ran elsewhere: {len(elsewhere)}", file=sys.stderr)
print(f"  no node named: {len(unplaced)}  "
      f"(local lanes, or units the cohort never placed — in the denominator, "
      f"never in the numerator)", file=sys.stderr)
print("", file=sys.stderr)
print(f"MEASURED {share:.4f} — {len(elsewhere)} of {len(lanes)} lane row(s) ran on a node "
      f"that did not submit them", file=sys.stderr)

if as_json:
    print(json.dumps({
        "value": share,
        "lanes": len(lanes),
        "elsewhere": len(elsewhere),
        "unplaced": len(unplaced),
        "submitted_by": submitted_by,
        "summary": path,
        "artifact": path,
    }))
else:
    print(json.dumps({"value": share, "artifact": path}))
PY
exit $?
