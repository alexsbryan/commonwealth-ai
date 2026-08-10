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
# Judge engine must be up, or every verdict tonight would be a
# could-not-judge(engine unavailable) — skip WITHOUT moving the mark so
# tomorrow's sweep covers tonight's commits (§18.3: reported, deferred,
# never silently spent).
if ! curl -sf --max-time 10 "$DAEMON/v1/models" >/dev/null 2>&1; then
  note "sweep skipped: daemon unreachable at $DAEMON (mark not moved)"
  exit 0
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
  echo "$sha" > "$STATE"
  n=$((n + 1))
done
note "sweep done: $n commit(s) reviewed, mark at $(cut -c1-7 "$STATE")"
exit 0
