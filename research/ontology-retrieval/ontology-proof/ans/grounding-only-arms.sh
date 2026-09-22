#!/usr/bin/env bash
# The reviewer's run (2026-09-22): SOVEREIGN_ATLAS_GROUNDING=1 with
# SOVEREIGN_ATOM_ENUM=0 — PRODUCTION'S shipped default combination — on the
# ANS bank, 3 runs, pod models. Answers: does the decline behaviour the
# board attributed to `full` ship to every atlas corpus in production today,
# or does it come from the experimental atom-enum? A diagnostic; the
# pre-reg's five frozen arms are untouched (direct eval invocations, no
# arms.toml change).
set -uo pipefail
cd /Users/alexsbryan/dev/commonwealth-ai
eval "$(scripts/dev-pod.sh env)"
OUT=research/ontology-retrieval/ontology-proof/ans/runs-grounding-only
mkdir -p "$OUT"
for r in 1 2 3; do
  [ -f "$OUT/run-$r/eval.json" ] && { echo "run $r on disk"; continue; }
  echo "== grounding-only run $r  [$(date -u +%T)]"
  mkdir -p "$OUT/run-$r"
  SOVEREIGN_ATLAS_GROUNDING=1 SOVEREIGN_ATOM_ENUM=0 SOVEREIGN_ATOM_ENUM_OVERVIEW=0 \
    sovereign eval run \
    --chat-model commonwealth/primary \
    --bank research/ontology-retrieval/ontology-proof/ans/bank.attested.toml \
    --synth --isolate --format json \
    --output "$OUT/run-$r/eval.json" > "$OUT/run-$r/stdout.log" 2> "$OUT/run-$r/stderr.log" \
    || echo "run $r exited $?"
  cat > "$OUT/run-$r/manifest.json" <<EOF
{
  "arm": "grounding-only",
  "diagnostic": true,
  "env": {"SOVEREIGN_ATLAS_GROUNDING": "1", "SOVEREIGN_ATOM_ENUM": "0", "SOVEREIGN_ATOM_ENUM_OVERVIEW": "0"},
  "production_default": true,
  "chat_model": "commonwealth/primary (pod resolves to the board Q6_K)",
  "run": $r,
  "started_utc": "$(date -u +%FT%TZ)"
}
EOF
done
echo "ARMS DONE  [$(date -u +%T)]"
