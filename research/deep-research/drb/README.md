# DRB — the frozen external holdout (order deep-research-t2b)

The DeepResearch Bench subset used for T2's P2 between-arm measurement and
P1's named cost proxy (PLAN.md §4 T2; pre-registration.md "T2b").

## Freeze discipline (frozen at pre-registration, never edited after)

- `query.subset.jsonl` — the 10 frozen English tasks. **Never edited.**
- `query.full.jsonl` — the full 100-task prompt set (the subset's population
  and the audit trail for the selection).
- `leaderboard.csv` — the official leaderboard (vendored verbatim from the
  benchmark space's data dir). The P2 references are read from here.
- `paper-fact-definition.md` — the paper's Appendix E definition + the named
  implementation note (vendored stat.py pooled definition).
- `p1-cost-reference.md` — the P1 proxy arithmetic and citations.
- `vendor/utils/` — the official FACT pipeline code, vendored verbatim
  (extract / deduplicate / validate / stat / api / io_utils / json_extractor
  / score_calculator / clean_article / generate_criteria / scrape).
- `vendor/fixture-validated.jsonl` — the official claude-3-7-sonnet-latest
  validated output (the stat.py reproduction fixture).
- `SHA256SUMS` — hashes of every frozen file. `verify-demo9.sh` re-checks
  them at landing.

Any later edit to a frozen file is a NAMED amendment (§18.6), recorded in
pre-registration.md with a reason, never silent.

## Selection (content-blind, reproducible)

```
python3 select-subset.py
```

population = the 50 English tasks; seed string
"deep-research-t2b-drb-subset-2026-08-17" ->
seed = int(sha256(seed_string)[:8], 16) = 556953489;
rng = random.Random(seed); subset = sorted(rng.sample(en_ids, 10)).
Result: ids [56, 58, 59, 62, 65, 69, 78, 83, 90, 95].

## Provenance

- Benchmark repo: https://github.com/Ayanami0730/deep_research_bench
  commit 469cce54ea7f6a63c163d3d9fec879cf289ec484 (2026-05-11).
  Cloned for local vendoring (read-only inspection; the benchmark's data was
  never uploaded anywhere).
- Leaderboard CSV: https://huggingface.co/spaces/muset-ai/DeepResearch-Bench-Leaderboard/raw/main/data/leaderboard.csv
  (fetched 2026-08-16).
- Paper: arXiv 2506.11763 v2 (ar5iv), Appendix E.

## Layout (non-frozen, produced at run time)

- `runs/{local,hybrid}/` — the flight artifacts (one dir per task).
- `drb-score.py` — the scorer (extract <- verdict-set, reference <- evidence
  windows, judge <- vendored validate en prompt on daemon :9741, stat <-
  vendored pooled definition, cluster bootstrap CI).
- `run-drb-arms.sh` — the run driver.

## The judge pin (DEFAULT LOCAL)

FACT judging runs on the local daemon :9741 with the daemon's pinned
deep-research draft model (Qwen3.6-35B-A3B-MTP-UD-Q6_K) through the vendored
openai-compat client (LLM_BACKEND=openai, OPENAI_BASE_URL=http://127.0.0.1:9741/v1,
FACT_MODEL=<pin>). The external-frontier-judge fallback is the NAMED
FALLBACK (§18.3): it fires only if FACT cannot run locally, and is then named
in the execution record. See pre-registration.md "T2b".
