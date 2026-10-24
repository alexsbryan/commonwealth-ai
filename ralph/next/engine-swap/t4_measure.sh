#!/bin/sh
# es-t4's measurement, end to end: Codex-shaped conversations, counted.
#
# Two arms against ONE `[engine] kind = "llama-server"` router (the shipped
# llama-server this lane built):
#
#   passthrough  a Responses turn as `/v1/responses` serves it on the router
#                (es-t4's change): a conversation on the model's own
#                template; the daemon checks the forced call (`oicp_client::
#                call_check`, es-t1).
#   hermes       the shape every Codex turn took before es-t4: a flattened
#                single turn with the tool-envelope grammar. Codex turns
#                cannot be asked for it on the router any more (es-t4 lifts
#                exactly their two reasons), so the arm is the caller shape
#                it stands in for: the same turn with no `think_budget`,
#                and an EXTERNAL caller's forced `tool_choice` that the
#                envelope grammar holds (`SOVEREIGN_FORCE_TOOL_CALLS=1`).
#                The daemon reports which path each turn took
#                (`inference_adapter: prompt path chosen`, debug).
#
# Boots sovereign-stock on a temp root with its own ports, HOME and data dir
# beside the deployed daemon. That daemon holds this host's ports and
# `~/.sovereign`; neither is touched.
#
# Usage: t4_measure.sh <stock-binary> <chat-gguf> [embed-gguf]
set -eu

BIN=${1:?sovereign-stock binary}
CHAT=${2:?chat GGUF}
EMBED=${3:-$HOME/dev/commonwealth-ai/sovereign/models/Qwen3-Embedding-0.6B-Q8_0.gguf}
HERE=$(dirname "$0")

free_port() {
  python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])'
}
SVRN=$(free_port)
MODEL=$(basename "$CHAT" .gguf)
INTERNAL=$(free_port)
SERVE=$(free_port)
RAILS=$(free_port)

root=$(mktemp -d)
pid=
# The router is a child SUPERVISOR with its own llama-server children; a
# SIGKILL of the daemon leaves them (the e2e proves production's parent-death
# signal and this harness is not that path), so tear the whole tree down.
stop() {
  for p in $(pgrep -f "llama-server.*--log-file $root/" 2>/dev/null); do kill -9 "$p" 2>/dev/null || true; done
  [ -n "$pid" ] && { kill -9 "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; }
  rm -rf "$root"
}
trap stop EXIT INT TERM

mkdir -p "$root/home" "$root/rails" "$root/data" "$root/svrnmesh"
config="$root/svrnmesh/config.toml"
cat >"$config" <<EOF
[engine]
kind = "llama-server"

[models]
primary = "$CHAT"
embed = "$EMBED"
context_size = 4096

[daemon]
client_port = $SVRN
internal_port = $INTERNAL
rails_base = "http://127.0.0.1:$RAILS"

[data]
dir = "$root/data"
EOF

FORCE_TOOL_CALLS=0

boot() {
  log=$1
  shift
  env HOME="$root/home" \
    SOVEREIGN_FORCE_TOOL_CALLS="$FORCE_TOOL_CALLS" \
    SVRNMESH_DATA_DIR="$root/svrnmesh" \
    CW_RAILS_DIR="$root/rails" \
    CW_RAILS_BIN="$root/no-cw-rails" \
    SOVEREIGN_SERVE_PORT="$SERVE" \
    RUST_LOG="${RUST_LOG:-sovereign_serving_host=debug,oicp_client=info}" \
    "$BIN" run --config "$config" >"$log" 2>&1 &
  pid=$!
  i=0
  while [ $i -lt 600 ]; do
    if curl -fsS "http://127.0.0.1:$SVRN/status" >/dev/null 2>&1; then return 0; fi
    if ! kill -0 "$pid" 2>/dev/null; then echo "daemon exited:"; tail -20 "$log"; exit 1; fi
    i=$((i + 1))
    sleep 1
  done
  echo "daemon never answered; last lines:"; tail -20 "$log"; exit 1
}

echo "== passthrough arm: the Responses turn as es-t4 serves it on the router =="
boot "$root/passthrough.log"
python3 "$HERE/t4_measure.py" "$SVRN" passthrough "$MODEL" | tee "$root/passthrough.txt"
echo "-- prompt path chosen --"
grep -h "prompt path chosen" "$root/passthrough.log" | sed -E 's/^.*(conversation=)/\1/' | sort | uniq -c || true
echo "-- required call check --"
grep -h "required call check" "$root/passthrough.log" | sed -E 's/^.*(required call check)/\1/' | sort | uniq -c || true
# A clean SIGKILL of the daemon would strand its router; the daemon's own
# children are torn down by pattern before the second arm boots.
for p in $(pgrep -f "llama-server.*--log-file $root/" 2>/dev/null); do kill -9 "$p" 2>/dev/null || true; done
kill -9 "$pid" 2>/dev/null || true
wait "$pid" 2>/dev/null || true
pid=

echo "== hermes arm: the pre-es-t4 shape, the envelope grammar holding the call =="
FORCE_TOOL_CALLS=1
boot "$root/hermes.log"
python3 "$HERE/t4_measure.py" "$SVRN" hermes "$MODEL" --no-budget | tee "$root/hermes.txt"
echo "-- prompt path chosen --"
grep -h "prompt path chosen" "$root/hermes.log" | sed -E 's/^.*(conversation=)/\1/' | sort | uniq -c || true
echo "-- required call check --"
grep -h "required call check" "$root/hermes.log" | sed -E 's/^.*(required call check)/\1/' | sort | uniq -c || true
