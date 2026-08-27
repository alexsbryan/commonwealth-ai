#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# probe-buffer-threshold.sh — find the prompt size at which llama.cpp's output
# buffer stops fitting in a pinned Vulkan host allocation and silently falls
# back to unreclaimable host memory.
#
# Tests PREREG-buffer-threshold.md. Predicted knee: 4,324 output tokens
# (device cap 0xfffffffc / (n_vocab 248320 * 4 bytes)).
#
# THE LADDER MUST ASCEND. `output_reserve` reallocates only when
# prev_size < new_size (llama-context.cpp:2092), so a large prompt early
# permanently masks every smaller one after it.
set -u
cd /home/alexbryan/dev/commonwealth-ai

MODEL=${MODEL:-Qwen3.5-4B-UD-MTP-Q6_K_XL}
OUT=${1:?usage: probe-buffer-threshold.sh <out.tsv>}
LADDER=${LADDER:-1500 3000 4000 4600 6000 9000}

# RESOLVE BY RESIDENCY, NOT BY FIRST MATCH. `pgrep -f` also matches the toolbox
# wrapper and any stale peer, and the frame's own invariant is that the syslog
# pid is the WRAPPER, not the daemon. Measured 2026-08-27: `| head -1` picked a
# 2 MB process and the whole anon column read 0.000 for every rung, including
# one that had just fallen back to a multi-GiB CPU buffer — a well-formed table
# of nothing (18.3). The daemon that owns a loaded model is the one holding the
# memory, so pick the largest RssAnon and REFUSE if no candidate is resident.
daemon_pid () {
  local best= bestkb=0 kb
  for p in $(pgrep -f 'sovereign-cli-daemon daemon run' 2>/dev/null); do
    kb=$(awk '/^RssAnon/{print $2}' /proc/"$p"/status 2>/dev/null)
    [ -n "$kb" ] || continue
    [ "$kb" -gt "$bestkb" ] && { bestkb=$kb; best=$p; }
  done
  echo "$best"
}

anon_gib () { awk '/^RssAnon/{printf "%.3f", $2/1048576}' /proc/"$1"/status 2>/dev/null; }

# Warm the slot FIRST. Otherwise the first rung's anon step is model load +
# buffer, not buffer, and the cheapest rung is the one we most need clean.
echo "    warming $MODEL (slot load is not a buffer step) ..."
python3 -c "
import json,urllib.request
body=json.dumps({'model':'$MODEL','messages':[{'role':'user','content':'hello'}],'max_tokens':4}).encode()
req=urllib.request.Request('http://127.0.0.1:9741/v1/chat/completions',data=body,headers={'Content-Type':'application/json'})
try: urllib.request.urlopen(req,timeout=900).read()
except Exception as e: print('warmup failed:',e)
"
sleep 3
PID=$(daemon_pid)
[ -n "$PID" ] || { echo "REFUSED: no daemon process found"; exit 2; }
WARMKB=$(awk '/^RssAnon/{print $2}' /proc/"$PID"/status 2>/dev/null)
# A slot-loaded daemon holds GiB. Anything smaller is the wrapper or a stub,
# and measuring it would produce a table that looks fine and means nothing.
[ "${WARMKB:-0}" -gt 1048576 ] || {
  echo "REFUSED: best daemon candidate pid=$PID holds only ${WARMKB:-0} kB anon"
  echo "         after warm-up — that is not a process with a model loaded."
  exit 6; }
echo "    warm. daemon pid $PID, anon now $(anon_gib "$PID") GiB"

printf 'target_words\tprompt_tokens\tanon_before\tanon_after\tanon_step\tpinned_warn\tpredicted_bytes\n' > "$OUT"
echo "=== BUFFER THRESHOLD PROBE — model $MODEL ==="
echo "    predicted knee 4324 prompt tokens (994 kB/token vs 4 GiB cap)"

for W in $LADDER; do
  # One-token-per-word filler keeps prompt_tokens close to $W; the exact count
  # comes back in usage.prompt_tokens and is what we actually key on.
  # A UNIQUE MARKER AT POSITION 0. Without it every rung is a strict prefix of
  # the next, the prefix cache serves the head, and only the SUFFIX is decoded
  # — so `output_reserve` sees a token count that is not the one we tabulated.
  # Measured 2026-08-27: the nested-prefix version produced near-constant anon
  # steps (0.30/0.36/0.32 GiB) that tracked nothing, and one rung that FREED
  # 2.5 GiB. Diverging at token ~5 forces a full prefill every rung.
  PROMPT=$(python3 -c "print('Session marker $W-$$. ' + ' '.join(['apple']*$W))")
  CUR=$(journalctl --user --no-pager -n1 --show-cursor 2>/dev/null | grep -oP '(?<=-- cursor: ).*')
  BEFORE=$(anon_gib "$PID")

  RESP=$(python3 -c "
import json,sys,urllib.request
body=json.dumps({'model':'$MODEL','messages':[{'role':'user','content':open('/dev/stdin').read()}],'max_tokens':16,'temperature':0}).encode()
req=urllib.request.Request('http://127.0.0.1:9741/v1/chat/completions',data=body,headers={'Content-Type':'application/json'})
try:
    print(urllib.request.urlopen(req,timeout=900).read().decode())
except Exception as e:
    print(json.dumps({'error':str(e)}))
" <<<"$PROMPT")

  PT=$(printf '%s' "$RESP" | python3 -c "
import json,sys
try: d=json.load(sys.stdin); print(d.get('usage',{}).get('prompt_tokens') or d.get('error','?'))
except Exception: print('?')")
  sleep 2
  AFTER=$(anon_gib "$PID")
  WARN=$(journalctl --user --no-pager --after-cursor "$CUR" 2>/dev/null \
          | grep -c 'Failed to allocate pinned memory')
  STEP=$(python3 -c "print('%.3f'%($AFTER-$BEFORE))" 2>/dev/null || echo "?")
  PRED=$(python3 -c "
t='$PT'
print(int(t)*993280+32 if t.isdigit() else '?')")
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$W" "$PT" "$BEFORE" "$AFTER" "$STEP" "$WARN" "$PRED" >> "$OUT"
  printf '    words=%-6s tokens=%-7s anon %sGiB -> %sGiB (%s)  pinned_warn=%s  predicted=%s B\n' \
    "$W" "$PT" "$BEFORE" "$AFTER" "$STEP" "$WARN" "$PRED"
done
echo "=== wrote $OUT ==="
