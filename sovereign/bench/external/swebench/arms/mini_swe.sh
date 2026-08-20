#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# The PUBLISHED control arm: mini-swe-agent, the SWE-agent team's
# ~100-line bash-only scaffold. Verified against mini-swe-agent 2.4.6.
#
# This arm is the reason the other numbers mean anything. It is not our
# code, it was not tuned here, and its score is comparable to what the
# field publishes. `native - mini-swe-agent` is our tool contract's
# value; `comaintainer - flat` is the seat protocol's.
#
# It is restricted by --filter to the SAME instance ids the other arms
# run (from instances.jsonl), because an arm graded on a different
# sample is not a control.
#
#   ./mini_swe.sh --model Qwen3-8B-Q4_K_M
#   ./mini_swe.sh --model claude-sonnet-5 --model-class anthropic --workers 4
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(dirname "$HERE")"
MODEL=""
MODEL_CLASS=""
WORKERS=1
BASE_URL="${SOVEREIGN_BASE_URL:-http://localhost:9741/v1}"
EXTRA=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --model)       MODEL="$2"; shift 2 ;;
    --model-class) MODEL_CLASS="$2"; shift 2 ;;
    --workers)     WORKERS="$2"; shift 2 ;;
    --base-url)    BASE_URL="$2"; shift 2 ;;
    *)             EXTRA+=("$1"); shift ;;
  esac
done
[[ -n "$MODEL" ]] || { echo "usage: mini_swe.sh --model <id> [--model-class anthropic] [--workers N]" >&2; exit 2; }

INSTANCES="$ROOT/instances.jsonl"
[[ -f "$INSTANCES" ]] || { echo "no $INSTANCES — run prepare.py first" >&2; exit 1; }

# Same sample as every other arm, expressed as mini's --filter regex.
FILTER="$(python3 -c "
import json,sys
ids=[json.loads(l)['instance_id'] for l in open('$INSTANCES') if l.strip()]
print('^(' + '|'.join(i.replace('.','\\\\.') for i in ids) + ')\$')
")"

OUT="$ROOT/.mini-swe-out"
mkdir -p "$OUT"

ARGS=(--subset verified --split test --output "$OUT" --workers "$WORKERS" --filter "$FILTER")
if [[ -n "$MODEL_CLASS" ]]; then
  ARGS+=(--model-class "$MODEL_CLASS" --model "$MODEL")
else
  # litellm routes `openai/<id>` at an OpenAI-compatible base URL — the
  # daemon. The key is unused but litellm insists one exists.
  export OPENAI_API_BASE="$BASE_URL"
  export OPENAI_BASE_URL="$BASE_URL"
  export OPENAI_API_KEY="${OPENAI_API_KEY:-sovereign-local}"
  ARGS+=(--model "openai/$MODEL")
fi

echo "mini-swe-agent · model=$MODEL · workers=$WORKERS · $(wc -l < "$INSTANCES") instances"
uvx --from mini-swe-agent mini-extra swebench "${ARGS[@]}" "${EXTRA[@]}"

# Normalise mini's preds.json into the per-instance shape collect.py reads,
# so every arm is assembled and audited by the same code path.
python3 - "$OUT" "$ROOT" "$MODEL" <<'PY'
import json, sys
from pathlib import Path
out, root, model = Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3]
src = next((p for p in [out / "preds.json", *out.glob("**/preds.json")] if p.exists()), None)
if src is None:
    raise SystemExit(f"mini-swe-agent wrote no preds.json under {out}")
data = json.loads(src.read_text())
rows = list(data.values()) if isinstance(data, dict) else data
dest = root / "preds" / "mini-swe-agent"
dest.mkdir(parents=True, exist_ok=True)
for r in rows:
    (dest / f"{r['instance_id']}.json").write_text(json.dumps({
        "instance_id": r["instance_id"],
        "model_name_or_path": f"mini-swe-agent:{model}",
        "model_patch": r.get("model_patch", ""),
    }) + "\n")
print(f"normalised {len(rows)} predictions -> {dest}")
PY
