# Wikipedia retrieval+judge bench

Scoring surface for `referential_atlas` on the Wikipedia corpus.
Same shape as `bench/sep/` — retrieval + LLM-judge, not atom F1.

## Shape

`questions.toml` is a symlink to
`sovereign-recipes/wikipedia/eval/wikipedia_questions.toml`. 20
challenging questions across six categories: factual recall,
multi-article synthesis, causal / historical, comparative,
boundary / coverage, contested / atlas-relevant.

Scored via `sovereign eval run`. Returns:
- per-question `fact_recall` (keyword-fuzzy substrings in retrieved
  chunks or synthesised answer)
- per-question `source_recall` (Wikipedia article titles in top-K)
- per-category rollups

## Baselines

| File | What it is |
|---|---|
| `baselines/pre-enrichment-2026-04-29.json` | Bare retrieval against the unenriched index. |
| `baselines/post-sync-2026-04-29.json` | After parallel-shard index sync (no atlas yet). |
| `baselines/latest-synth-v16-2026-04-30.json` | Latest synth-mode run with proper OICP profile (v16). |

Synced 2026-05-15 from `commonwealth-ai.pre-monorepo/sovereign-recipes/wikipedia/eval/runs/`. ~20 iteration files (`synth_v{10..15}_*.json`, `synth_kq_redesign_v{1..3}_*.json`, etc.) stayed in pre-monorepo as historical archive.

## Provenance

- Recipe: `sovereign-recipes/wikipedia/recipe.toml`.
- Pipeline: `referential_atlas` (per `corpus-engine/src/enrichment/pipeline/pipelines/referential_atlas.rs`).
- Atlas state: `~/.sovereign/indexes/wikipedia/atlas/` (atoms.json + edges.json present locally).
- Bonus banks: `sovereign-recipes/wikipedia/eval/{single_atomic,single_roman}.toml` — single-article ablation banks; not yet wired into the rollup.
