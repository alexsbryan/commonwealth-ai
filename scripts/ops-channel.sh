#!/bin/bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# ops-channel.sh — sandboxed SSH command surface for mesh peer operations.
#
# Installed as a FORCED COMMAND in ~/.ssh/authorized_keys:
#
#   restrict,command="<abs path to this script>" ssh-ed25519 AAAA... svrn-ops
#
# `restrict` disables pty/port-forwarding/agent-forwarding/X11; the forced
# command means the client NEVER gets a shell — whatever it sends arrives in
# SSH_ORIGINAL_COMMAND and is matched against the verb allowlist below.
# Unknown verbs are rejected and logged. Every invocation (allowed or not)
# is appended to ~/.svrnmesh/logs/ops-channel.log.
#
# Verbs are fixed commands; the only accepted argument shapes are validated
# by regex. Nothing from the client is ever eval'd or passed to a shell.
#
# Works on macOS (bash 3.2) and Linux. Revoke access by deleting the
# authorized_keys line.

set -u

LOG_DIR="$HOME/.svrnmesh/logs"
LOG="$LOG_DIR/ops-channel.log"
mkdir -p "$LOG_DIR"

RAW="${SSH_ORIGINAL_COMMAND:-ping}"
CLIENT="${SSH_CLIENT%% *}"
NOW="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

log() { printf '%s from=%s %s\n' "$NOW" "${CLIENT:-local}" "$1" >>"$LOG"; }

# Tokenize the client string ourselves; no eval, no shell round-trip.
VERB="$(printf '%s' "$RAW" | awk '{print $1}')"
ARG="$(printf '%s' "$RAW" | awk '{print $2}')"

# Locate the sovereign CLI without trusting the client's environment.
SVRN=""
for c in "$HOME/.local/bin/sovereign" "$HOME/.local/bin/svrn" \
         "$HOME/dev/commonwealth-ai/target/debug/sovereign-cli" \
         "$HOME/dev/commonwealth-ai/target/release/sovereign-cli"; do
  if [ -x "$c" ]; then SVRN="$c"; break; fi
done

REPO="$HOME/dev/commonwealth-ai"

daemon_pid() { pgrep -f 'sovereign-cli-daemon' 2>/dev/null | head -1; }

exe_info() {
  pid="$(daemon_pid)"
  if [ -z "$pid" ]; then echo "daemon: not running"; return 0; fi
  case "$(uname)" in
    Linux)  exe="$(readlink -f "/proc/$pid/exe" 2>/dev/null)" ;;
    Darwin) exe="$(ps -p "$pid" -o comm= 2>/dev/null)" ;;
    *)      exe="unknown" ;;
  esac
  echo "pid=$pid exe=$exe"
  [ -n "$exe" ] && [ -e "$exe" ] && ls -l "$exe"
  ps -p "$pid" -o etime= 2>/dev/null | sed 's/^/uptime=/'
}

deny() {
  log "REJECTED cmd=[$RAW]"
  echo "ops-channel: verb not allowed: [$RAW]" >&2
  echo "allowed: ping status mesh-status transport http-status mesh-http logs [N] cache-size exe-info git-head daemon-start daemon-stop daemon-restart daemon-kill9 [dry]" >&2
  exit 1
}

case "$VERB" in
  ping)
    log "OK ping"
    echo "ok host=$(uname -n) time=$NOW"
    ;;
  status)
    log "OK status"
    [ -n "$SVRN" ] && exec "$SVRN" status || { echo "sovereign CLI not found" >&2; exit 2; }
    ;;
  mesh-status)
    log "OK mesh-status"
    [ -n "$SVRN" ] && exec "$SVRN" mesh status || { echo "sovereign CLI not found" >&2; exit 2; }
    ;;
  transport)
    log "OK transport"
    [ -n "$SVRN" ] && exec "$SVRN" mesh transport || { echo "sovereign CLI not found" >&2; exit 2; }
    ;;
  http-status)
    log "OK http-status"
    exec curl -sS --max-time 5 http://127.0.0.1:9741/status
    ;;
  mesh-http)
    log "OK mesh-http"
    exec curl -sS --max-time 5 http://127.0.0.1:9741/v1/mesh/status
    ;;
  logs)
    N="${ARG:-200}"
    case "$N" in (*[!0-9]*|'') deny ;; esac
    [ "$N" -gt 5000 ] && N=5000
    log "OK logs n=$N"
    newest="$(ls -t "$LOG_DIR"/*.log 2>/dev/null | grep -v 'ops-channel.log' | head -1)"
    if [ -z "$newest" ]; then echo "no daemon logs under $LOG_DIR" >&2; exit 2; fi
    echo "== $newest (last $N lines) =="
    exec tail -n "$N" "$newest"
    ;;
  cache-size)
    log "OK cache-size"
    du -sh "$HOME/.svrnmesh"/*cache* "$HOME/.svrnmesh"/rpc* 2>/dev/null || true
    df -h "$HOME" | tail -1
    ;;
  exe-info)
    log "OK exe-info"
    exe_info
    ;;
  git-head)
    log "OK git-head"
    git -C "$REPO" log -1 --oneline 2>/dev/null
    git -C "$REPO" status --short 2>/dev/null | head -20
    ;;
  daemon-start|daemon-stop|daemon-restart)
    sub="${VERB#daemon-}"
    log "OK $VERB"
    [ -n "$SVRN" ] && exec "$SVRN" daemon "$sub" || { echo "sovereign CLI not found" >&2; exit 2; }
    ;;
  daemon-kill9)
    pid="$(daemon_pid)"
    if [ -z "$pid" ]; then log "OK daemon-kill9 (no daemon)"; echo "daemon: not running"; exit 0; fi
    if [ "$ARG" = "dry" ]; then
      log "OK daemon-kill9 dry pid=$pid"
      echo "would kill -9 pid=$pid"
      exit 0
    fi
    log "OK daemon-kill9 pid=$pid"
    kill -9 "$pid" && echo "killed -9 pid=$pid at $NOW"
    ;;
  *)
    deny
    ;;
esac
