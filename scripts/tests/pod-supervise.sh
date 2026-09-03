#!/usr/bin/env bash
# pod-supervise.sh — the supervisor restarts, including when the dead process
# left a child holding the log.
#
# THE FAILING INPUT, NAMED. The first supervision loop was
# `while :; do "$@" 2>&1 | tee -a "$LOG"; done`, inline in the pod's onstart
# heredoc. bash waits for every member of a pipeline and `tee` only sees EOF
# when the last writer closes the pipe — so a supervised process that leaves a
# child behind (the daemon spawns several) wedges the supervisor forever.
# Flown 2026-09-03 on pod 49783403: supervisor alive, no daemon, no restart
# banner, port dead, and a bench run that reported "daemon unreachable" for
# the rest of its length.
#
# The orphan test below is that exact shape and it FAILS against the piped
# loop — which is the only reason to trust it.
#
# No daemon, no model, no GPU, no network. Runs in a couple of seconds.
set -uo pipefail

ROOT="$(git rev-parse --show-toplevel)"
SUP="$ROOT/scripts/pod-supervise.sh"
[[ -x "$SUP" ]] || { echo "cannot find/execute $SUP"; exit 2; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
rc=0
pass() { echo "  ok    $1"; }
fail() { echo "  FAIL  $1"; rc=1; }

# ── fixture: a "daemon" that dies immediately, leaving a child that holds
#    its stdout open for well past the test's patience.
cat > "$T/orphan-daemon.sh" <<'EOF'
#!/usr/bin/env bash
# Spawn a child that inherits stdout and outlives us, then exit non-zero the
# way an abort() would.
sleep 30 &
echo "daemon-up"
exit 134   # 128 + SIGABRT, what a failed GGML_ASSERT looks like
EOF
chmod +x "$T/orphan-daemon.sh"

cat > "$T/clean-daemon.sh" <<'EOF'
#!/usr/bin/env bash
echo "daemon-up"
exit 1
EOF
chmod +x "$T/clean-daemon.sh"

# ── 1. THE REGRESSION TEST: restart despite an orphan holding the log ───────
out="$T/orphan.out"
SUPERVISE_MAX_STARTS=3 SUPERVISE_YOUNG_SECS=0 SUPERVISE_BACKOFF=0 \
  timeout 25 "$SUP" "$T/orphan.log" "$T/orphan-daemon.sh" > "$out" 2>&1
starts=$(grep -c "^\[supervise\] start #" "$out" 2>/dev/null || echo 0)
if [[ "$starts" -eq 3 ]]; then
  pass "restarts when the dead process left a child holding the log (got $starts starts)"
else
  fail "orphan-holding-the-log wedged the supervisor — wanted 3 starts, got $starts"
  sed 's/^/        /' "$out" | head -12
fi

# ── 2. control: a process that leaves nothing behind also restarts ──────────
out2="$T/clean.out"
SUPERVISE_MAX_STARTS=3 SUPERVISE_YOUNG_SECS=0 SUPERVISE_BACKOFF=0 \
  timeout 25 "$SUP" "$T/clean.log" "$T/clean-daemon.sh" > "$out2" 2>&1
starts2=$(grep -c "^\[supervise\] start #" "$out2" 2>/dev/null || echo 0)
[[ "$starts2" -eq 3 ]] && pass "restarts a cleanly-exiting process too (control)" \
  || fail "control failed — wanted 3 starts, got $starts2"

# ── 3. the exit status is the SUPERVISED process's, not the mirror's ────────
# A supervisor that reported tee's status would call every crash a success —
# which is how the pipeline version hid the daemon's abort code.
SUPERVISE_MAX_STARTS=1 SUPERVISE_YOUNG_SECS=0 SUPERVISE_BACKOFF=0 \
  timeout 25 "$SUP" "$T/status.log" "$T/orphan-daemon.sh" > "$T/status.out" 2>&1
got=$?
[[ "$got" -eq 134 ]] && pass "propagates the supervised process's exit status (134)" \
  || fail "wanted exit 134 from the supervised process, got $got"

# ── 4. the death is REPORTED, with the status, not swallowed ───────────────
if grep -qE "^\[supervise\] EXITED — start #1, status 134" "$T/status.out"; then
  pass "names the exit status in the banner (a silent respawn is the other bug)"
else
  fail "no EXITED banner carrying status 134"
  sed 's/^/        /' "$T/status.out" | head -8
fi

# ── 5. a young death backs off, so a broken loadout cannot spin a rented GPU
start_ts=$(date +%s)
SUPERVISE_MAX_STARTS=2 SUPERVISE_YOUNG_SECS=999 SUPERVISE_YOUNG_BACKOFF=2 \
  timeout 25 "$SUP" "$T/backoff.log" "$T/clean-daemon.sh" > "$T/backoff.out" 2>&1
took=$(( $(date +%s) - start_ts ))
if grep -q "backing off" "$T/backoff.out" && [[ "$took" -ge 2 ]]; then
  pass "a young death backs off before restarting (${took}s)"
else
  fail "expected a backoff of >=2s and a banner saying so; took ${took}s"
fi

# ── 6. refuses with no command rather than looping on nothing ──────────────
"$SUP" "$T/none.log" >/dev/null 2>&1
[[ $? -eq 2 ]] && pass "refuses when given no command" || fail "expected exit 2 with no command"

exit "$rc"
