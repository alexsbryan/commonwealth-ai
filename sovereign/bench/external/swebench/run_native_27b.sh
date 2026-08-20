#!/usr/bin/env bash
# One-shot: native arm, 27B, repository scale. Launched via launchd so
# the harness reaper cannot kill a long daemon-bound run mid-flight.
set -euo pipefail
cd /Users/alexsbryan/dev/commonwealth-ai
rm -f sovereign/bench/external/swebench/preds/native/*.json
exec ./target/debug/sovereign-agent-bench swebench \
  --root sovereign/bench/external/swebench \
  --agent native \
  --model Qwen3.8-27B-UD-Q6_K_XL \
  --only "${1:-pylint-dev__pylint-4661}" \
  --wall-cap 2400 --token-cap 300000
