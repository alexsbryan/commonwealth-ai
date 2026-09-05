#!/usr/bin/env bash
# ei-7c A/B arm 0, ATLAS-BEARING mode. `--prod-pipeline` drives the production
# KnowledgeQuery pipeline in-process, whose step 8 is `atlas_grounding`
# (retrieval_pipeline.rs:49). The default retrieval mode does NOT consult the
# atlas ("atlas content is not part of retrieval unless explicitly opted in",
# eval_cmd/mod.rs:157) — so it cannot tell the three arms apart and is kept
# only as a negative control.
set -uo pipefail
WT=/home/alexbryan/dev/ei7c-wt
OUT=$WT/runs/ab-wikipedia
cd "$WT"
export RUST_LOG=corpus_engine::enrichment::atlas::provider=info,warn
for i in 1 2; do
  echo "=== arm0-prod run $i ===" >&2
  ./target/debug/sovereign-cli eval run \
    --bank sovereign/bench/wikipedia/questions.toml \
    --prod-pipeline --isolate \
    --format json --output "$OUT/arm0prod-run$i.json" \
    > "$OUT/arm0prod-run$i.txt" 2> "$OUT/arm0prod-run$i.err"
  echo "$?" > "$OUT/arm0prod-run$i.rc"
done
date -Is > "$OUT/ARM0PROD_DONE"
