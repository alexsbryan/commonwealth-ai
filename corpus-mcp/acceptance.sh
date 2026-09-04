#!/usr/bin/env bash
# The enrichment-as-plugin acceptance test — the criterion the whole plan is
# for (operator, 2026-09-03):
#
#   Someone should be able to run a query against a corpus-engine MCP with our
#   SEP corpus, using a plain `llama-server` as their inference frontend — and
#   ideally with the enrichments.
#
# Run on a machine with NO sovereign daemon required:
#   1. a bare inference frontend, nothing of ours in it (llama-server);
#   2. corpus-mcp pointed at it, over stdio;
#   3. a cited corpus_search and a tier-1.5 atoms_lookup, asserted on;
#   4. the dep tree, asserted free of llama.cpp / ort / iroh.
#
# Env: EMBED_GGUF (default sovereign/models/Qwen3-Embedding-0.6B-Q8_0.gguf),
#      PORT (8089), CORPUS (sep), ATLAS_CORPUS (sep-freewill),
#      CORPUS_MCP (target/debug/corpus-mcp), QUERY.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/.." && pwd)"
cd "$repo"

EMBED_GGUF="${EMBED_GGUF:-sovereign/models/Qwen3-Embedding-0.6B-Q8_0.gguf}"
PORT="${PORT:-8089}"
CORPUS="${CORPUS:-sep}"
ATLAS_CORPUS="${ATLAS_CORPUS:-sep-freewill}"
CORPUS_MCP="${CORPUS_MCP:-target/debug/corpus-mcp}"
QUERY="${QUERY:-van Inwagen consequence argument}"
work="$(mktemp -d)"
trap 'kill ${server_pid:-} 2>/dev/null || true; rm -rf "$work"' EXIT

fail() { echo "acceptance: FAIL — $*" >&2; exit 1; }
[[ -x "$CORPUS_MCP" ]] || fail "$CORPUS_MCP not built (cargo build -p corpus-mcp)"
[[ -f "$EMBED_GGUF" ]] || fail "embedding model $EMBED_GGUF not found"
command -v llama-server >/dev/null || fail "llama-server not on PATH"

# ── 1. bare frontend ────────────────────────────────────────────────────────
llama-server -m "$EMBED_GGUF" --embeddings --host 127.0.0.1 --port "$PORT" \
  >"$work/llama-server.log" 2>&1 &
server_pid=$!
for _ in $(seq 1 120); do
  curl -sf "http://127.0.0.1:$PORT/health" >/dev/null 2>&1 && break
  kill -0 "$server_pid" 2>/dev/null || { cat "$work/llama-server.log" >&2; fail "llama-server exited"; }
  sleep 1
done
curl -sf "http://127.0.0.1:$PORT/health" >/dev/null || fail "llama-server never became healthy"
cap_code="$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/oicp/v1/capabilities")"
echo "acceptance: frontend up on :$PORT; /oicp/v1/capabilities -> $cap_code (404 = the baseline path under test)"

# ── 2 + 3. the host, driven over stdio ──────────────────────────────────────
{
  echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"acceptance","version":"0"}}}'
  echo '{"jsonrpc":"2.0","method":"notifications/initialized"}'
  echo '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
  printf '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"corpus_search","arguments":{"query":"%s","corpus":"%s"}}}\n' "$QUERY" "$CORPUS"
  printf '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"atoms_lookup","arguments":{"corpus":"%s","kind":"claim","limit":5}}}\n' "$ATLAS_CORPUS"
  printf '{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"corpus_ontology","arguments":{"corpus":"%s"}}}\n' "$ATLAS_CORPUS"
} | "$CORPUS_MCP" --base-url "http://127.0.0.1:$PORT/v1" --corpus "$CORPUS" \
    >"$work/out.jsonl" 2>"$work/err.log" || { cat "$work/err.log" >&2; fail "corpus-mcp exited non-zero"; }

echo "--- corpus-mcp stderr ---"; cat "$work/err.log"; echo "-------------------------"
grep -q 'baseline OpenAI-compatible path' "$work/err.log" || fail "host was not detected as baseline — the test did not exercise the no-OICP path"

python3 - "$work/out.jsonl" "$CORPUS" <<'PY'
import json, sys
out, corpus = sys.argv[1], sys.argv[2]
by_id = {}
for line in open(out):
    line = line.strip()
    if not line: continue
    m = json.loads(line); by_id[m.get("id")] = m
def result(i):
    m = by_id.get(i) or sys.exit(f"acceptance: FAIL — no response to id {i}")
    if "error" in m and m["error"]: sys.exit(f"acceptance: FAIL — id {i} errored: {m['error']}")
    return m["result"]
init = result(1); assert init["serverInfo"]["name"] == "corpus-mcp", init
tools = {t["name"] for t in result(2)["tools"]}
assert {"corpus_list","corpus_search","atoms_lookup","corpus_ontology"} <= tools, tools
search = result(3)
assert not search.get("isError"), search["content"][0]["text"][:400]
rows = search["structuredContent"]["results"]
assert rows, "corpus_search returned no chunks"
cited = [r for r in rows if r.get("url") and r.get("corpus_id") == corpus and r.get("content")]
assert cited, f"no cited chunk with url+corpus+content: {rows[:2]}"
print(f"acceptance: corpus_search -> {len(rows)} chunks, {len(cited)} cited; top: {rows[0]['title']} {rows[0]['url']} score={rows[0]['score']:.4f}")
atoms = result(4)
assert not atoms.get("isError"), atoms["content"][0]["text"][:400]
arows = atoms["structuredContent"]["atoms"]
assert arows and all(a["kind"] == "claim" for a in arows), arows[:2]
print(f"acceptance: atoms_lookup -> {len(arows)} Claim atoms of {atoms['structuredContent']['total_atoms']} in {atoms['structuredContent']['corpus']}; first: {arows[0]['text'][:120]!r}")
ont = result(5)
print(f"acceptance: corpus_ontology -> {'declared' if not ont.get('isError') else 'absent, reported: ' + ont['content'][0]['text'][:120]}")
PY

# ── 4. the closure ──────────────────────────────────────────────────────────
if command -v cargo >/dev/null; then
  bad="$(cargo tree -p corpus-mcp -e normal --prefix none 2>/dev/null | awk '{print $1}' | sort -u \
         | grep -E '^(llama-cpp-4|llama-cpp-sys-4|ort|ort-sys|iroh|sovereign-inference|sovereign-gliner|commonwealth-transport|sovereign-core|sovereign-tools)$' || true)"
  [[ -z "$bad" ]] || fail "dep tree carries: $bad"
  echo "acceptance: cargo tree -p corpus-mcp: no llama.cpp / ort / iroh / mesh transport / runtime"
else
  echo "acceptance: cargo not on PATH here — closure checked by tests/no_inference_stack.rs instead"
fi
if command -v ldd >/dev/null; then
  ldd "$CORPUS_MCP" | grep -iE 'llama|ggml|onnx' && fail "binary links an inference shared library" || true
fi
echo "acceptance: PASS"
