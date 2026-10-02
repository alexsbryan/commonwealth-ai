#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# One run of pb-rails-idle-stock's two bars (phase-b-31, phase-b-33) for ONE
# side: the stock binary or the toolbox's llama-server, on one GGUF and one
# context size. Runs inside the sovereign-vulkan toolbox. The side's server
# comes up, the idle reading is scripts/idle-cpu.py on its pid (settle, then
# window), and first token is scripts/throughput_probe.py at the side's
# /v1/chat/completions (the stock side also reads svrn's full turn beside,
# gating nothing). A reading already on disk is skipped, so a killed session
# resumes where it stopped.
#
#   scripts/idle-stock-bars.sh <stock|llama> <run> <gguf> <ctx> <stock-bin>
set -eu
side=$1 run=$2 gguf=$3 ctx=$4 bin=$5
out=target/ralph/phase-b/idle/stock
label=$side-r$run
root=$PWD/$out/roots/$label
mkdir -p "$out" "$root/home"
[ -s "$out/$label.ttft.json" ] && grep -q "\"$label\"" "$out/readings.jsonl" 2>/dev/null && {
  echo "idle-stock-bars: $label already read"; exit 0; }

serve=18492 svrn=18490
case $side in
  llama)
    llama-server -m "$gguf" -c "$ctx" -ngl 99 --host 127.0.0.1 --port $serve \
      > "$out/$label.server.log" 2>&1 &
    ;;
  stock)
    printf '[models]\nprimary = "%s"\nembed = "%s/absent-embed.gguf"\ncontext_size = %s\n\n[daemon]\nclient_port = %s\ninternal_port = 18491\nrails_base = "http://127.0.0.1:18499"\n\n[data]\ndir = "%s/data"\n' \
      "$gguf" "$root" "$ctx" $svrn "$root" > "$root/config.toml"
    # Installed and run outside any checkout, as a distribution is: a binary
    # under this repo's target/ finds the checkout from current_exe
    # (sovereign-daemon workspace.rs:97) and puts an inotify watch on it.
    inst=$HOME/.cache/pb-idle-stock
    mkdir -p "$inst"
    cmp -s "$bin" "$inst/sovereign-stock" || cp "$bin" "$inst/sovereign-stock"
    (cd "$inst" && SOVEREIGN_SKIP_VRAM_CHECK=1 HOME=$root/home SVRNMESH_DATA_DIR=$root/svrnmesh \
      CW_RAILS_DIR=$root/rails SOVEREIGN_SERVE_PORT=$serve RUST_LOG=info \
      exec ./sovereign-stock run --config "$root/config.toml") > "$out/$label.server.log" 2>&1 &
    ;;
  *) echo "idle-stock-bars: side is stock or llama, not $side" >&2; exit 2 ;;
esac
pid=$!
trap 'kill $pid 2>/dev/null; wait $pid 2>/dev/null || true' EXIT
i=0
until curl -sf http://127.0.0.1:$serve/health >/dev/null; do
  kill -0 $pid 2>/dev/null || { echo "idle-stock-bars: $label server died (see $out/$label.server.log)" >&2; exit 1; }
  i=$((i + 1)); [ $i -lt 300 ] || { echo "idle-stock-bars: $label never healthy in 300s" >&2; exit 1; }
  sleep 1
done
echo "idle-stock-bars: $label healthy after ${i}s, pid $pid, load $(cut -d' ' -f1-3 /proc/loadavg)"

python3 scripts/idle-cpu.py --out "$out" --label "$label" --pid $pid
# A prompt the model answers without a reasoning channel on both sides: the
# probe times `content` deltas, so a reasoning preamble would time its length.
probe() { python3 scripts/throughput_probe.py --url "http://127.0.0.1:$1" --model "$(basename "$gguf" .gguf)" \
  --prompt "Say hello in five words." --max-tokens 32 --trials 5 --warmup 1 --json --label "$2" 2>> "$out/$label.probe.log"; }
{ printf '{"label":"%s","load_before":"%s","serve":' "$label" "$(cut -d' ' -f1-3 /proc/loadavg)"
  probe $serve "$label-serve"
  [ "$side" = stock ] && { printf ',"svrn_full_turn":'; probe $svrn "$label-svrn"; }
  printf ',"load_after":"%s"}\n' "$(cut -d' ' -f1-3 /proc/loadavg)"; } | tr -d '\n' > "$out/$label.ttft.json"
echo >> "$out/$label.ttft.json"
cat "$out/$label.ttft.json"
# A side that streamed no content token has no first token: not a reading.
if grep -q '"ttft_ms_median": null' "$out/$label.ttft.json"; then
  mv "$out/$label.ttft.json" "$out/$label.ttft.invalid.json"
  echo "idle-stock-bars: $label streamed no content token (see $out/$label.probe.log)" >&2; exit 1
fi
