#!/usr/bin/env bash
# Run the local daemon with ONE arm's env (arms.toml), or put the registered daemon back.
#
#   harness/arm_daemon.sh <arm> [pool_scale]     stop the registered daemon, start one with the arm's env
#   harness/arm_daemon.sh restore                stop the arm daemon, restart the registered one
#
# Since pb-bench-dials-turns every eval turn runs in the daemon answering :9741, not in the
# eval process, so an arm's env must be THAT process's env. run_arm.py reads the subject's
# env and refuses an arm it does not carry; this is how an arm gets carried.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../../.." && pwd)
PIDF=$HOME/.svrnmesh/logs/arm-daemon.pid
up() { for _ in $(seq 1 120); do curl -s -m 3 http://127.0.0.1:9741/v1/models >/dev/null && return 0; sleep 5; done; return 1; }
down() { for _ in $(seq 1 60); do lsof -nP -iTCP:9741 -sTCP:LISTEN -t >/dev/null || return 0; sleep 2; done; return 1; }
# A daemon can answer /v1/models with a dead GPU backend (Metal OOM at the primary slot's first
# prefill leaves every later decode at ret -3, 2026-10-03), so "up" means one primary decode worked.
decodes() {
  curl -s -m 600 http://127.0.0.1:9741/v1/chat/completions -H 'Content-Type: application/json' \
    -d '{"model":"primary","max_tokens":4,"temperature":0,"messages":[{"role":"user","content":"Say OK."}]}' \
    | python3 -c "import json,sys; d=json.load(sys.stdin); sys.exit(0 if d.get('choices') else 1)"
}
stop_arm() { if [ -f "$PIDF" ]; then kill "$(cat "$PIDF")" 2>/dev/null || true; rm -f "$PIDF"; fi; }

if [ "${1:-}" = restore ]; then
  stop_arm; down || { echo "arm_daemon: :9741 still held" >&2; exit 1; }
  sovereign daemon restart
  exit 0
fi

ARM=${1:?usage: arm_daemon.sh <arm> [pool_scale] | restore}
ENV=$(cd "$HERE" && python3 -c "
import sys, run_arm
arm = run_arm.load_arms().get(sys.argv[1]) or sys.exit(f'unknown arm {sys.argv[1]}')
ps = int(sys.argv[2]) if sys.argv[2] else None
print(' '.join(f'{k}={v}' for k, v in run_arm.arm_env(arm, ps).items()))
" "$ARM" "${2:-}")
echo "arm_daemon: $ARM -> ${ENV:-(no env)}"
stop_arm
sovereign daemon stop || true
down || { echo "arm_daemon: :9741 still held after stop" >&2; exit 1; }
cd "$HOME/.svrnmesh"
# shellcheck disable=SC2086
env $ENV nohup "$REPO/target/debug/sovereign-cli-daemon" daemon run \
  >>"$HOME/.svrnmesh/logs/daemon.log" 2>>"$HOME/.svrnmesh/logs/daemon.err" &
echo $! > "$PIDF"
up || { echo "arm_daemon: daemon did not answer :9741" >&2; exit 1; }
decodes || { echo "arm_daemon: :9741 answers but the primary slot cannot decode (see daemon.err for Metal OOM)" >&2; exit 1; }
echo "arm_daemon: $ARM up and decoding (pid $(cat "$PIDF"))"
