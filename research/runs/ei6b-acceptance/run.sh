#!/usr/bin/env bash
# ei6b-acceptance — item 5: does applying embed quirks move the WHOLE GAME the
# wrong way?
#
# ei-6b changed what corpus-mcp sends to an embedding endpoint on both sides:
# documents now carry the family's EOS, and the atlas seed table is built with
# the QUERY-side embedder instead of the document-side one (the contract
# `build_with_progress_with_embedder` always stated, and which this crate had
# been violating). Cosines say that is right. This run asks whether the corpus
# a third party BUILDS with it is as good as the one our daemon builds.
#
# THE BEFORE AND THE AFTER ARE IN ONE RUN. acceptance.sh's recall leg scores
# `truth.json` through scripts/truth-recall.py — the ONE scorer, ei-3c's, not a
# copy — over TWO atlases: the daemon-built `wessex-hoard` CONTROL and the
# atlas this run just built from bare llama-servers. The control IS the before.
# A second 80-minute run on main would compare a different scorer's numbers
# (ei5b-stage2 predates ei-3c's fix), which is worse evidence, not better.
#
# The bar is acceptance.sh's own: the bare-endpoint atlas must meet every
# truth.json bar the control meets. The script FAILS the run if it is below the
# control on any bar — that gate is not ours and is not tuned here.
#
# CONTROLS: `wessex-hoard` is READ as the control and never written.
# The build lands in a NEW id, `wessex-hoard-bare-ei6b`.
#
# Env the CALLER sets: none. Optional: SKIP_BUILD=1, ALLOW_BUSY_BOX=1.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
out="$here/$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$out"

MODELS="${MODELS:-/home/alexbryan/dev/commonwealth-ai/sovereign/models}"
# The SAME chat model the 2026-09-05 candidate used (runs/ei5b-stage2), so any
# difference is the code path and not the model.
CHAT_GGUF="${CHAT_GGUF:-$MODELS/Qwen3.6-35B-A3B-MTP-UD-Q6_K.gguf}"
EMBED_GGUF="${EMBED_GGUF:-$MODELS/Qwen3-Embedding-0.6B-Q8_0.gguf}"
INGEST_CORPUS="${INGEST_CORPUS:-wessex-hoard-bare-ei6b}"

mark() { printf '%s rc=%s %s\n' "$1" "$2" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "$out/markers.txt"; }
box()  { { date -u +%Y-%m-%dT%H:%M:%SZ; free -g | sed -n 2p; df -h /home | tail -1; } > "$out/box-$1.txt"; }
die()  { mark "$1" 1; echo "DONE rc=1" >> "$out/markers.txt"; box after 2>/dev/null || true; exit 1; }
on_signal() {
  printf 'DONE rc=%s KILLED-BY=%s %s\n' "$2" "$1" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "$out/markers.txt"
  box after 2>/dev/null || true
  cp "$out/acceptance.log" "$out/acceptance-partial.log" 2>/dev/null || true
  exit "$2"
}
trap 'on_signal SIGTERM 143' TERM
trap 'on_signal SIGINT 130'  INT
trap 'on_signal SIGHUP 129'  HUP

# ── preflight: everything, BEFORE the ~80-minute ingest ────────────────────
[[ -f "$CHAT_GGUF"  ]] || die preflight-chat-gguf
[[ -f "$EMBED_GGUF" ]] || die preflight-embed-gguf
command -v llama-server >/dev/null || die preflight-llama-server
for t in jq python3 curl; do command -v "$t" >/dev/null || die "preflight-tool-$t"; done
[[ -f "$repo/scripts/truth-recall.py" ]] || die preflight-scorer
[[ -f "$repo/sovereign-recipes/wessex-hoard/truth.json" ]] || die preflight-truth
[[ -d "$HOME/.svrnmesh/indexes/wessex-hoard" ]] || die preflight-control-missing
# A leftover from a previous attempt would make the ingest resume, not rebuild.
[[ -e "$HOME/.svrnmesh/indexes/$INGEST_CORPUS" ]] && die preflight-ingest-corpus-exists
mark preflight 0

box before
avail=$(awk '/MemAvailable/{print int($2/1048576)}' /proc/meminfo)
disk=$(df --output=avail -BG /home | tail -1 | tr -dc '0-9')
if [[ -z "${ALLOW_BUSY_BOX:-}" ]] && { (( avail < 40 )) || (( disk < 120 )); }; then
  echo "run.sh: REFUSED — MemAvailable=${avail}G (want >=40 for a 35B chat server), disk=${disk}G" >&2
  die box-before
fi
mark box-before 0

# ── build corpus-mcp from THIS branch ─────────────────────────────────────
if [[ -z "${SKIP_BUILD:-}" ]]; then
  t0=$(date +%s)
  ( cd "$repo" && flock /tmp/sovereign-build.lock \
      cargo build -p corpus-mcp --features corpus-engine/treesitter ) > "$out/build.log" 2>&1
  rc=$?; mark build "$rc"
  printf 'build wall=%ss\n' "$(( $(date +%s) - t0 ))" >> "$out/walls.txt"
  [[ $rc -eq 0 ]] || { tail -25 "$out/build.log" >&2; die build; }
fi
BIN="$repo/target/debug/corpus-mcp"
[[ -x "$BIN" ]] || die binary-missing
# A stale binary would measure main, not this branch — the trap ei-6's probe
# added after learning it the hard way.
[[ -z "$(find "$repo/corpus-mcp/src" "$repo/corpus-mcp/Cargo.toml" -newer "$BIN" 2>/dev/null)" ]] \
  || die binary-stale
mark binary-fresh 0

# ── the run ───────────────────────────────────────────────────────────────
t0=$(date +%s)
(
  export ACCEPT_INGEST=1
  export CHAT_GGUF EMBED_GGUF INGEST_CORPUS
  export CORPUS_MCP="$BIN"
  exec "$repo/corpus-mcp/acceptance.sh"
) > "$out/acceptance.log" 2>&1
rc=$?
mark acceptance "$rc"
printf 'acceptance wall=%ss\n' "$(( $(date +%s) - t0 ))" >> "$out/walls.txt"

# ── the rows, as the script printed them ──────────────────────────────────
# Reported verbatim, both arms, with the scorer named — never re-derived here.
{
  echo "=== truth.json recall, scripts/truth-recall.py (acceptance.sh's own scorer) ==="
  sed -n '/truth.json recall: CONTROL/,/^acceptance: /p' "$out/acceptance.log"
  echo
  sed -n '/truth.json recall: THIS RUN/,/^acceptance: /p' "$out/acceptance.log"
} > "$out/RECALL.txt" 2>/dev/null
grep -aE 'truth.json recall|acceptance: corpus ingest|embed family|embed quirks' \
  "$out/acceptance.log" > "$out/SENTENCES.txt" 2>/dev/null

if [[ $rc -eq 0 ]]; then
  mark VERDICT-ACCEPTANCE-PASSED 0
elif grep -qa 'recall below the daemon-built control' "$out/acceptance.log"; then
  mark VERDICT-RECALL-BELOW-CONTROL 1
elif grep -qa 'COULD-NOT-JUDGE' "$out/acceptance.log"; then
  mark VERDICT-RECALL-COULD-NOT-JUDGE 1
else
  mark VERDICT-ACCEPTANCE-FAILED-ELSEWHERE 1
fi

box after
echo "DONE rc=$rc" >> "$out/markers.txt"
echo "artifacts: $out"
exit "$rc"
