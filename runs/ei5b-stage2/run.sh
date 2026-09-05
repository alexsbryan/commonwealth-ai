#!/usr/bin/env bash
# ei-5b stage 2 — `corpus ingest` over two bare llama-server endpoints on the
# 35B, TWICE, back to back into two new corpus ids.
#
# Twice because one run is not a measurement (ARCH §18.5): the bar is
# truth.json recall against the daemon-built `wessex-hoard`, and a single
# reading cannot say whether a miss is the endpoint or the sampling. The two
# runs are identical except for the corpus id.
#
# Staged for the run channel because a 20-chapter enrichment against a
# llama-server exceeds the 25-minute in-session ceiling. The seat launches it.
#
# Env the CALLER sets — an absent one is refused, never guessed (ARCH §18.3):
#   CHAT_GGUF    the chat model .gguf (stage 2: Qwen3.6-35B-A3B-MTP-UD-Q6_K.gguf)
#   EMBED_GGUF   the embedding .gguf  (Qwen3-Embedding-0.6B-Q8_0.gguf, dim 1024)
# Optional: RUN_IDS (default "wessex-hoard-bare-35b-a wessex-hoard-bare-35b-b").
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
out="$here/$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$out"

mark() { printf '%s rc=%s %s\n' "$1" "$2" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "$out/markers.txt"; }
box() { { date -u +%Y-%m-%dT%H:%M:%SZ; free -g | sed -n 2p; df -h /home | tail -1;
          echo "builds: $(pgrep -af 'cargo|rustc' | grep -v lspmux | grep -vc pgrep)"; } > "$out/box-$1.txt"; }

: "${CHAT_GGUF:?run.sh: CHAT_GGUF is required — name the chat model to ingest with}"
: "${EMBED_GGUF:?run.sh: EMBED_GGUF is required — name the embedding model}"
[[ -f "$CHAT_GGUF"  ]] || { mark preflight-env 1; echo "DONE rc=1" >> "$out/markers.txt"; exit 1; }
[[ -f "$EMBED_GGUF" ]] || { mark preflight-env 1; echo "DONE rc=1" >> "$out/markers.txt"; exit 1; }
mark preflight-env 0

RUN_IDS="${RUN_IDS:-wessex-hoard-bare-35b-a wessex-hoard-bare-35b-b}"
for id in $RUN_IDS; do
  [[ "$id" != "wessex-hoard" ]] || { mark control-guard 1; echo "DONE rc=1" >> "$out/markers.txt"; exit 1; }
done
mark control-guard 0

# The box conditions the campaign requires before a lane. RECORDED *and*
# ENFORCED. The first launch of this script (2026-09-05T08:26:39Z) wrote
# `builds: 5` into box-before.txt and then ran anyway, colliding with a
# sibling's gate sweep and costing the run. A check that observes and does not
# stop is not a check (ARCH §18.1) — so this refuses, and names both measured
# values rather than saying "busy". ALLOW_BUSY_BOX=1 is the deliberate
# override, the shape `--allow-empty` has on the test script: an operator who
# means it is not blocked, and nobody drifts past it by accident.
MEM_FLOOR_GB="${MEM_FLOOR_GB:-40}"
box before
builds_now=$(pgrep -af 'cargo|rustc' | grep -v lspmux | grep -vc pgrep)
avail_now=$(awk '/MemAvailable/{print int($2/1048576)}' /proc/meminfo)
if [[ -z "${ALLOW_BUSY_BOX:-}" ]] && { (( builds_now != 0 )) || (( avail_now < MEM_FLOOR_GB )); }; then
  echo "run.sh: REFUSED — builds=$builds_now (want 0), MemAvailable=${avail_now}G (want >= ${MEM_FLOOR_GB}G)." \
       "A 35B ingest beside a cargo sweep reaches this box's ceiling, the wall would not mean what it says," \
       "and the daemon is the preferred OOM victim. Override with ALLOW_BUSY_BOX=1 if you mean it." >&2
  mark box-before 1
  echo "DONE rc=1" >> "$out/markers.txt"
  exit 1
fi
mark box-before 0

cd "$repo"
worst=0
n=0
for id in $RUN_IDS; do
  n=$((n+1))
  t0=$(date +%s)
  ACCEPT_INGEST=1 INGEST_CORPUS="$id" CHAT_GGUF="$CHAT_GGUF" EMBED_GGUF="$EMBED_GGUF" \
    "$repo/corpus-mcp/acceptance.sh" > "$out/acceptance-$n.log" 2>&1
  rc=$?
  mark "acceptance-$n($id)" "$rc"
  printf '%s wall=%ss\n' "$id" "$(( $(date +%s) - t0 ))" >> "$out/walls.txt"
  (( rc > worst )) && worst=$rc

  # Per-leg outcomes read back out of the script's own assertion lines rather
  # than re-derived here — one decider for what each leg said (ARCH §10.6).
  for leg in \
    'frontend up on:embed-server' \
    'chat frontend up on:chat-server' \
    'corpus ingest(.*) -> ok in:ingest' \
    'GLiNER is NOT linked:degradation-named' \
    'truth.json recall:recall' \
    'ask(.*) ->:ask' \
    'cargo tree -p corpus-mcp:dep-tree' ; do
    pat="${leg%%:*}"; name="${leg##*:}"
    if grep -qE "$pat" "$out/acceptance-$n.log"; then mark "run$n-leg-$name" 0; else mark "run$n-leg-$name" 1; fi
  done
done

grep -hE 'ingested and enriched in|ok in [0-9]+s|COULD-NOT-JUDGE|acceptance: (PASS|FAIL)|^  [a-z_ ]+ +[0-9]+ / [0-9]+' \
  "$out"/acceptance-*.log > "$out/verdicts.txt" 2>/dev/null
box after
echo "DONE rc=$worst" >> "$out/markers.txt"
exit "$worst"
