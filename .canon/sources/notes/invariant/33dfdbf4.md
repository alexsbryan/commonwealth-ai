# sovereign eval run retrieval-limit is the load-bearing baseline-comparability dial; bench all defaults to 30 to match synced baselines**

**`sovereign eval run` retrieval-limit is the load-bearing baseline-comparability dial; bench all defaults to 30 to match synced baselines**

`sovereign eval run --limit <N>` caps top-K at `N`; `run_question` ends with `all_hits.truncate(limit)`. Source_recall directly depends on N — more retrieved chunks = more chances for canonical expected_sources to land in the bag.

Why this matters: the first full `bench all` run on 2026-05-15 reported -9pt SEP and -10pt Wikipedia source_recall regressions. Root cause was apples-to-oranges:

- Synced pre-monorepo baselines were captured with variable per-question chunk counts (14-30 for wiki, 21-30 for SEP).
- New `bench all` subprocess invoked `sovereign eval run` without `--limit`, picked up the CLI default (10).
- Same questions, same index, smaller bag → fewer canonical sources matched → false regression signal.

After standardizing `bench all --retrieval-limit 30`, both corpora moved ABOVE baseline (SEP 0.62→0.84, Wiki 0.57→0.78). The ranker / index / atlas-tier are healthy; the original regression was an instrumentation artifact.

How to apply:
- `bench all` defaults to `--retrieval-limit 30`. Don't override below 30 unless you're stress-testing precision-at-low-K specifically.
- When establishing a new bench baseline, record the limit alongside it. Future comparisons must match.
- If a fresh `bench all` run shows a source_recall delta, FIRST verify limit matches the baseline. THEN look at ranker / index / atlas-tier changes.

Where this lives:
- `sovereign-cli/src/bench_cmd/all.rs::Opts.retrieval_limit` — default 30.
- `sovereign-cli/src/eval_cmd/runner.rs::run_question` — `all_hits.truncate(limit)` is the load-bearing site.
- `sovereign-cli/src/eval_cmd/mod.rs::RunArgs::default()` — CLI default still 10 (interactive use; one-shot exploration). DO NOT bump the CLI default to 30; it would silently change every other consumer of `eval run`.

Pairs with [[feedback-bench-all-workflow]].


## Index overflow (moved from MEMORY.md 2026-07-07 compaction)

- [Retrieval limit must match baseline (2026-05-15)](invariant_retrieval_limit_must_match_baseline.md) — `bench all` defaults `--retrieval-limit 30` to match synced pre-monorepo baselines. First bench-all run reported -9pt SEP / -10pt wiki source_recall, all instrumentation: CLI default limit=10 vs baselines captured at 14-30 chunks/q. After fix: SEP 0.62→0.84, wiki 0.57→0.78 (above baseline).

---
