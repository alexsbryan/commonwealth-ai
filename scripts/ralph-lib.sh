#!/bin/bash
# ralph-lib.sh — the session/contract layer shared by ralph-loop.sh (serial)
# and ralph-pool.sh (parallel). Both source this; neither re-implements a
# subset. The pool kept missing mechanisms the loop had (the process-group
# kill, the wait-for marker) — each surfaced as a "failure" that was really a
# missing feature. One implementation ends that.
#
# The sourcing script sets, before calling:
#   STOP_FILE, DONE_FILE, NEEDS_HUMAN, SESSION_TIMEOUT, NOTIFY, OPENCODE_BIN
# and may set MODEL_ARGS (e.g. "--model x"). WORKDIR is the cwd.

say() { echo "$(date -u +%FT%TZ) $*"; }

# Shared queue grammar: both drivers decide readiness from these helpers.
status_of() {
  local line
  line=$(grep -E "^- \[[x~ ]\] $1([[:space:]]|$)" "$STATE" | head -1)
  case "$line" in
    "- [x] "*) printf 'x' ;;
    "- [~] "*) printf '~' ;;
    "- [ ] "*) printf ' ' ;;
    *) printf '' ;;
  esac
}
deps_of() {
  grep -E "^- \[[x~ ]\] $1([[:space:]]|$)" "$STATE" | head -1 \
    | sed -n 's/.*depends \[\([^]]*\)\].*/\1/p' | tr ',' ' '
}
deps_met() { local d; for d in $(deps_of "$1"); do [ "$(status_of "$d")" = x ] || return 1; done; return 0; }

current_unit() {
  [ -f "$STATE" ] || return 0
  local u
  u=$(grep -oE '^- \[~\] [A-Za-z0-9-]+' "$STATE" | head -1 | awk '{print $NF}')
  if [ -n "$u" ]; then printf '%s' "$u"; return; fi
  for u in $(grep -oE '^- \[ \] [A-Za-z0-9-]+' "$STATE" | awk '{print $NF}'); do
    if deps_met "$u"; then printf '%s' "$u"; return; fi
  done
}

notify() { # title body
  [ "${NOTIFY:-0}" -eq 1 ] || return 0
  /usr/bin/osascript -e "display notification \"${2}\" with title \"ralph: ${1}\"" >/dev/null 2>&1 || true
}

halt() { # reason
  say "HALT: $1"
  printf '%s\n\nresolve by hand, then remove %s\n' "$1" "$STOP_FILE" > "$NEEDS_HUMAN"
  : > "$STOP_FILE"
  notify "HALT" "$1"
  exit 3
}

# One session, in its own process group, with a wall-clock timeout, a
# STOP check, a dirty-tree note, and permission-reject detection.
# prompt_file may already carry a caller note (e.g. the pool's lane note).
run_session() { # prompt_file dir log
  local prompt="$1" dir="$2" log="$3" input pid waited before after
  input="${TMPDIR:-/tmp}/ralph-$$-$(basename "$dir").md"
  {
    if [ -n "$(git -C "$dir" status --porcelain 2>/dev/null)" ]; then
      printf 'NOTE: the tree holds uncommitted work from a prior session:\n'
      git -C "$dir" status --short
      printf 'Inspect it and continue from it; do not discard work already done. Commit it as you go.\n\n'
    fi
    cat "$prompt"
  } > "$input"
  before=$(grep -c 'auto-rejecting' "$log" 2>/dev/null || true); before=${before:-0}
  set -m
  # shellcheck disable=SC2086
  ( cd "$dir" && exec "$OPENCODE_BIN" run $MODEL_ARGS "$(cat "$input")" ) > "$log" 2>&1 &
  pid=$!
  set +m
  waited=0
  while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt "$SESSION_TIMEOUT" ]; do
    sleep 30; waited=$((waited + 30))
    [ $((waited % 60)) -eq 0 ] && say "  session $(basename "$dir") running ${waited}s"
    if [ -f "$STOP_FILE" ]; then
      say "STOP requested — killing the session group"
      kill -TERM -- -"$pid" 2>/dev/null; sleep 5; kill -KILL -- -"$pid" 2>/dev/null; break
    fi
  done
  if kill -0 "$pid" 2>/dev/null; then
    say "session exceeded ${SESSION_TIMEOUT}s — killing its group"
    notify "timeout" "killed at ${SESSION_TIMEOUT}s"
    kill -TERM -- -"$pid" 2>/dev/null; sleep 5; kill -KILL -- -"$pid" 2>/dev/null
  fi
  wait "$pid" 2>/dev/null || true
  rm -f "$input"
  after=$(grep -c 'auto-rejecting' "$log" 2>/dev/null || true); after=${after:-0}
  if [ "$after" -gt "$before" ]; then
    say "WARNING: $((after - before)) permission auto-rejections — extend opencode.json"
    notify "permissions" "$((after - before)) auto-rejections"
  fi
}

# The detached-run contract. Returns 0 to proceed, 1 to wait. `ralph/waiting`
# names a *.done marker on its first line; if the marker exists, clear the file
# and proceed; if it does not, the caller waits; a file naming no marker is
# ignored (a mistake, reported, never a hang).
wait_for_marker() {
  [ -f ralph/waiting ] || return 0
  local m
  m=$(grep -oE '[A-Za-z0-9._/-]+\.done' ralph/waiting | head -1)
  if [ -z "$m" ]; then
    say "ralph/waiting names no *.done marker — ignoring it"
    rm -f ralph/waiting
    return 0
  fi
  if [ ! -f "$m" ]; then
    say "waiting on $m (no session this tick)"
    return 1
  fi
  say "$m present — resuming"
  rm -f ralph/waiting
  return 0
}
