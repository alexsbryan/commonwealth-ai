#!/usr/bin/env bash
# dev-pod-status.sh — a Vast account dev-pod.sh cannot read is never reported
# as one with nothing billing.
#
# THE FAILING INPUT. A logged-out CLI prints nothing on stdout and its error on
# stderr. The readers did `2>/dev/null | json.load` with a fallback to
# `rows = []`, so `status` said "no dev pod running (nothing billing)".
#
# Cases 1, 3 and 4 fail against the script before `vast_rows` and the
# VAST_API_TOKEN alias. Run the old one with DEV_POD=<path>.
#
# A stub vastai on PATH. No network, no Vast account, no GPU.
set -uo pipefail

ROOT="$(git rev-parse --show-toplevel)"
DEV_POD="${DEV_POD:-$ROOT/scripts/dev-pod.sh}"
[[ -r "$DEV_POD" ]] || { echo "cannot read $DEV_POD"; exit 2; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
rc=0
pass() { echo "  ok    $1"; }
fail() { echo "  FAIL  $1"; rc=1; }

# The stub answers `show instances` from $STUB_INSTANCES unless it was given no
# key, in which case it answers as a logged-out CLI does: nothing on stdout,
# the error on stderr, exit 0. Every call is recorded, so a case can assert
# what was NOT called.
mkdir -p "$T/bin"
cat > "$T/bin/vastai" <<'STUB'
#!/usr/bin/env bash
echo "$*" >> "$STUB_CALLS"
if [[ -z "${VAST_API_KEY:-}" ]]; then
  echo '{"error": true, "status_code": 403, "msg": "This action requires login."}' >&2
  exit 0
fi
case "$1 $2" in
  "show instances") printf '%s\n' "$STUB_INSTANCES" ;;
  *) echo "stub: $*" ;;
esac
STUB
chmod +x "$T/bin/vastai"

run() {  # run <verb...>; env comes from the caller
  : > "$T/calls"
  STUB_CALLS="$T/calls" PATH="$T/bin:$PATH" bash "$DEV_POD" "$@" >"$T/out" 2>"$T/err"
  echo $? > "$T/rc"
}

echo "dev-pod status / instance reads:"

# 1. Not logged in: status must refuse, not answer "nothing billing".
env -u VAST_API_KEY -u VAST_API_TOKEN STUB_INSTANCES='[]' bash -c "$(declare -f run); T='$T' DEV_POD='$DEV_POD'; run status"
if [[ "$(cat "$T/rc")" != 0 ]] && grep -q "requires login" "$T/err" && ! grep -q "nothing billing" "$T/out"; then
  pass "an unreadable account is could-not-judge, not nothing billing"
else
  fail "unreadable account: rc=$(cat "$T/rc") out=[$(cat "$T/out")] err=[$(cat "$T/err")]"
fi

# 2. Logged in, no instances: the one case that may say nothing is billing.
env -u VAST_API_TOKEN VAST_API_KEY=k STUB_INSTANCES='[]' bash -c "$(declare -f run); T='$T' DEV_POD='$DEV_POD'; run status"
if [[ "$(cat "$T/rc")" == 0 ]] && grep -q "nothing billing" "$T/out"; then
  pass "a readable empty account still says nothing is billing"
else
  fail "readable empty account: rc=$(cat "$T/rc") out=[$(cat "$T/out")] err=[$(cat "$T/err")]"
fi

# 3. Not logged in: a verb that resolves the pod by label must stop before it
#    acts, rather than reporting "no instance labelled" and hiding why.
env -u VAST_API_KEY -u VAST_API_TOKEN STUB_INSTANCES='[]' bash -c "$(declare -f run); T='$T' DEV_POD='$DEV_POD'; run logs"
if [[ "$(cat "$T/rc")" != 0 ]] && grep -q "requires login" "$T/err" && ! grep -q "^logs" "$T/calls"; then
  pass "an unreadable account stops a resolving verb before it acts"
else
  fail "resolve on unreadable account: rc=$(cat "$T/rc") calls=[$(tr '\n' ';' < "$T/calls")] err=[$(cat "$T/err")]"
fi

# 4. The key exported only as VAST_API_TOKEN authenticates.
row='[{"id": 4242, "label": "sovereign-dev-daemon", "actual_status": "running", "intended_status": "running", "gpu_name": "RTX A6000", "num_gpus": 1}]'
env -u VAST_API_KEY VAST_API_TOKEN=k STUB_INSTANCES="$row" bash -c "$(declare -f run); T='$T' DEV_POD='$DEV_POD'; run status"
if [[ "$(cat "$T/rc")" == 0 ]] && grep -q "4242" "$T/out"; then
  pass "a key exported as VAST_API_TOKEN reaches the CLI"
else
  fail "VAST_API_TOKEN only: rc=$(cat "$T/rc") out=[$(cat "$T/out")] err=[$(cat "$T/err")]"
fi

exit $rc
