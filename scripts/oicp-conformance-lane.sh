#!/usr/bin/env bash
# oicp-conformance-lane.sh — certify this host against the OICP v0.4 contract
# and fail on any regression from the committed baseline.
#
# WHY THIS WRAPPER EXISTS. `commonwealth/crates/oicp-conformance` has been a
# complete, working certifier since it was written — manifest invariants, the
# three constraint modes, embed bit-compat, knowledge search, the ingest state
# machine, auth posture — with a baseline ratchet already built into it
# (`src/main.rs:65-87`, `src/report.rs:119`). It ran in NO workflow, NO script
# and NO timer. `commonwealth/docs/ARCHITECTURE_REVIEW_2026-08-05.md:421` lists
# wiring it up as the single highest-leverage item in the repo. This is that
# wire, and it is deliberately thin: every judgement below belongs to the
# binary, not to this file.
#
# THE PRECONDITION IS THE WHOLE DESIGN. `regressions()` counts `Pass -> Skip`
# as a regression, on purpose — feature-gating a check off must not be a silent
# way to stop proving it. That property also makes the lane worthless if it is
# pointed at a daemon that cannot answer: 7 of the 10 passing checks drive real
# inference or embeddings, so a stopped or model-less daemon reports seven
# "regressions" that are nothing of the kind. A gate that cries wolf whenever
# the box is idle stops being read. So an unmet precondition is reported as
# COULD-NOT-JUDGE and exits 2 — never as a failure (ARCH_PRINCIPLES §18.3:
# absence is reported, never defaulted; §18.2: four verdicts, not two).
#
# NOT SAFE TO RUN CONCURRENTLY WITH ITSELF. The lane drives real chat,
# embedding and grammar-constrained decodes against the resident daemon's
# slots. Two overlapping runs starve each other and one will flake a `must`
# check (measured 2026-08-31: three back-to-back runs, the third flaked).
#
#   scripts/oicp-conformance-lane.sh                    # run it now
#   scripts/oicp-conformance-lane.sh --update-baseline  # re-mint the baseline
#   scripts/run-if-stale.sh oicp-conformance            # the scheduled form
#
# ── exit codes ───────────────────────────────────────────────────────────
#   0  conformant, no regression vs the baseline
#   1  a `must` check failed, or a check regressed vs the baseline
#   2  could not judge — daemon unreachable, model-less, or the build broke
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/.." && pwd)"
# shellcheck source=scripts/lib/svrn-root.sh
. "${REPO_ROOT}/scripts/lib/svrn-root.sh"

HOST="${OICP_CONFORMANCE_HOST:-http://127.0.0.1:9741}"
BASELINE="$REPO_ROOT/quality/baselines/oicp"
REPORT_DIR="$(svrn_root)/oicp-conformance"
STAMP="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
HEAD_SHA="$(cd "$REPO_ROOT" && git rev-parse --short HEAD 2>/dev/null || echo unknown)"
DIRTY=false
[ -n "$(cd "$REPO_ROOT" && git status --porcelain 2>/dev/null)" ] && DIRTY=true
UPDATE=""
[ "${1:-}" = "--update-baseline" ] && UPDATE="--update-baseline"

mkdir -p "$REPORT_DIR"
REPORT="$REPORT_DIR/report-$(date -u +%Y%m%dT%H%M%SZ).json"

# One writer for the machine-readable verdict, so no path can forget a field.
# `svrn posture` reads this; a lane that ends without writing it reads as
# never-run, which is the honest answer for a lane that did not finish.
emit() { # verdict exit summary
  printf '{"stamp":"%s","commit":"%s","dirty":%s,"verdict":"%s","exit":%s,"summary":"%s"}\n' \
    "$STAMP" "$HEAD_SHA" "$DIRTY" "$1" "$2" "$3" > "$REPORT_DIR/latest.json"
}

BIN="$REPO_ROOT/target/debug/oicp-conformance"
if ! ( cd "$REPO_ROOT" && cargo build -p oicp-conformance 2>&1 | tail -20 ); then
  echo "VERDICT: COULD-NOT-JUDGE — oicp-conformance did not build. Nothing is proven."
  emit could-not-judge 2 "build failed"
  exit 2
fi

# ── precondition: a daemon that can actually answer ──────────────────────
# Checked HERE rather than inside the certifier because "this host does not
# serve OICP" and "this host serves it wrongly" are different findings and
# only the second one is a failure.
manifest="$(curl -fsS --max-time 10 "$HOST/oicp/v1/capabilities" 2>/dev/null)"
if [ -z "$manifest" ]; then
  echo "VERDICT: COULD-NOT-JUDGE — no OICP manifest at $HOST/oicp/v1/capabilities."
  echo "  start one:  sovereign daemon start"
  emit could-not-judge 2 "no manifest at $HOST"
  exit 2
fi
model_count="$(printf '%s' "$manifest" | grep -o '"id"' | wc -l | tr -d ' ')"
if [ "${model_count:-0}" -eq 0 ]; then
  echo "VERDICT: COULD-NOT-JUDGE — $HOST advertises no models."
  echo "  7 of the 10 checks drive real inference; against a model-less host they"
  echo "  would report Pass -> Skip regressions that mean nothing."
  emit could-not-judge 2 "host advertises no models"
  exit 2
fi

# ── the lane ─────────────────────────────────────────────────────────────
"$BIN" --host "$HOST" --baseline "$BASELINE" --report "$REPORT" $UPDATE
rc=$?
ln -sf "$REPORT" "$REPORT_DIR/latest-report.json"

summary="$(sed -n 's/.*"status":"\(fail\)".*/\1/p' "$REPORT" 2>/dev/null | wc -l | tr -d ' ') failing check(s)"
case "$rc" in
  0) echo; echo "VERDICT: PASS — conformant, no regression vs $BASELINE/latest.json"
     emit pass 0 "conformant" ;;
  2) echo; echo "VERDICT: COULD-NOT-JUDGE — the baseline could not be read; the ratchet did not run."
     emit could-not-judge 2 "baseline unreadable" ;;
  *) echo; echo "VERDICT: FAIL — see $REPORT"
     emit fail 1 "$summary" ;;
esac
exit "$rc"
