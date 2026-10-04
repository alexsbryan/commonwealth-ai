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
#
#   run.sh                    the whole unit
#   run.sh --preflight-only   the dependency table and nothing else, ~1 s
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

# The out dir and the markers file are created FIRST, before anything that can
# fail. Run 1 (2026-09-07) exited 2 in its first second on a missing model and
# left no out dir and no markers.txt, which a watcher reads as a unit that never
# started — the same trap ei-6 fixed on 2026-09-05. Every exit path from here on
# writes `markers.txt` and `DONE`, SIGTERM included.
# A DRY run writes to a throwaway dir. Run 2 (2026-09-07) was reported DONE off
# artifacts that were not its own — a host-side test of the preflight had left
# `out/` populated, the unit was still queued on the build lock, and the seat
# read `preflight rc=1 / DONE rc=2` from files nobody had told it were stale.
# Two structural answers, both here: a dry run never touches `out/`, and a real
# run RESETS `out/` before it writes, so what is in there is always this run's.
if [[ "${1:-}" == "--preflight-only" ]]; then
  OUT="$(mktemp -d)"
  trap 'rm -rf "$OUT"' EXIT
else
  OUT="$RUN_DIR/out"
  rm -rf "$OUT"
fi
mkdir -p "$OUT"
: > "$OUT/markers.txt"
mark() {
  echo "$1 rc=$2" >> "$OUT/markers.txt"
  echo "$2" > "$OUT/rc.$1"
  echo "== leg $1 -> $2" >&2
}
serve_pid=""
finish() {
  rc=$?
  [[ -n "$serve_pid" ]] && kill "$serve_pid" 2>/dev/null
  {
    echo "finished: $(date -Is)"
    free -g | head -2
    df -h /home | tail -1
  } > "$OUT/box-after.txt" 2>&1
  echo "DONE rc=$rc" >> "$OUT/markers.txt"
  echo "DONE rc=$rc $(date -Is)" > "$OUT/DONE"
}
if [[ "${1:-}" != "--preflight-only" ]]; then
  trap finish EXIT
  trap 'echo "signal=TERM" >> "$OUT/markers.txt"; exit 143' TERM
  trap 'echo "signal=INT"  >> "$OUT/markers.txt"; exit 130' INT
fi

{
  echo "started: $(date -Is)"
  echo "container: $(head -1 /run/.containerenv 2>/dev/null || echo HOST)"
  free -g | head -2
  df -h /home | tail -1
  pgrep -af 'cargo|rustc' | grep -v pgrep || echo "no builds running"
} > "$OUT/box-before.txt" 2>&1

# ── preflight: everything a LATER leg needs, checked BEFORE the long ones ────
# What the ACCEPTANCE needs is acceptance.sh's own list, asked for by name
# rather than kept twice here (ARCH §10.6). It resolves the embed gguf against
# the main checkout when this runs from a worktree — `models/` is gitignored, so
# no worktree carries it, which is exactly how run 1 died.
#
# EVERY check writes its verdict to out/preflight.txt, passing ones included.
# Run 2 (2026-09-07) exited 2 with a preflight.txt holding only the ONE check
# that passed: the three others reported to stderr, and under `systemd-run
# --scope` behind a wait loop that stderr reached neither the journal nor the
# out dir. A refusal a runner cannot read from its own artifacts is not a
# reported absence (ARCH §18.3) — it is a silent one, and it cost a whole
# scheduling round to guess at.
pf_fail=0
pf() {  # pf <name> <rc> <detail>
  local st=PASS
  (( $2 == 0 )) || { st=FAIL; pf_fail=1; }
  printf '%-12s %-4s %s\n' "$1" "$st" "${3:-}" >> "$OUT/preflight.txt"
  echo "PREFLIGHT $1 $st ${3:-}" >&2
}
{ echo "preflight $(date -Is)"; echo "HOME=$HOME PATH=$PATH"; } >> "$OUT/preflight.txt"

[[ -f /run/.containerenv ]]
pf toolbox $? "/run/.containerenv — must run INSIDE sovereign-vulkan; llama-server needs Vulkan, ROCm x A3B SEGVs on the host"
# What the ACCEPTANCE needs is acceptance.sh's own list, asked for by name
# rather than kept twice here (ARCH §10.6). It resolves the embed gguf against
# the main checkout when this runs from a worktree — `models/` is gitignored, so
# no worktree carries it, which is exactly how run 1 died.
acc_pf="$("$REPO/corpus-mcp/acceptance.sh" --preflight 2>&1)"
pf acceptance $? "$acc_pf"
# What only THIS unit needs, beyond the acceptance's own list.
ollama_bin="$(command -v ollama || true)"
[[ -n "$ollama_bin" ]]
pf ollama $? "${ollama_bin:-not on PATH; looked under $OLLAMA_HOME/bin}"
[[ -x "${ollama_bin:-/nonexistent}" ]] && "$ollama_bin" --version >/dev/null 2>&1
pf ollama-runs $? "the binary executes here (a tarball built for another libc would fail HERE, not at serve)"
[[ -d "$HOME/.svrnmesh/indexes/sep" ]]
pf sep-index $? "$HOME/.svrnmesh/indexes/sep — the arm's corpus_list target"

mark preflight "$pf_fail"
# A dry mode, so the preflight can be verified in one second without a lane
# window and without starting `ollama serve`. `--preflight-only` is what the
# re-request was checked with.
if [[ "${1:-}" == "--preflight-only" ]]; then
  cat "$OUT/preflight.txt"
  exit "$pf_fail"
fi
(( pf_fail == 0 )) || exit 2

# ── leg 1: ollama serve, CPU ────────────────────────────────────────────────
ollama serve > "$OUT/ollama-serve.log" 2>&1 &
serve_pid=$!
up=1
for _ in $(seq 1 60); do
  curl -sf -m 3 "$OLLAMA_URL/models" >/dev/null 2>&1 && { up=0; break; }
  kill -0 "$serve_pid" 2>/dev/null || break
  sleep 1
done
mark serve "$up"
(( up == 0 )) || { tail -30 "$OUT/ollama-serve.log" >&2; exit 3; }
grep -iE 'inference compute|gpu|library=|no compatible' "$OUT/ollama-serve.log" | head -10 \
  > "$OUT/ollama-device.txt" 2>&1 || true
echo "-- device lines ollama reported:"; cat "$OUT/ollama-device.txt"

# ── leg 2: the two model pulls (new egress for this BOX, not for the product) ─
pull_rc=0
for tag in "$EMBED_TAG" "$CHAT_TAG"; do
  echo "== pulling $tag"
  ollama pull "$tag" >> "$OUT/ollama-pull.log" 2>&1 || pull_rc=1
done
ollama list >> "$OUT/ollama-pull.log" 2>&1
curl -s "$OLLAMA_URL/models" | python3 -m json.tool > "$OUT/ollama-models.json" 2>&1 || true
mark pull "$pull_rc"
(( pull_rc == 0 )) || { tail -20 "$OUT/ollama-pull.log" >&2; exit 4; }

# The width question EPISTEMIC_INDEX.md §7 step 6 asks, answered directly before
# the acceptance run so it is on the record whichever way the arm goes.
python3 - "$OLLAMA_URL" "$EMBED_TAG" > "$OUT/embed-width.txt" 2>&1 <<'PY'
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
cat "$OUT/embed-width.txt"

# ── leg 3: the acceptance, once, with Ollama reachable ──────────────────────
# Read arm only: ACCEPT_INGEST and ACCEPT_PULL stay unset, so the 60-90 min
# ingest and the 875 MB snapshot pull both report NEVER-RAN by name as usual.
cd "$REPO"
OLLAMA_URL="$OLLAMA_URL" CORPUS_MCP="$REPO/target/debug/corpus-mcp" \
  ./corpus-mcp/acceptance.sh > "$OUT/acceptance.log" 2>&1
acc_rc=$?
mark acceptance "$acc_rc"
grep -E '^acceptance: ollama' "$OUT/acceptance.log" || echo "(no ollama lines — read the log)"
tail -25 "$OUT/acceptance.log"

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
      > "$OUT/named-model.jsonl" 2> "$OUT/named-model.err"
named_rc=$?
mark named-model "$named_rc"
grep -E 'embeddings via|vector search|DISABLED' "$OUT/named-model.err" | head -5

exit "$acc_rc"
