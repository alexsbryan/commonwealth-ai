#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# The engine conformance battery (PREREG_CONFORMANCE_20261010.md), from the
# HOST; it calls into the sovereign-vulkan toolbox itself.
#
#   battery.sh [--only <case-id>]... <target>...
#
# Targets, each one pass of the resident daemon on a swapped config:
#   embedded       the config as it stands ([engine] kind = "llama")
#   llama-server   [engine] kind = "remote" against one llama-server router on
#                  :18301, built by build-llama-server.sh --llguidance
#   llama-server-matched
#                  the same with no MTP drafting, the configuration the
#                  compute row compares: a drafted token gets no
#                  probabilities, and no request field turns drafting off
#                  in this build (speculative.type "none" and n_max 0 both
#                  still drafted, 2026-10-11)
#
# Each pass replays the case bank (BATTERY_CASES, default the 2026-10-10
# bank) through /internal/engine/replay, which the daemon mounts on its
# operator listener (:9741, beside inference/warmup: server.rs), and writes
# <out>/<n>-<target>.jsonl; then every record file is judged into <out>.
# Name a target twice for an A/A pair. On exit, however it exits, the
# resident is put back on the config it started with.
#
# `sovereign daemon start` hands off to sovereign.service, whose ExecStart
# fixes the daemon's environment, so every pass runs under the same one and
# differs only in config. No reranker is configured yet: SOVEREIGN_RERANK_
# MODEL_PATH cannot reach a service-started daemon, so rerank cases refuse.
set -eu
repo=$(cd "$(dirname "$0")/../../.." && pwd)
cfg=$HOME/.sovereign/config.toml
models=$repo/sovereign/models
embed=$repo/models/qwen-embedding-0.6b.gguf/Qwen3-Embedding-0.6B-Q8_0.gguf
server=$repo/target/llama-server-vanilla/build-llg/bin/llama-server
cases=${BATTERY_CASES:-$repo/target/engine-conformance/cases-20261010.jsonl}
out=$repo/target/engine-conformance/battery-$(date +%Y%m%d-%H%M%S)
tb() { toolbox run -c sovereign-vulkan "$@"; }
say() { echo "battery: $*"; }
die() { echo "battery: $*" >&2; exit 1; }

only=""
while [ $# -gt 0 ]; do
  case $1 in
    --only) only="$only --only ${2:?--only takes a case id}"; shift 2 ;;
    --) shift; break ;;
    -*) die "unknown flag $1" ;;
    *) break ;;
  esac
done
[ $# -gt 0 ] || { echo "battery.sh [--only <case-id>]... embedded|llama-server|llama-server-matched..." >&2; exit 2; }
for t in "$@"; do
  case $t in embedded|llama-server|llama-server-matched) ;; *) die "unknown target $t" ;; esac
done

# ── preflight: refuse rather than share the box ────────────────────────────

preflight() {
  [ -f "$cases" ] || die "no case bank at $cases"
  [ -x "$server" ] || die "no llama-server at $server"
  # Available, not free: page cache from reading the GGUFs is reclaimed on
  # demand, and counting it refused a box with 89G available (2026-10-10).
  free_g=$(free -g | awk '/^Mem:/ {print $7}')
  [ "$free_g" -ge 60 ] || die "available memory ${free_g}G, the battery needs 60G"
  if pgrep -x llama-server >/dev/null; then
    die "a llama-server is running (pid $(pgrep -x llama-server | tr '\n' ' ')); stop probe servers first"
  fi
  if ss -ltn 'sport = :18301' | grep -q LISTEN; then die "port 18301 is taken"; fi
  # The resident must be doing nothing: no slot mid-load, no peer served.
  # Held slots are no reason to refuse, since every pass stops the daemon and
  # a fresh one loads fast and embed at boot (watched 2026-10-10); their
  # memory already counts against the available check above.
  if status=$(curl -sf -m 5 localhost:9741/status); then
    busy=$(printf '%s' "$status" | python3 -c '
import json, sys
inf = json.load(sys.stdin)["inference"]
busy = [s["role"] + " loading" for s in inf["resident"] if s["transitioning"]]
busy += ["a peer request"] * len(inf.get("peer_requests") or [])
print(", ".join(busy))')
    [ -z "$busy" ] || die "the resident daemon is busy: $busy"
  fi
  # The inventory must load: an empty record set judges nothing (exit 4),
  # an inventory that does not parse exits 2.
  rc=0; sovereign bench engine-conformance judge --records /dev/null >/dev/null 2>&1 || rc=$?
  [ $rc -eq 4 ] || die "the inventory does not load (judge on no records exited $rc, want 4)"
  say "preflight passed: ${free_g}G available, no llama-server, :18301 free, resident not busy, inventory loads"
}

# ── the resident daemon, restarted per target ──────────────────────────────

daemon_down() {
  tb sovereign daemon stop >/dev/null 2>&1 || true
  i=0; while curl -sf -m 2 localhost:9741/health >/dev/null; do
    i=$((i + 1)); [ $i -lt 60 ] || die "daemon still answering after 60s"; sleep 1
  done
}

daemon_up() {
  tb sovereign daemon start >"$out/daemon-start-$1.log" 2>&1 || true
  curl -sf -m 5 localhost:9741/health >/dev/null || die "daemon not up, see $out/daemon-start-$1.log"
  pid=$(pgrep -x sovereign-stock)
  say "daemon up for $1, pid $pid, exe $(readlink -f /proc/$pid/exe)"
}

write_config() {
  case $1 in
    embedded) cp "$out/config.before.toml" "$cfg" ;;
    llama-server|llama-server-matched)
      python3 - "$out/config.before.toml" "$cfg" <<'EOF'
import re, sys
src, dst = sys.argv[1], sys.argv[2]
text = open(src).read()
engine = '''[engine]
kind = "remote"
endpoint = "http://127.0.0.1:18301/v1"
model_id = "Qwen3.6-35B-A3B-MTP-UD-Q6_K"
fast_model_id = "Qwen3.5-4B-UD-MTP-Q6_K_XL"
embed_model_id = "Qwen3-Embedding-0.6B-Q8_0"
embed_inputs = "client"
grammar = "llguidance"
context_size = 65536
'''
new, n = re.subn(r'(?ms)^\[engine\]\n.*?(?=^\[|\Z)', engine + "\n", text)
assert n == 1, "exactly one [engine] table"
open(dst, "w").write(new)
EOF
      ;;
  esac
}

# ── the llama-server router, for remote targets ────────────────────────────

router_down() {
  [ -f "$out/router.pid" ] || return 0
  kill "$(cat "$out/router.pid")" 2>/dev/null || true
  rm -f "$out/router.pid"
  i=0; while pgrep -x llama-server >/dev/null; do
    i=$((i + 1)); [ $i -lt 60 ] || die "llama-server still running after 60s"; sleep 1
  done
}

# $1: "spec" to draft with each model's MTP head, as the serving config does.
router_up() {
  if [ "$1" = spec ]; then
    spec='spec-type = draft-mtp
spec-draft-n-max = 3'
  else
    spec=''
  fi
  cat > "$out/router.ini" <<EOF
version = 1

[*]
n-gpu-layers = 99

[Qwen3.6-35B-A3B-MTP-UD-Q6_K]
model = $models/Qwen3.6-35B-A3B-MTP-UD-Q6_K.gguf
ctx-size = 65536
$spec

[Qwen3.5-4B-UD-MTP-Q6_K_XL]
model = $models/Qwen3.5-4B-UD-MTP-Q6_K_XL.gguf
ctx-size = 65536
$spec

[Qwen3-Embedding-0.6B-Q8_0]
model = $embed
embedding = true
ctx-size = 8192
batch-size = 8192
ubatch-size = 8192
EOF
  # --slots: the driver reads the sampler the server applied from /slots, and
  # LLAMA_SERVER_SLOTS_DEBUG makes it describe the grammar too (metrics-only
  # form omits it: LS/tools/server/server-context.cpp:2418). The router hands
  # its environment to each model's child.
  tb sh -c "LLAMA_SERVER_SLOTS_DEBUG=1 '$server' --models-preset '$out/router.ini' --models-max 4 --slots \
    --host 127.0.0.1 --port 18301 > '$out/router.log' 2>&1 & echo \$! > '$out/router.pid'"
  i=0; until curl -sf localhost:18301/v1/models >/dev/null; do
    i=$((i + 1)); [ $i -lt 120 ] || die "router not up, see $out/router.log"; sleep 1
  done
  # Load every model before the first case, so no case pays a load.
  for m in Qwen3.6-35B-A3B-MTP-UD-Q6_K Qwen3.5-4B-UD-MTP-Q6_K_XL; do
    curl -sf -m 600 localhost:18301/v1/chat/completions -H 'content-type: application/json' \
      -d "{\"model\":\"$m\",\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}],\"max_tokens\":4}" >/dev/null \
      || die "router would not load $m, see $out/router.log"
  done
  curl -sf -m 600 localhost:18301/v1/embeddings -H 'content-type: application/json' \
    -d '{"model":"Qwen3-Embedding-0.6B-Q8_0","input":"hi"}' >/dev/null \
    || die "router would not load the embedder, see $out/router.log"
  say "router up on :18301 ($1), three models loaded"
}

restore() {
  rc=$?
  trap - EXIT INT TERM
  say "restoring the resident config"
  daemon_down
  router_down
  [ -f "$out/config.before.toml" ] && cp "$out/config.before.toml" "$cfg"
  tb sovereign daemon start >"$out/daemon-start-restore.log" 2>&1 || true
  curl -sf -m 5 localhost:9741/health >/dev/null && say "resident back up" \
    || echo "battery: the resident did not come back, see $out/daemon-start-restore.log" >&2
  exit $rc
}

# ── run ────────────────────────────────────────────────────────────────────

preflight
mkdir -p "$out"
cp "$cfg" "$out/config.before.toml"
trap restore EXIT
trap 'exit 130' INT TERM

n=0; records=""
for t in "$@"; do
  n=$((n + 1))
  daemon_down
  router_down
  write_config "$t"
  server_flag=""
  case $t in
    llama-server) router_up spec ;;
    llama-server-matched) router_up nospec ;;
  esac
  case $t in llama-server*) server_flag="--server http://127.0.0.1:18301" ;; esac
  daemon_up "$t"
  rec=$out/$n-$t.jsonl
  t0=$(date +%s)
  # shellcheck disable=SC2086 # $only and $server_flag are word lists by design
  sovereign bench engine-conformance run --target "$t" --daemon http://127.0.0.1:9741 \
    $server_flag --cases "$cases" --out "$rec" $only 2>&1 | tee "$out/$n-$t.run.log"
  # tee hides the driver's exit code; an empty record file is its exit 1.
  [ -s "$rec" ] || die "$t replayed nothing, see $out/$n-$t.run.log"
  say "$t replayed in $(( $(date +%s) - t0 ))s, $(wc -l < "$rec") records in $rec"
  records="$records --records $rec"
done

rc=0
# shellcheck disable=SC2086
sovereign bench engine-conformance judge $records --out "$out" >"$out/judge.log" 2>&1 || rc=$?
cat "$out/judge.log"
say "judged into $out (judge exit $rc: 0 as predicted, 3 a verdict differs, 4 nothing judged)"
exit $rc
