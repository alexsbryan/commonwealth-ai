# SEP retrieval+judge bench

Scoring surface for `referential_atlas` / `philosophy_atlas` on the
Stanford Encyclopedia of Philosophy. Different shape from the
enrichment-eval goldens under `bench/{obsidian,literary,philosophy}/`
— this is a **retrieval + LLM-judge** bench, not atom F1.

## Shape

`questions.toml` is a symlink to
`sovereign/bench/sep/questions.toml`. 21 questions
calibrated for the Harvard undergrad-philosophy-essay bar. Each
question has `expected_facts` (keyword-fuzzy substrings) +
`expected_sources` (canonical SEP article slugs).

The scoring CLI is `sovereign eval run` (`eval_cmd/runner.rs`), not
`sovereign enrich eval`. It returns:
- per-question `fact_recall` (substring matches in retrieved chunks)
- per-question `source_recall` (top-K hit list)
- LLM-judge `essay_readiness` axis (rationale + score 0-3)
- per-category rollups: `position_summary`,
  `argument_reconstruction`, `concept_distinction`, `dialectical`,
  `comparative`, `contested`

## Baselines

| File | What it is |
|---|---|
| `baselines/pre-enrichment-v1_1.json` | No-atlas baseline (bare retrieval; v1.1 questions). |
| `baselines/canonical-57-articles.json` | 57-article enriched corpus, v1 question-bank — canonical baseline reference. |
| `baselines/latest-2026-05-07.json` | Latest enriched run (2026-05-07 post-batch3). |

Synced 2026-05-15 from `commonwealth-ai.pre-monorepo/sovereign/bench/sep_eval/`. Earlier iterations and raw logs (`multi-atlas-{12,15,17,18,23,58}-*.{json,log}`) stayed in the pre-monorepo dir as historical archive — `~80 iteration files / ~25 MB` not worth carrying in the monorepo tree.

## Provenance

- Driver: `sovereign/bench/sep_atlas/run_batch.sh` (per-article parallel ingest across mesh peers).
- Recipe: `sovereign-recipes/sep/recipe.toml`.
- Pipeline: `philosophy_atlas` (per `corpus-engine/src/enrichment/pipeline/pipelines/philosophy_atlas.rs`).
- Historical findings: `sovereign/bench/HISTORY.md` §sep_atlas + memory `project_sep_atlas_phase0.md` + `project_sep_atlas_gemma_ab.md`.

The full 57-article SEP corpus is **mesh-syncable** (`mesh_sharing=false` on the base index per Stanford license, but per-article atlas outputs `sep-<slug>/atlas/` are independent and gossip-able). When fedora is online, `sovereign mesh sync sep-*` pulls every per-slug atlas the peer has built.
