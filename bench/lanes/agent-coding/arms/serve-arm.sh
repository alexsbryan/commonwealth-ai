#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Bring up one arm of the daemon-vs-llama-server coding A/B on one port, and
# hold it there until killed. Runs inside the sovereign-vulkan toolbox.
#
#   A  llama-server built from the llama.cpp commit the daemon vendors
#      (target/llama-server-vanilla, unpatched), no daemon in the path
#   A0 A with `--reasoning off`: the same kernels without the thinking
#      channel. The TDD machine's search runner needs it — at thinking-on
#      defaults one turn of this model ran 10-11k reasoning tokens, past
#      its 4000-token candidate cap before any content.
#   B  the stock daemon as shipped, release-built, on its client port
#   C  B with SOVEREIGN_FRONTDOOR_RESHAPE=0 (turn_fidelity::reshape_enabled):
#      the request nudges off, the serving layer otherwise identical
#
# What every arm shares is the model's identity, not the server's behaviour:
# one GGUF, one context window per request (`--parallel 1`, the daemon's
# single sequence), F16 KV, n_ubatch 512, MTP drafting at depth 3 (the
# daemon's default, model_slot.rs `mtp_draft_max_decide`). Prompt caching,
# sampler defaults and chat rendering are left at each server's own default,
# because those are what the A/B measures.
#
#   serve-arm.sh <A|A0|B|C> <port> [ctx]
#
# Stop an arm by its pid file: kill "$(cat target/agent-coding-arms/<arm>-<port>/pid)".
# Killing the `toolbox run` that started it does not reach the server.
#
# The daemon arms are installed and run outside the checkout, with their own
# HOME, data dirs and closed rails base, as scripts/idle-stock-bars.sh does:
# a binary under target/ finds the checkout and watches it.
set -eu
arm=$1 port=$2 ctx=${3:-65536}
repo=$(cd "$(dirname "$0")/../../../.." && pwd)
gguf=${AGENT_ARM_GGUF:-$repo/sovereign/models/Qwen3.8-27B-UD-Q6_K_XL.gguf}
out=$repo/target/agent-coding-arms/$arm-$port
mkdir -p "$out"
[ -f "$gguf" ] || { echo "serve-arm: no GGUF at $gguf" >&2; exit 2; }

case $arm in
  A|A0)
    bin=$repo/target/llama-server-vanilla/build/bin/llama-server
    [ -x "$bin" ] || { echo "serve-arm: no $bin (bench/lanes/agent-coding/arms/build-llama-server.sh)" >&2; exit 2; }
    reasoning=auto
    [ "$arm" = A0 ] && reasoning=off
    "$bin" -m "$gguf" -c "$ctx" -ngl 99 --parallel 1 \
      --spec-type draft-mtp --spec-draft-n-max 3 --reasoning "$reasoning" \
      --host 127.0.0.1 --port "$port" > "$out/server.log" 2>&1 &
    ;;
  B|C)
    bin=${AGENT_ARM_STOCK_BIN:-$repo/target/release/sovereign-stock}
    [ -x "$bin" ] || { echo "serve-arm: no stock binary at $bin (scripts/dev-release.sh -p sovereign-stock)" >&2; exit 2; }
    root=$out/root
    mkdir -p "$root/home"
    printf '[models]\nprimary = "%s"\nembed = "%s/absent-embed.gguf"\ncontext_size = %s\n\n[daemon]\nclient_port = %s\ninternal_port = %s\nrails_base = "http://127.0.0.1:%s"\n\n[data]\ndir = "%s/data"\n' \
      "$gguf" "$root" "$ctx" "$port" $((port + 1)) $((port + 9)) "$root" > "$root/config.toml"
    inst=$HOME/.cache/agent-coding-arms
    mkdir -p "$inst"
    cmp -s "$bin" "$inst/sovereign-stock" || cp "$bin" "$inst/sovereign-stock"
    reshape=1
    [ "$arm" = C ] && reshape=0
    (cd "$inst" && SOVEREIGN_SKIP_VRAM_CHECK=1 SOVEREIGN_FRONTDOOR_RESHAPE=$reshape \
      HOME=$root/home SVRNMESH_DATA_DIR=$root/svrnmesh CW_RAILS_DIR=$root/rails \
      SOVEREIGN_SERVE_PORT=$((port + 2)) RUST_LOG=${RUST_LOG:-info} \
      exec ./sovereign-stock run --config "$root/config.toml") > "$out/server.log" 2>&1 &
    ;;
  *) echo "serve-arm: arm is A, A0, B or C, not $arm" >&2; exit 2 ;;
esac
pid=$!
echo $pid > "$out/pid"
trap 'kill $pid 2>/dev/null; wait $pid 2>/dev/null || true; rm -f "$out/pid"' EXIT INT TERM
i=0
until curl -sf "http://127.0.0.1:$port/v1/models" >/dev/null; do
  kill -0 $pid 2>/dev/null || { echo "serve-arm: $arm died (see $out/server.log)" >&2; exit 1; }
  i=$((i + 1)); [ $i -lt 600 ] || { echo "serve-arm: $arm not answering after 600s" >&2; exit 1; }
  sleep 1
done
echo "serve-arm: $arm up on :$port after ${i}s, pid $pid, gguf $(basename "$gguf"), ctx $ctx"
wait $pid
