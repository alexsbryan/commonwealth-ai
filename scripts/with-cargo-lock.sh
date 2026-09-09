#!/bin/sh
# with-cargo-lock.sh — serialize cargo-family commands across concurrent
# agents on one host.
#
# WHY THIS EXISTS (2026-09-09): parallel worker sessions on this repo each
# run `cargo` / sovereign-lint.sh / sovereign-test.sh, and cargo's own
# package lock makes them BLOCK on each other (idle minutes); worse, two
# concurrent nextest runs overwrite the shared JUnit report, which the test
# script can only DETECT afterwards (its exit-5 unattributable-results
# guard) rather than prevent. One lock around every cargo-adjacent
# invocation turns both failure modes into a queue.
#
# USAGE:
#   scripts/with-cargo-lock.sh cargo check -p sovereign-mesh --features treesitter
#   scripts/with-cargo-lock.sh ./scripts/sovereign-test.sh --human --package X
#   scripts/with-cargo-lock.sh ./scripts/sovereign-lint.sh --human
#
# RULE (AGENTS.md "Compilation and test feedback"): any command that will
# touch the Cargo package lock — build, check, test, clippy, the two gate
# scripts — goes through this wrapper when more than one agent might be
# alive on the host. Solo sessions may run bare; the wrapper is then a
# no-cost pass-through (one mkdir + one rmdir).
#
# The lock is a DIRECTORY (mkdir is atomic everywhere, including macOS
# which ships no flock(1)) holding the holder's PID and command line.
# A holder that died without releasing is reclaimed automatically: a PID
# that no longer exists, or a lock older than the reclaim age, is removed.
# Both reclaim paths print what they reclaimed so the log explains itself.

set -u

LOCK_DIR="${SVRN_CARGO_LOCK_DIR:-/tmp/svrn-cargo-lock.$(id -u)}"
# A single warm full-workspace test run is ~45s and a cold one ~4m; a lock
# held longer than this is from a dead holder, not a slow build.
RECLAIM_AFTER_SECS="${SVRN_CARGO_LOCK_RECLAIM_SECS:-1800}"
POLL_SECS=5

lock_age() {
    now=$(date +%s)
    born=$(stat -f %m "$LOCK_DIR" 2>/dev/null || stat -c %Y "$LOCK_DIR" 2>/dev/null || echo "$now")
    echo $((now - born))
}

holder_pid_alive() {
    pid=$(cat "$LOCK_DIR/pid" 2>/dev/null) || return 1
    case "$pid" in
        ''|*[!0-9]*) return 1 ;;
    esac
    kill -0 "$pid" 2>/dev/null
}

print_holder() {
    cmd=$(tr '\n' ' ' < "$LOCK_DIR/cmd" 2>/dev/null || echo '?')
    pid=$(cat "$LOCK_DIR/pid" 2>/dev/null || echo '?')
    echo "pid $pid: $cmd"
}

reclaim() {
    reason="$1"
    echo "with-cargo-lock: reclaiming stale lock ($reason; was $(print_holder))" >&2
    rm -rf "$LOCK_DIR"
}

acquire() {
    while ! mkdir "$LOCK_DIR" 2>/dev/null; do
        if ! holder_pid_alive; then
            reclaim "holder pid is gone"
        elif [ "$(lock_age)" -gt "$RECLAIM_AFTER_SECS" ]; then
            reclaim "older than ${RECLAIM_AFTER_SECS}s"
        else
            echo "with-cargo-lock: waiting for $(print_holder)" >&2
            sleep "$POLL_SECS"
        fi
    done
    printf '%s\n' "$$" > "$LOCK_DIR/pid"
    printf '%s\n' "$*" > "$LOCK_DIR/cmd"
}

release() {
    # Only remove what we created: another agent may have reclaimed and
    # re-acquired while we ran. The PID check makes the release honest.
    if [ "$(cat "$LOCK_DIR/pid" 2>/dev/null)" = "$$" ]; then
        rm -rf "$LOCK_DIR"
    fi
}

acquire "$@"
trap release EXIT INT TERM
"$@"
