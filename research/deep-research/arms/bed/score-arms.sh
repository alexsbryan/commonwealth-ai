#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# score-arms.sh — score a directory of compose-replay arms on the SAME ruler
# the flights use, and print the curve.
#
# Separate from sweep-compose.sh on purpose: the arms are expensive (~10-20
# min each) and the scoring is cheap, so a scoring bug, a dead shell, or a
# judge swap must never cost the compute again. `sweep-compose.sh` delegates
# here rather than carrying a second copy (ARCH 10.6 — one decider, one name).
#
# Idempotent: scores.jsonl is REWRITTEN, not appended, so re-running cannot
# double-count an arm.
set -u
cd /home/alexbryan/dev/commonwealth-ai

DIR=${1:?usage: score-arms.sh <run-dir-with-arm-*.md> [task] [judge]}
TASK=${2:-69}
JUDGE=${3:-Qwen3.8-27B-UD-Q6_K_XL}

ls "$DIR"/arm-*.md >/dev/null 2>&1 || { echo "REFUSED: no arm-*.md in $DIR"; exit 3; }

# The judge is the instrument. score_one.py exits 2 when it is not served, and
# an arm scored by a DIFFERENT model is not comparable to the flights (18.4).
curl -sf --max-time 10 http://127.0.0.1:9741/v1/models \
  | python3 -c "
import json,sys
ids={m['id'] for m in json.load(sys.stdin)['data']}
sys.exit(0 if '$JUDGE' in ids else 1)" \
  || { echo "REFUSED: judge $JUDGE is not served locally"; exit 2; }

SIDE="$DIR/judge-sidecar.jsonl"
: > "$DIR/scores.jsonl"
echo "=== SCORING $(ls "$DIR"/arm-*.md | wc -l) arms — task $TASK, judge $JUDGE ==="
for md in "$DIR"/arm-*.md; do
  name=$(basename "$md" .md); name=${name#arm-}
  # ~9.5 min per article on the pinned 27B, so never re-judge bytes already
  # judged by the same model. The sidecar's sha256 is the key.
  sc=$(python3 research/deep-research/arms/bed/score_cached.py \
        "$SIDE" "$md" "$JUDGE" "$TASK" 2>/dev/null | grep -oE '[0-9]+\.[0-9]+')
  if [ -n "$sc" ]; then
    cached=" (cached)"
  else
    cached=""
    # WAIT OUT A SHED, RETRY NOTHING ELSE — the same distinction port.rs makes.
    # The judge shares one daemon queue with any flight still in the air, and
    # `deep_research_bench/utils/api.py` does not retry: it raises on the first
    # 503. Measured 2026-08-27 scoring an arm while the next arm was drafting:
    #   503 {"error":"host busy: ~30000 ms predicted wait at queue position 1",
    #        "reason":"local_queue_full","retry_after_secs":30}
    # That is transient and honouring it costs one sleep. A non-shed failure is
    # NOT retried — spinning on a deterministic error is the bug this codebase
    # just fixed on the inference side.
    sc=""
    for attempt in 1 2 3 4 5 6; do
      out=$(python3 research/deep-research/arms/lab/score_one.py \
              --task "$TASK" --article "$md" --model "$JUDGE" --save-judge "$SIDE" 2>&1)
      sc=$(printf '%s' "$out" | tail -1 | grep -oE '[0-9]+\.[0-9]+')
      [ -n "$sc" ] && break
      if printf '%s' "$out" | grep -qE 'local_queue_full|host busy|retry_after_secs|503'; then
        wait_s=$(printf '%s' "$out" | grep -oE 'retry_after_secs[^0-9]*[0-9]+' \
                  | grep -oE '[0-9]+' | head -1)
        wait_s=${wait_s:-30}
        echo "    $name: shed (attempt $attempt) — waiting ${wait_s}s"
        sleep "$wait_s"
        continue
      fi
      # Not a shed. Surface the reason instead of retrying into it.
      echo "    $name: judge FAILED (not a shed): $(printf '%s' "$out" | tail -1)"
      break
    done
  fi
  # An arm the judge could not score is COULD-NOT-JUDGE, never a zero (18.1).
  printf '    %-8s %6s words  overall %s%s\n' "$name" "$(wc -w < "$md")" "${sc:-COULD-NOT-JUDGE}" "$cached"
  echo "{\"arm\":\"$name\",\"words\":$(wc -w < "$md"),\"overall\":${sc:-null}}" >> "$DIR/scores.jsonl"
done

echo
echo "=== CURVE ==="
# The merged report carries EVERY arm's timing; the bare one carries only the
# last invocation's. Prefer merged, fall back for single-shot runs.
REPORT="$DIR/arms-merged.json"
[ -f "$REPORT" ] || REPORT="$DIR/compose-replay.json"
python3 research/deep-research/arms/bed/curve.py "$DIR/scores.jsonl" "$REPORT"
