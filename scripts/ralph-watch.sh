#!/bin/bash
# ralph-watch.sh — a watchdog for a detached ralph job. It notifies when a
# decision package has been sitting unresolved, or when the job is stopped
# without DONE/STOP, and repeats at most once per NAG_SECS while the condition
# holds. The supervisor cannot report its own death; this can.
#
#   scripts/ralph-watch.sh --workdir . --label domains [--install-launchd]
#   RALPH_WATCH_DRY=1 scripts/ralph-watch.sh ...   # print instead of notify
set -u
WORKDIR=""
LABEL=""
INSTALL=0
NAG_SECS="${RALPH_WATCH_NAG_SECS:-1800}"
NOTIFY_CMD="${RALPH_WATCH_OSASCRIPT:-/usr/bin/osascript}"
LAUNCHCTL="${RALPH_WATCH_LAUNCHCTL:-launchctl}"
DF="${RALPH_WATCH_DF:-df}"

while [ $# -gt 0 ]; do
  case "$1" in
    --workdir) WORKDIR="$2"; shift 2 ;;
    --label) LABEL="$2"; shift 2 ;;
    --install-launchd) INSTALL=1; shift ;;
    -h|--help) sed -n '2,9p' "$0"; exit 0 ;;
    *) echo "ralph-watch: unknown flag $1" >&2; exit 2 ;;
  esac
done
[ -n "$WORKDIR" ] || { echo "ralph-watch: --workdir is required" >&2; exit 2; }
[ -n "$LABEL" ] || { echo "ralph-watch: --label is required" >&2; exit 2; }
cd "$WORKDIR" || exit 2
STATE_DIR="${HOME}/.svrnmesh/ralph/$(basename "$PWD")-${LABEL}"
mkdir -p "$STATE_DIR"
STAMP="$STATE_DIR/watch.state"
JOB="dev.ralph.$(basename "$PWD")-$LABEL"
SELF="$(cd "$(dirname "$0")" && pwd)/$(basename "$0")"

notify() { # title body
  if [ "${RALPH_WATCH_DRY:-0}" = "1" ]; then
    echo "notify: $1: $2"
    return 0
  fi
  "$NOTIFY_CMD" -e "display notification \"$2\" with title \"ralph watch: $1\"" >/dev/null 2>&1 || true
}

if [ "$INSTALL" -eq 1 ]; then
  PLIST="${HOME}/Library/LaunchAgents/dev.ralphwatch.$(basename "$PWD")-${LABEL}.plist"
  {
    echo '<?xml version="1.0" encoding="UTF-8"?>'
    echo '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">'
    echo '<plist version="1.0"><dict>'
    echo "  <key>Label</key><string>dev.ralphwatch.$(basename "$PWD")-${LABEL}</string>"
    echo '  <key>ProgramArguments</key><array>'
    echo '    <string>/bin/bash</string>'
    echo "    <string>${SELF}</string>"
    echo '    <string>--workdir</string>'
    echo "    <string>${PWD}</string>"
    echo '    <string>--label</string>'
    echo "    <string>${LABEL}</string>"
    echo '  </array>'
    echo "  <key>WorkingDirectory</key><string>${PWD}</string>"
    echo '  <key>StartInterval</key><integer>120</integer>'
    echo '  <key>RunAtLoad</key><true/>'
    echo '  <key>EnvironmentVariables</key><dict>'
    echo "    <key>HOME</key><string>${HOME}</string>"
    echo "    <key>PATH</key><string>${PATH}</string>"
    echo '  </dict>'
    echo "  <key>StandardOutPath</key><string>${STATE_DIR}/watch.log</string>"
    echo "  <key>StandardErrorPath</key><string>${STATE_DIR}/watch.log</string>"
    echo '</dict></plist>'
  } > "$PLIST"
  plutil -lint "$PLIST" >/dev/null || { echo "ralph-watch: invalid plist" >&2; exit 1; }
  echo "wrote $PLIST"
  echo "load: launchctl bootstrap gui/\$(id -u) $PLIST"
  exit 0
fi

condition=""
body=""
if [ -f ralph/NEEDS_HUMAN.md ]; then
  condition="needs-human:$(shasum -a 256 ralph/NEEDS_HUMAN.md | cut -c1-16)"
  body="$(head -1 ralph/NEEDS_HUMAN.md) — $(basename "$PWD")-$LABEL"
elif [ -f ralph/DONE ] || [ -f ralph/STOP ]; then
  condition=""
else
  state=$("$LAUNCHCTL" print "gui/$(id -u)/$JOB" 2>/dev/null | grep -c "state = running" || true)
  if [ "${state:-0}" -eq 0 ]; then
    condition="down"
    body="$(basename "$PWD")-$LABEL is not running and has no DONE/STOP"
  else
    # A full volume kills the loop mid-write and the halt cannot leave a
    # package (2026-09-15); warn while the loop still has room to work.
    avail_mb=$("$DF" -m /System/Volumes/Data 2>/dev/null | awk 'NR==2 {print $4}')
    if [ -n "$avail_mb" ] && [ "$avail_mb" -lt "${RALPH_WATCH_MIN_FREE_MB:-5120}" ]; then
      condition="disk-low"
      body="$(basename "$PWD")-$LABEL: ${avail_mb}MB free on the data volume"
    fi
  fi
fi

last_cond=""; last_ts=0
if [ -f "$STAMP" ]; then
  read -r last_cond last_ts < "$STAMP" || true
fi
now=$(date +%s)
if [ -n "$condition" ]; then
  if [ "$condition" != "$last_cond" ] || [ $((now - last_ts)) -ge "$NAG_SECS" ]; then
    case "$condition" in
      needs-human:*) notify "needs human" "$body" ;;
      down) notify "loop down" "$body" ;;
      disk-low) notify "disk low" "$body" ;;
    esac
    printf '%s %s\n' "$condition" "$now" > "$STAMP"
  fi
else
  : > "$STAMP"
fi
