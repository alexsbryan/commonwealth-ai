#!/usr/bin/env bash
# co-sweep.sh — the comaintainer's nightly shadow sweep.
#
# Reviews every commit that landed since the last sweep by calling
# scripts/co-review.sh per commit (verdicts append to
# ~/.sovereign/comaintainer/verdicts.jsonl, same as the interactive
# seat). SYSTEM-OWNED scheduling: launchd on this host, deliberately
# not any agent harness's scheduler — operator edit recorded in the M0
# directive log 2026-08-06 (the workflow owner must be our system, and
# it must keep running whichever agent family is in use).
#
#   scripts/co-sweep.sh              # sweep now (since high-water mark)
#   scripts/co-sweep.sh --install    # write + load launchd agent (03:30)
#   scripts/co-sweep.sh --uninstall  # remove the launchd agent
#
# `launchctl submit` is BANNED in this repo (2026-08-13): it carries implicit
# keepalive and leaves no plist to find. --install writes an explicit plist
# and --uninstall names the bootout that removes it.
#
# Exit 0 always on the sweep path (advisory shadow work, like the seat).
set -uo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUT_DIR="$HOME/.sovereign/comaintainer"
STATE="$OUT_DIR/sweep.last"          # high-water mark: the ONLY state
DAEMON="${SOVEREIGN_DAEMON_URL:-http://localhost:9741}"
LABEL="com.svrn.co-sweep"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
CAP=20                               # commits per night; overflow is NAMED

mkdir -p "$OUT_DIR"
# One write path: everything prints to stdout/stderr; the launchd agent
# captures both into sweep.log, a by-hand run prints to the terminal.
# (v0 had note() tee into sweep.log while launchd wrote a SECOND log —
# a seam with no function; collapsed 2026-08-06 on operator steer.)
note() { printf '%s %s\n' "$(date -u +%FT%TZ)" "$*"; }

case "${1:-}" in
  --install)
    mkdir -p "$HOME/Library/LaunchAgents"
    cat > "$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key>
  <array><string>/bin/bash</string><string>$REPO/scripts/co-sweep.sh</string></array>
  <key>StartCalendarInterval</key>
  <dict><key>Hour</key><integer>3</integer><key>Minute</key><integer>30</integer></dict>
  <key>StandardOutPath</key><string>$OUT_DIR/sweep.log</string>
  <key>StandardErrorPath</key><string>$OUT_DIR/sweep.log</string>
</dict></plist>
EOF
    launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
    launchctl bootstrap "gui/$(id -u)" "$PLIST"
    echo "co-sweep: launchd agent loaded ($LABEL, nightly 03:30) -> $PLIST"
    exit 0 ;;
  --uninstall)
    launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
    rm -f "$PLIST"
    echo "co-sweep: launchd agent removed"
    exit 0 ;;
  -h|--help) sed -n '2,17p' "$0"; exit 2 ;;
esac

# ---- sweep ------------------------------------------------------------
# Bar telemetry FIRST, before the daemon check: the runner is stdlib and
# daemon-free, so measurement survives daemon-down nights (order
# campaign-telemetry). Advisory — a failure is noted, never fatal.
python3 "$REPO/scripts/co-lineage.py" measure --all-active || note "measure: exit $? (advisory)"

# Judge engine must be up, or every verdict tonight would be a
# could-not-judge(engine unavailable) — skip WITHOUT moving the mark so
# tomorrow's sweep covers tonight's commits (§18.3: reported, deferred,
# never silently spent).
#
# PROBE THE SEAM THAT CAN ACTUALLY FAIL (§18.1). Until 2026-08-17 this
# curled /v1/models, which serves the configured model REGISTRY and
# returns 200 whether or not anything is loadable. Measured that day:
# /v1/models 200 while /v1/chat/completions 503 with loaded_models: []
# and primary placed at 0 blocks. A guard on a field the subject merely
# echoes back is not a guard — so spend one 4-token completion instead.
# 180s, not 30s: a cold 22GB primary load is legitimate and the sweep has
# all night. A tight timeout here would defer every night the daemon
# happened to be cold, and the backlog would grow behind a green log.
if ! curl -sf --max-time 180 -X POST "$DAEMON/v1/chat/completions" \
     -H 'content-type: application/json' \
     -d '{"messages":[{"role":"user","content":"ok"}],"max_tokens":4}' \
     >/dev/null 2>&1; then
  note "sweep skipped: judge engine cannot serve a completion at $DAEMON (mark not moved)"
  exit 0
fi

# Interpreter for co-arch.py: it reads the rule set with `tomllib` (3.11+),
# and under launchd PATH is minimal so bare `python3` is the system 3.9.
# Same picker co-review.sh uses for the closure step — one idiom, not two.
# If nothing suitable exists, SAY the audit is skipped (ARCH §18.3); do not
# let a refusing run read as a clean night.
ARCH_PY=""
for cand in "${SOVEREIGN_PYTHON:-}" python3.13 python3.12 python3.11 python3 \
            /Library/Frameworks/Python.framework/Versions/3.13/bin/python3 \
            /opt/homebrew/bin/python3; do
  [ -n "$cand" ] || continue
  command -v "$cand" >/dev/null 2>&1 || continue
  if "$cand" -c 'import tomllib' >/dev/null 2>&1; then ARCH_PY="$cand"; break; fi
done
if [ "${CO_ARCH:-1}" = "1" ] && [ -z "$ARCH_PY" ]; then
  note "co-arch DISABLED this sweep: no python3 with tomllib on PATH" \
       "(set SOVEREIGN_PYTHON). Named, not silent — the audit did not run."
fi

HEAD_SHA="$(git -C "$REPO" rev-parse HEAD)"
LAST="$(cat "$STATE" 2>/dev/null || true)"
if [ -z "$LAST" ] || ! git -C "$REPO" cat-file -e "$LAST" 2>/dev/null; then
  # First run (or rewritten history): baseline to HEAD, review nothing —
  # sweeping months of history would spend a night blessing the past.
  echo "$HEAD_SHA" > "$STATE"
  note "sweep baselined at $HEAD_SHA (first run; no commits reviewed)"
  exit 0
fi

COMMITS="$(git -C "$REPO" rev-list --first-parent --reverse "$LAST..$HEAD_SHA")"
[ -z "$COMMITS" ] && { note "sweep: no new commits since ${LAST:0:7}"; exit 0; }

n=0
for sha in $COMMITS; do
  if [ "$n" -ge "$CAP" ]; then
    remaining=$(printf '%s\n' "$COMMITS" | tail -n +$((CAP + 1)) | wc -l | tr -d ' ')
    note "sweep CAP hit: $CAP reviewed, $remaining deferred to next sweep (mark stays at last reviewed)"
    break
  fi
  "$REPO/scripts/co-review.sh" "$sha"
  # The engine can die MID-sweep, and co-review.sh exits 0 by contract
  # (advisory seat). Advancing the mark past a commit whose only verdict
  # is could-not-judge(engine unavailable) spends it forever — the commit
  # is never swept again. So read the row that was just written and stop
  # without advancing. Deferred, never silently spent (§18.3); the
  # 2026-08-17 repair, evidenced by 3 rows already carrying
  # model:"daemon-unavailable".
  if python3 - "$OUT_DIR/verdicts.jsonl" "$sha" <<'PY'
import json, sys
log, sha = sys.argv[1], sys.argv[2]
try:
    rows = [json.loads(l) for l in open(log) if l.strip()]
except OSError:
    sys.exit(1)
mine = [r for r in rows if r.get("ref") == sha and r.get("kind") in (None, "review")]
sys.exit(0 if mine and str(mine[-1].get("model", "")).endswith("-unavailable") else 1)
PY
  then
    note "sweep HALTED at ${sha:0:7}: engine went unavailable mid-sweep" \
         "($n reviewed, mark stays at $(cut -c1-7 "$STATE" 2>/dev/null || echo none))"
    exit 0
  fi
  python3 "$REPO/scripts/co-drift.py" "$sha" || note "co-drift: exit $? on ${sha:0:7} (advisory)"
  # Architecture audit — DEFAULT ON (operator, 2026-08-17). Advisory and
  # shadow: it writes rows, gates nothing, and a failure here never fails
  # the sweep. Costs nothing on 56% of commits by construction (the gate
  # is model-free and most commits fire no rule); ~19s on the ones that
  # fire, so ~2.8 min/night at this CAP. CO_ARCH=0 turns it off.
  # Bars + the cost-anchor amendment: gym/comaintainer/PREREG_arch_probes_20260817.md.
  if [ "${CO_ARCH:-1}" = "1" ] && [ -n "$ARCH_PY" ]; then
    "$ARCH_PY" "$REPO/scripts/co-arch.py" "$sha" \
      || note "co-arch: exit $? on ${sha:0:7} (advisory)"
  fi
  echo "$sha" > "$STATE"
  n=$((n + 1))
done
note "sweep done: $n commit(s) reviewed, mark at $(cut -c1-7 "$STATE")"
exit 0
