#!/usr/bin/env bash
# ei3c-ollama-arm — order ei-3c-instruments item 4.
#
# EPISTEMIC_INDEX.md §4 names Ollama as the default shape the three commands are
# written for, and `corpus-mcp/acceptance.sh`'s step 0b has reported
# COULD-NOT-RUN by name on every run so far because this box had no Ollama. This
# unit installs nothing (the tarball is already unpacked under ~/.local/ollama),
# serves it CPU-only beside the resident daemon, pulls the two smallest Qwen3
# tags, and runs the acceptance ONCE with OLLAMA_URL pointing at it.
#
# CPU-ONLY, and that is a named substitution, not an accident: the base Linux
# tarball carries CUDA libraries and no ROCm ones, this box's GPU work must be
# Vulkan inside the toolbox (ROCm x A3B SEGVs), and the daemon owns the GPU. The
# arm measures the ENDPOINT CONTRACT — does corpus-mcp discover Ollama, pick an
# embedding model, get a width, and serve a cited corpus_list out of `sep` — not
# throughput. Nothing in that contract is a function of the device.
#
# Legs, each with its own rc marker; a DONE marker is written on every exit path
# including SIGTERM, so a killed unit is distinguishable from a hung one.
set -uo pipefail
RUN_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd -- "$RUN_DIR/../.." && pwd)"
export OLLAMA_HOME="${OLLAMA_HOME:-$HOME/.local/ollama}"
export OLLAMA_MODELS="${OLLAMA_MODELS:-$HOME/.local/share/ollama}"
export OLLAMA_HOST="${OLLAMA_HOST:-127.0.0.1:11434}"
export PATH="$OLLAMA_HOME/bin:$PATH"
OLLAMA_URL="http://127.0.0.1:11434/v1"
EMBED_TAG="qwen3-embedding:0.6b"
CHAT_TAG="qwen3:0.6b"

mark() { echo "$2" > "$RUN_DIR/rc.$1"; echo "== leg $1 -> $2" >&2; }
serve_pid=""
finish() {
  [[ -n "$serve_pid" ]] && kill "$serve_pid" 2>/dev/null
  {
    echo "finished: $(date -Is)"
    free -g | head -2
    df -h /home | tail -1
  } > "$RUN_DIR/box-after.txt" 2>&1
  echo "DONE $(date -Is)" > "$RUN_DIR/DONE"
}
trap finish EXIT
trap 'echo "SIGTERM" >> "$RUN_DIR/DONE.reason"; exit 143' TERM INT

{
  echo "started: $(date -Is)"
  echo "container: $(cat /run/.containerenv 2>/dev/null | head -1 || echo HOST)"
  free -g | head -2
  df -h /home | tail -1
  pgrep -af 'cargo|rustc' | grep -v pgrep || echo "no builds running"
} > "$RUN_DIR/box-before.txt" 2>&1

# ── preflight: everything a LATER leg needs, checked BEFORE the long ones ────
pf_fail=0
[[ -f /run/.containerenv ]] || { echo "PREFLIGHT: must run INSIDE the sovereign-vulkan toolbox — llama-server needs Vulkan; ROCm x A3B SEGVs on the host" >&2; pf_fail=1; }
command -v ollama    >/dev/null || { echo "PREFLIGHT: no ollama on PATH ($OLLAMA_HOME/bin)" >&2; pf_fail=1; }
command -v llama-server >/dev/null || { echo "PREFLIGHT: no llama-server on PATH" >&2; pf_fail=1; }
command -v jq        >/dev/null || { echo "PREFLIGHT: no jq" >&2; pf_fail=1; }
command -v python3   >/dev/null || { echo "PREFLIGHT: no python3" >&2; pf_fail=1; }
[[ -x "$REPO/target/debug/corpus-mcp" ]] || { echo "PREFLIGHT: corpus-mcp not built" >&2; pf_fail=1; }
[[ -f "$REPO/sovereign/models/Qwen3-Embedding-0.6B-Q8_0.gguf" ]] || { echo "PREFLIGHT: no embed gguf" >&2; pf_fail=1; }
[[ -d "$HOME/.svrnmesh/indexes/sep" ]] || { echo "PREFLIGHT: sep corpus not installed (the arm's corpus_list target)" >&2; pf_fail=1; }
mark preflight "$pf_fail"
(( pf_fail == 0 )) || exit 2

# ── leg 1: ollama serve, CPU ────────────────────────────────────────────────
ollama serve > "$RUN_DIR/ollama-serve.log" 2>&1 &
serve_pid=$!
up=1
for _ in $(seq 1 60); do
  curl -sf -m 3 "$OLLAMA_URL/models" >/dev/null 2>&1 && { up=0; break; }
  kill -0 "$serve_pid" 2>/dev/null || break
  sleep 1
done
mark serve "$up"
(( up == 0 )) || { tail -30 "$RUN_DIR/ollama-serve.log" >&2; exit 3; }
grep -iE 'inference compute|gpu|library=|no compatible' "$RUN_DIR/ollama-serve.log" | head -10 \
  > "$RUN_DIR/ollama-device.txt" 2>&1 || true
echo "-- device lines ollama reported:"; cat "$RUN_DIR/ollama-device.txt"

# ── leg 2: the two model pulls (new egress for this BOX, not for the product) ─
pull_rc=0
for tag in "$EMBED_TAG" "$CHAT_TAG"; do
  echo "== pulling $tag"
  ollama pull "$tag" >> "$RUN_DIR/ollama-pull.log" 2>&1 || pull_rc=1
done
ollama list >> "$RUN_DIR/ollama-pull.log" 2>&1
curl -s "$OLLAMA_URL/models" | python3 -m json.tool > "$RUN_DIR/ollama-models.json" 2>&1 || true
mark pull "$pull_rc"
(( pull_rc == 0 )) || { tail -20 "$RUN_DIR/ollama-pull.log" >&2; exit 4; }

# The width question EPISTEMIC_INDEX.md §7 step 6 asks, answered directly before
# the acceptance run so it is on the record whichever way the arm goes.
python3 - "$OLLAMA_URL" "$EMBED_TAG" > "$RUN_DIR/embed-width.txt" 2>&1 <<'PY'
import json, sys, urllib.request
url, tag = sys.argv[1], sys.argv[2]
req = urllib.request.Request(f"{url}/embeddings",
        data=json.dumps({"input": "corpus-mcp probe", "model": tag}).encode(),
        headers={"Content-Type": "application/json"})
d = json.load(urllib.request.urlopen(req, timeout=120))
print(f"{tag}: {len(d['data'][0]['embedding'])}-d")
listed = json.load(urllib.request.urlopen(f"{url}/models", timeout=30))
print("models listed, in the order corpus-mcp sees them (it takes the FIRST "
      "when --embed-model is absent):")
for m in listed["data"]:
    print("   ", m["id"])
PY
cat "$RUN_DIR/embed-width.txt"

# ── leg 3: the acceptance, once, with Ollama reachable ──────────────────────
# Read arm only: ACCEPT_INGEST and ACCEPT_PULL stay unset, so the 60-90 min
# ingest and the 875 MB snapshot pull both report NEVER-RAN by name as usual.
cd "$REPO"
OLLAMA_URL="$OLLAMA_URL" CORPUS_MCP="$REPO/target/debug/corpus-mcp" \
  ./corpus-mcp/acceptance.sh > "$RUN_DIR/acceptance.log" 2>&1
acc_rc=$?
mark acceptance "$acc_rc"
grep -E '^acceptance: ollama' "$RUN_DIR/acceptance.log" || echo "(no ollama lines — read the log)"
tail -25 "$RUN_DIR/acceptance.log"

# ── leg 4: the named-model control ──────────────────────────────────────────
# corpus-mcp takes the FIRST model /v1/models lists when --embed-model is absent
# (host.rs:364). Ollama serves chat AND embeddings from one URL, so that first
# entry may be the CHAT model. This second invocation names the embedding model
# explicitly; the two together say whether the default path picked right.
{
  echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"ei3c","version":"0"}}}'
  echo '{"jsonrpc":"2.0","method":"notifications/initialized"}'
  echo '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"corpus_list","arguments":{}}}'
} | "$REPO/target/debug/corpus-mcp" serve --base-url "$OLLAMA_URL" \
      --embed-model "$EMBED_TAG" --corpus sep \
      > "$RUN_DIR/named-model.jsonl" 2> "$RUN_DIR/named-model.err"
named_rc=$?
mark named-model "$named_rc"
grep -E 'embeddings via|vector search|DISABLED' "$RUN_DIR/named-model.err" | head -5

exit "$acc_rc"
