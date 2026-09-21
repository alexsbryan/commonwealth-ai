#!/usr/bin/env bash
# dev-pod-tunnel-auth.sh — a refused key ends `tunnel`, it does not spin.
#
# THE FAILING INPUT. 2026-09-19, instance 51635391: both local keys were
# registered on the Vast account and the instance refused them. `tunnel`'s
# reconnect loop retried "Permission denied (publickey)" every 5 s while the
# pod billed, and nothing named the cause. Against the script before
# `attach_key` + the auth preflight, case 1 never returns (the timeout kills
# it, rc 124) and case 2 records no `attach ssh` call.
#
# Stub vastai and stub ssh on PATH. No network, no Vast account, no GPU.
set -uo pipefail

ROOT="$(git rev-parse --show-toplevel)"
DEV_POD="${DEV_POD:-$ROOT/scripts/dev-pod.sh}"
[[ -r "$DEV_POD" ]] || { echo "cannot read $DEV_POD"; exit 2; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
rc=0
pass() { echo "  ok    $1"; }
fail() { echo "  FAIL  $1"; rc=1; }

mkdir -p "$T/bin" "$T/home/.ssh"
echo "ssh-ed25519 AAAATESTKEY test@host" > "$T/home/.ssh/id_ed25519.pub"
INSTANCES='[{"id": 4242, "label": "sovereign-dev-daemon-solo", "actual_status": "running", "cur_state": "running", "dph_total": 0.7, "start_date": 0, "gpu_name": "RTX", "num_gpus": 1}]'

cat > "$T/bin/vastai" <<'STUB'
#!/usr/bin/env bash
echo "$*" >> "$STUB_CALLS"
case "$1 $2" in
  "show instances") printf '%s\n' "$STUB_INSTANCES" ;;
  "ssh-url "*)      echo "ssh://root@203.0.113.9:2222" ;;
  "attach ssh")     echo "{'success': True}"; : > "$STUB_ATTACHED" ;;
  *) echo "stub: $*" ;;
esac
STUB
# ssh: refuses until the key is "attached" when STUB_SSH=heals, always when =refuses.
cat > "$T/bin/ssh" <<'STUB'
#!/usr/bin/env bash
echo "ssh $*" >> "$STUB_CALLS"
if [[ "$STUB_SSH" == heals && -e "$STUB_ATTACHED" ]]; then exit 0; fi
echo "root@203.0.113.9: Permission denied (publickey)." >&2
exit 255
STUB
cat > "$T/bin/sleep" <<'STUB'
#!/usr/bin/env bash
exit 0
STUB
chmod +x "$T/bin/vastai" "$T/bin/ssh" "$T/bin/sleep"

run() {  # run <STUB_SSH mode>; tunnel under a 20 s ceiling so a spin is rc 124
  : > "$T/calls"; rm -f "$T/attached"
  HOME="$T/home" VAST_API_KEY=k STUB_INSTANCES="$INSTANCES" STUB_SSH="$1" \
  STUB_CALLS="$T/calls" STUB_ATTACHED="$T/attached" PATH="$T/bin:$PATH" \
    perl -e 'alarm 20; exec @ARGV' bash "$DEV_POD" tunnel >"$T/out" 2>"$T/err"
  echo $? > "$T/rc"
} 2>/dev/null   # the ceiling's "Alarm clock" job notice is expected in case 2

echo "dev-pod tunnel / refused key:"

# 1. A key the instance never accepts: attach is tried, then exit 1 naming it.
run refuses
if [[ "$(cat "$T/rc")" == 1 ]] && grep -q "^attach ssh 4242" "$T/calls" \
   && grep -q "still refused after attach" "$T/err" && grep -q "BILLING" "$T/err"; then
  pass "a permanently refused key exits 1 with the cause, after one attach"
else
  fail "refused key: rc=$(cat "$T/rc") calls=[$(tr '\n' ';' < "$T/calls")] err=[$(cat "$T/err")]"
fi

# 2. A key that works once attached: attach is called, and the tunnel goes on
#    to its ssh -N loop (the stub ssh returns at once, so the loop is cut by
#    the ceiling — reaching `ssh -N` is the assertion).
run heals
if grep -q "^attach ssh 4242" "$T/calls" && grep -q "^ssh -N" "$T/calls" \
   && ! grep -q "still refused" "$T/err"; then
  pass "a refused key is attached and the tunnel proceeds"
else
  fail "healing key: rc=$(cat "$T/rc") calls=[$(tr '\n' ';' < "$T/calls")] err=[$(cat "$T/err")]"
fi

exit "$rc"
