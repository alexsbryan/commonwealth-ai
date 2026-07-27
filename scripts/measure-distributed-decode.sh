#!/usr/bin/env bash
# Wait for the primary slot to report mode=distributed, then measure REAL decode
# tok/s across the iroh tunnel.
#
# Usage:  PEER=<mesh peer name> [MAX_TOKENS=120] [WAIT_SECS=1500] \
#           scripts/measure-distributed-decode.sh
# Output: target/distributed-decode/{distributed-decode.json,frames.jsonl}
#
# Guards, each earned by a specific observed false result (see
# docs/DISTRIBUTED_GDN_CRASH_STATUS.md §8 for the full trap list):
#
#  1. WHICH SLOT SERVED IT. Config is fast=0.8B / primary=4B. A request hijacked
#     to the fast slot would look fast and successful while proving nothing (the
#     0.8B is 100% local). Assert the SSE `model` names the primary.
#  2. REAL PER-FRAME TIMING. curl streams into a python reader that timestamps
#     every frame at arrival, so TTFT (prefill + tunnel setup) separates from the
#     steady-state inter-token rate. Wall-clock/total-tokens smears them.
#  3. PLACEMENT RE-READ AFTER. Quarantine mid-run reverts the slot to local and
#     the tail becomes local decode.
#  4. PEER LIVENESS BEFORE AND AFTER. A 2026-07-27 02:51 run fired into a worker
#     that had stopped gossiping 25s earlier: `reaffirm_plan`'s Rebridge path
#     re-mints a known bridged worker from the LOCAL bridge cache with no probe,
#     so discovery kept logging "discovered mesh RPC worker" for 68s after the
#     peer was gone. Confirm the peer is Online immediately before firing, and
#     again after.
#  5. CANARY FIRST. A tiny non-streaming request proves tokens flow at all before
#     we spend the window on a timed run — and if the worker is going to abort the
#     host, it happens here with a clean attribution instead of inside the timing.
#  6. HOST-ALIVE CHECK. A GGML_ABORT from a bad worker kills this daemon. If the
#     daemon is gone afterwards, say THAT rather than reporting 0.00 tok/s.
#     Implemented by resolving /proc/<pid>/exe — a bare pgrep -f matches bash
#     wrappers whose command line contains the daemon path (§8 trap 1), and a
#     daemon running on a deleted inode after a rebuild must not count either
#     (§8 trap 2).
set -uo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUT_DIR="${OUT_DIR:-$REPO/target/distributed-decode}"
mkdir -p "$OUT_DIR"
CLI="$REPO/target/debug/sovereign-cli"
RESULT="$OUT_DIR/distributed-decode.json"
FRAMES="$OUT_DIR/frames.jsonl"
DEADLINE=$(( $(date +%s) + ${WAIT_SECS:-1500} ))
MAX_TOKENS=${MAX_TOKENS:-120}
PEER=${PEER:?set PEER to the mesh peer name expected to host the remote shard}

placement() {
  curl -s --max-time 5 localhost:9741/status \
    | python3 -c 'import sys,json
try:
    d=json.load(sys.stdin)
    for r in d.get("inference",{}).get("resident",[]):
        if r.get("role")=="primary":
            p=r.get("placement") or {}
            print(p.get("mode","none"), p.get("local_blocks",0), p.get("total_blocks",0), r.get("model_id","?"))
            break
    else: print("none 0 0 ?")
except Exception: print("err 0 0 ?")'
}

peer_status() {  # -> online | offline | unknown
  timeout 20 "$CLI" mesh status 2>/dev/null \
    | awk -v p="$PEER" '$0 ~ p { for(i=1;i<=NF;i++) if($i=="online"||$i=="offline"){print $i; found=1; exit} }
                         END { if(!found) print "unknown" }'
}

# yes only if some process's RESOLVED exe is the daemon binary, on a live inode.
host_alive() {
  for pid in $(pgrep -f 'sovereign-cli-daemon daemon run' 2>/dev/null); do
    exe=$(readlink "/proc/$pid/exe" 2>/dev/null) || continue
    case "$exe" in
      *' (deleted)') continue ;;              # rebuilt under a running daemon (§8 trap 2)
      *sovereign-cli-daemon) echo yes; return ;;
    esac
  done
  echo no
}

echo "[$(date -u +%H:%M:%S)] polling /status for mode=distributed …" >&2
LAST=""
while [ "$(date +%s)" -lt "$DEADLINE" ]; do
  if [ "$(host_alive)" = "no" ]; then
    echo "[$(date -u +%H:%M:%S)] HOST DAEMON GONE while waiting — aborting" >&2
    exit 3
  fi
  read -r M LOCAL TOTAL PRIMARY_ID <<<"$(placement)"
  if [ "$M$LOCAL$TOTAL" != "$LAST" ]; then
    echo "[$(date -u +%H:%M:%S)] placement=$M local=$LOCAL/$TOTAL primary=$PRIMARY_ID" >&2
    LAST="$M$LOCAL$TOTAL"
  fi
  if [ "$M" = "distributed" ]; then
    PEER_BEFORE=$(peer_status)
    echo "[$(date -u +%H:%M:%S)] DISTRIBUTED $LOCAL/$TOTAL local · peer $PEER=$PEER_BEFORE" >&2
    if [ "$PEER_BEFORE" != "online" ]; then
      echo "[$(date -u +%H:%M:%S)] peer is '$PEER_BEFORE' — NOT firing (this is the 02:51 failure)" >&2
      sleep 5; continue
    fi

    # --- guard 5: canary -------------------------------------------------
    echo "[$(date -u +%H:%M:%S)] canary (8 tokens, non-streaming)…" >&2
    CANARY=$(curl -s --max-time 90 localhost:9741/v1/chat/completions \
      -H 'content-type: application/json' \
      -d '{"model":"commonwealth/primary","max_tokens":8,"temperature":0,
           "messages":[{"role":"user","content":"Say hello."}]}' 2>&1)
    CANARY_TOKS=$(printf '%s' "$CANARY" | python3 -c 'import sys,json
try:
    d=json.load(sys.stdin); print((d.get("usage") or {}).get("completion_tokens") or 0)
except Exception: print(0)' 2>/dev/null)
    echo "[$(date -u +%H:%M:%S)] canary completion_tokens=$CANARY_TOKS  host_alive=$(host_alive)" >&2
    if [ "$(host_alive)" = "no" ]; then
      echo "GGML_ABORT: the canary killed the host daemon — the worker crashed or returned malformed response." >&2
      python3 -c "
import json,sys
json.dump({'verdict':'INVALID','problems':['host daemon died during canary — remote RPC worker crashed or malformed (GGML_ABORT)'],
           'placement_before':{'mode':'distributed','local_blocks':int('$LOCAL'),'total_blocks':int('$TOTAL')},
           'peer_before':'$PEER_BEFORE','canary_tokens':$CANARY_TOKS}, open('$RESULT','w'), indent=2)"
      exit 4
    fi
    if [ "${CANARY_TOKS:-0}" -lt 1 ]; then
      echo "[$(date -u +%H:%M:%S)] canary produced NO tokens — not timing a dead path" >&2
      python3 -c "
import json
json.dump({'verdict':'INVALID','problems':['canary produced zero tokens; distributed decode is not producing output'],
           'placement_before':{'mode':'distributed','local_blocks':int('$LOCAL'),'total_blocks':int('$TOTAL')},
           'peer_before':'$PEER_BEFORE','canary_tokens':${CANARY_TOKS:-0}}, open('$RESULT','w'), indent=2)"
      exit 5
    fi

    # --- the measured run ------------------------------------------------
    # Retried: a request fired immediately after the canary has been observed
    # to return an empty/error body (0 SSE frames) while the same request
    # succeeds seconds later. Non-`data:` lines are recorded too ("raw"
    # entries) so an error body is visible in frames.jsonl instead of being
    # silently dropped.
    echo "[$(date -u +%H:%M:%S)] canary OK — firing timed streaming completion" >&2
    # The reader must be a FILE: `curl | python3 - <<'PY'` silently discards the
    # pipe (the heredoc overrides python's stdin, which is where `-` reads the
    # program from), yielding an empty frames.jsonl and a bogus INVALID verdict.
    READER="$OUT_DIR/sse_reader.py"
    cat > "$READER" <<'PY'
import sys, time, json
out = open(sys.argv[1], "w"); t0 = time.monotonic()
for line in sys.stdin:
    line = line.rstrip("\n")
    if not line.strip(): continue
    entry = {"t": time.monotonic() - t0}
    if line.startswith("data:"):
        entry["d"] = line[5:].strip()
    else:
        entry["raw"] = line
    out.write(json.dumps(entry) + "\n"); out.flush()
out.close()
PY
    for attempt in 1 2 3; do
      curl -sN --max-time 300 localhost:9741/v1/chat/completions \
        -H 'content-type: application/json' \
        -d "{\"model\":\"commonwealth/primary\",\"max_tokens\":$MAX_TOKENS,\"temperature\":0,\"stream\":true,
             \"messages\":[{\"role\":\"user\",\"content\":\"Count from 1 to 40, one number per line.\"}]}" \
      | python3 -u "$READER" "$FRAMES"
      if grep -q '"d"' "$FRAMES" 2>/dev/null; then break; fi
      echo "[$(date -u +%H:%M:%S)] attempt $attempt got no SSE data frames ($(wc -l < "$FRAMES") raw lines in $FRAMES) — retrying in 5s" >&2
      sleep 5
    done

    read -r M2 LOCAL2 TOTAL2 _ <<<"$(placement)"
    PEER_AFTER=$(peer_status); ALIVE=$(host_alive)
    echo "[$(date -u +%H:%M:%S)] post-run placement=$M2 local=$LOCAL2/$TOTAL2 · peer=$PEER_AFTER · host_alive=$ALIVE" >&2

    python3 - "$FRAMES" "$LOCAL" "$TOTAL" "$M2" "$PRIMARY_ID" "$RESULT" \
             "$PEER_BEFORE" "$PEER_AFTER" "$ALIVE" "$CANARY_TOKS" <<'PY' >&2
import json, sys
(frames_path, loc, tot, post_mode, primary_id, result_path,
 peer_before, peer_after, alive, canary) = sys.argv[1:11]

rows = []
for line in open(frames_path, errors="replace"):
    try: rows.append(json.loads(line))
    except Exception: pass

served_model, text, finish, stamps = None, [], None, []
for r in rows:
    if "d" not in r:
        print(f"  non-SSE line in stream: {r.get('raw','')[:160]!r}")
        continue
    if r["d"] == "[DONE]": continue
    try: d = json.loads(r["d"])
    except Exception: continue
    served_model = d.get("model") or served_model
    got = False
    for ch in d.get("choices") or []:
        piece = (ch.get("delta") or {}).get("content")
        if piece: text.append(piece); got = True
        if ch.get("finish_reason"): finish = ch["finish_reason"]
    if got: stamps.append(r["t"])

body = "".join(text)
ttft = stamps[0] if stamps else None
span = (stamps[-1] - stamps[0]) if len(stamps) > 1 else 0.0
rate = ((len(stamps) - 1) / span) if span > 0 else 0.0

problems = []
if alive != "yes":
    problems.append("host daemon DIED during the timed run (GGML_ABORT from the worker)")
if served_model is None:
    problems.append("no `model` field in any SSE frame — cannot attribute the run")
elif (served_model not in ("commonwealth/primary", "primary")  # alias IS the primary slot
      and primary_id not in served_model and served_model not in primary_id):
    problems.append(f"WRONG SLOT: served by {served_model!r}, primary is {primary_id!r}")
if post_mode != "distributed":
    problems.append(f"placement fell back to {post_mode!r} — not all timed tokens crossed the boundary")
if peer_after != "online":
    problems.append(f"peer went {peer_after!r} during the run")
if len(stamps) < 10:
    problems.append(f"only {len(stamps)} content frames — too few to call this a decode rate")

verdict = "VALID" if not problems else "INVALID"
out = {"verdict": verdict, "problems": problems,
       "served_model": served_model, "primary_model_id": primary_id,
       "placement_before": {"mode": "distributed", "local_blocks": int(loc), "total_blocks": int(tot)},
       "placement_after": post_mode, "peer_before": peer_before, "peer_after": peer_after,
       "host_alive_after": alive, "canary_tokens": int(canary or 0),
       "content_frames": len(stamps),
       "ttft_s": round(ttft, 3) if ttft is not None else None,
       "decode_span_s": round(span, 3), "decode_tok_s": round(rate, 2),
       "finish_reason": finish, "text_head": body[:200]}
json.dump(out, open(result_path, "w"), indent=2)

print(f"\n=== {verdict} ===")
for p in problems: print("  ! " + p)
print(f"  served_model   = {served_model}   (primary slot = {primary_id})")
print(f"  placement      = distributed {loc}/{tot} local  ->  after: {post_mode}")
print(f"  peer {peer_before} -> {peer_after} · host_alive={alive} · canary_tokens={canary}")
print(f"  content frames = {len(stamps)}   finish={finish}")
print(f"  TTFT           = {ttft if ttft is None else round(ttft,3)} s")
print(f"  DECODE         = {rate:.2f} tok/s over {span:.2f}s")
print(f"  text: {body[:160]!r}")
print(f"\n  full result -> {result_path}")
PY
    exit 0
  fi
  sleep 2
done
echo "TIMEOUT: never observed mode=distributed within the deadline" >&2
exit 1
