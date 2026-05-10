# Recipes

This directory holds the reference recipe TOML files bundled with `corpus-engine`. Each recipe describes one corpus end-to-end (`acquire → extract → chunk → index → enrich`) and is also mirrored at compile time via `registry_snapshot.toml` so the engine works fully offline. The same files live (and update independently) in the [`sovereign-recipes`](../../sovereign-recipes) repository; the registry's `toml_url` field points back at the live copy.

For pipeline mechanics, see the parent [`README.md`](../README.md). For the v2 enrichment plan, see [`../ENRICHMENT_V2.md`](../ENRICHMENT_V2.md).

## Production-readiness tiers

| Tier | Meaning |
|---|---|
| **A — Stable** | End-to-end validated (recent ingest run, dedicated extractor with fixtures, a published `TEST_REPORT.md` or pinned A/B notes). Safe to recommend to a first-time user. |
| **B — Available** | Works, but rarely exercised at full scale (large source, less recent validation). Smoke-tested via the same extractor used by a Tier A recipe. |
| **C — On-demand only** | Templated recipe with `on_demand = true`. The id is overridden at runtime by `CatalogIngestService`; direct ingest is refused by the guard in `engine/ingest.rs`. Never installed on its own. |
| **D — Beta** | Acquire/extract path works, but enrichment is wired to a domain that is currently `todo!()`. Recipe ships with `[enrichment] enabled = false` so ingestion succeeds; flipping enrichment on will panic until the prompt set lands. |
| **E — Mutable transport** | New shape: rides the corpus rails as a sync transport for mutable text (memory, plans). Opts into `mutable_merge = "source_doc_id_newest_mtime"` so two daemons editing the same logical file converge on the newer copy after a mesh tick. Pairs with a daemon-side post-merge projector that materializes chunks back to disk. Distinct from A–D because the corpus is never browsed by a human; it is replication infrastructure. |

## Catalog

| ID | Tier | Kind | Source | License | Mesh share | Enrichment |
|---|---|---|---|---|---|---|
| [`wikipedia`](wikipedia/recipe.toml) | A | knowledge | `bulk_download` HF structured zip → `wikipedia_jsonl` | CC-BY-SA-4.0 | yes | off (Layer 1, time-to-grounded) |
| [`wikipedia-simple`](wikipedia-simple/recipe.toml) | A | knowledge | HF parquet → `wikipedia_structured` | CC-BY-SA-4.0 | yes | off (Layer 0 fast grounding) |
| [`sep`](sep/recipe.toml) | A | knowledge | HF parquet → `parquet` | Stanford educational/research | **no** | atlas v2 (`philosophy_atlas`) |
| [`gutenberg`](gutenberg/recipe.toml) | A | catalog | gutenberg.org gz CSV → `gutenberg_catalog` | Public Domain | yes | off |
| [`crs_reports`](crs_reports/recipe.toml) | A | knowledge | HF parquet (`launch/gov_report`) → `parquet` | Public Domain | yes | off |
| [`wikipedia-catalog`](wikipedia-catalog/recipe.toml) | B | catalog | HF gz JSONL → `wikipedia_catalog` | CC-BY-SA-4.0 | yes | off |
| [`openalex`](openalex/recipe.toml) | B | knowledge | HF parquet (`open-index/open-alex`) → `parquet` w/ inverted-index transform | CC0-1.0 | yes | off |
| [`stackexchange`](stackexchange/recipe.toml) | B | knowledge | HF parquet (`common-pile/stackexchange`) → `parquet` | CC-BY-SA-4.0 | yes | off |
| [`stackexchange-knowledge`](stackexchange-knowledge/recipe.toml) | D | knowledge | archive.org `.7z` × 5 → `stackexchange_xml` (`question_with_answers`) | CC-BY-SA-4.0 | yes | off — `engineering` domain is a stub |
| [`wikipedia-article`](wikipedia-article/recipe.toml) | C | knowledge (on_demand) | MediaWiki Action API → `wikipedia_api_article` | CC-BY-SA-4.0 | yes | atlas (encyclopedic) |
| [`gutenberg-work`](gutenberg-work/recipe.toml) | C | knowledge (on_demand) | gutenberg.org plaintext → `plaintext` | Public Domain | yes | field model (literary) |
| [`alignment`](alignment/recipe.toml) | E | knowledge (mutable transport) | `~/.claude/` walk → `alignment_workspace` | private | yes (own peers only) | off |

## Per-recipe notes

### Tier A — Stable

**`wikipedia`** — Default scope is the curator-pace **Vital Articles Level 5** (~51K mainspace articles), enforced via `[[filter]] type = "title_list"` and a build-time bundled list (`@bundled:vital_articles_l5`). Indexes in 5–8 min on M-series. The full 6.7M dump is one `expand_corpus` away. Delta updates via the monthly revision manifest at `updates.sovereign.dev/manifests/wikipedia-en.json`.

**`wikipedia-simple`** — Layer 0 of the layered Wikipedia stack: ~230K plain-language articles, ready for chat in 2–3 min so grounded responses are available while `wikipedia` finishes indexing. Distinct `corpus_id` from `wikipedia` so they never share an embedding space.

**`sep`** — Stanford Encyclopedia of Philosophy (~1,800+ entries). The only Tier A recipe with enrichment **on** by default — `pipeline = "philosophy_atlas"` is fully implemented (`src/enrichment/pipeline/pipelines/philosophy_atlas.rs:71`) and the philosophy domain has zero `todo!()` (`src/enrichment/domains/philosophy.rs`). Per-article scaffolding via `sovereign enrich sep-ingest <slug>`. **License does not permit redistribution** — `mesh_sharing = false`. Has the only `TEST_REPORT.md` in this directory ([`sep/TEST_REPORT.md`](sep/TEST_REPORT.md), 100% extraction rate on 100-record sample, 2026-04-08).

**`gutenberg`** — Catalog-only corpus of 70K+ public-domain works (~50 MB indexed). The catalog *knows of* every work; reading any one happens via the on-demand `gutenberg-work` recipe wired through `[catalog] content_recipe`.

**`crs_reports`** — Government Reports (CRS + GAO) from the `launch/gov_report` HF dataset. Plain parquet path, ~19.5K reports.

### Tier B — Available

**`wikipedia-catalog`** — Catalog of ~6.8M English Wikipedia articles (titles + abstracts + section anchors only). When a query lands on a catalog hit, retrieval surfaces "I haven't read this yet — want me to fetch it?" and on-demand single-article ingest fires via `wikipedia-article`. `target_corpus_id = "wikipedia-fetched"` keeps every fetched article in one shared corpus rather than spawning per-article indexes. One-hop minesweeper expansion (`expansion_link_cap = 20`) eagerly queues outgoing links. The catalog JSONL is produced offline by `sovereign-recipes/wikipedia-catalog/scripts/build_catalog.py`. Tier B because this catalog-pivot pattern is newer than the Tier A recipes.

**`openalex`** — 330 GB scholarly metadata snapshot from the `open-index/open-alex` HF mirror. Uses `parquet` extractor with `content_transform = "openalex_inverted_index"` to reconstruct abstracts. Tier B because few users run a 330 GB ingest on commodity hardware; the pipeline itself is identical to the Tier A `parquet` path.

**`stackexchange`** — Single-canonical-answer Q&A from `common-pile/stackexchange` (~39 GB compressed, ~120 GB indexed). Pre-composed Q+A per parquet row, so per-answer score thresholds can only be approximated at the document level — see the comment block in `stackexchange/recipe.toml:20-27` for the planned migration to `stackexchange_xml` once `.7z` extraction matures further.

### Tier C — On-demand only

These recipes carry `on_demand = true` and are refused by direct ingest. They are invoked exclusively by `CatalogIngestService` after a catalog hit; the templated `[corpus] id` and `[acquire] url` are overridden at runtime via `CorpusSpec::Inline`.

**`wikipedia-article`** — Single-article fetch via the MediaWiki Action API (`api.php?action=parse`). `wikipedia_api_article` extractor produces section-level chunks with full `WikipediaChunkMetadata` — indistinguishable from bulk-dump output, so atlas grounding and link-graph expansion work identically. `parent_corpus_id = "wikipedia"` so fetched articles surface as part of the user's existing Wikipedia corpus rather than a flood of one-off corpora.

**`gutenberg-work`** — Single-work plaintext fetch from `gutenberg.org/cache/epub/{id}/pg{id}.txt`. Sentence-level chunking keeps verse stanzas intact while slicing long chapters at clean boundaries. `domain = "literary"` resolves to the `literary` pipeline (`src/enrichment/pipeline/pipelines/literary.rs`).

### Tier D — Beta

**`stackexchange-knowledge`** — Multi-answer trade-off threads from five charter-knowledge SE sites (Software Engineering, DBA, Security, DevOps, Skeptics) via `archive.org` `.7z` archives + `stackexchange_xml` in `question_with_answers` mode. Acquire/extract/chunk path is solid. **Beta because** `[enrichment] domain = "engineering"` is a `todo!()` stub today (`src/enrichment/domains/engineering.rs:26-36`); the recipe ships with `enabled = false` so ingest succeeds, but flipping it on will panic. Promote to Tier A once the engineering prompt set lands.

## Domain & pipeline status (drives Tier D classification)

Enrichment dispatch is by string id. The `Domain` (legacy field-model flow) and `Pipeline` (atlas v2) registries are independent.

| Domain (`enrichment.type = "field_model"`) | Status | Used by |
|---|---|---|
| `philosophy` | implemented | (legacy SEP) |
| `literary` | implemented (via pipeline `literary`) | `gutenberg-work` |
| `engineering` | **stub** (`src/enrichment/domains/engineering.rs:26+`) | `stackexchange-knowledge` |
| `multi`, `science`, `policy`, `legal`, `community` | **stubs** | none currently |

| Pipeline (`enrichment.type = "atlas"`) | ID | Used by |
|---|---|---|
| `PhilosophyAtlasPipeline` | `philosophy_atlas` | `sep` |
| `LiteraryAtlasPipeline` | `literary_atlas` | (planned) |
| `ReferentialAtlasPipeline` | `referential_atlas` | (planned; encyclopedic recipes) |
| `LiteraryPipeline` | `literary` (legacy) | `gutenberg-work` |

Built-in pipelines are registered in `src/enrichment/pipeline/registry.rs:26-40`.

## Companion / parent relationships

```
gutenberg (catalog)        ──content_recipe──▶  gutenberg-work (on_demand)
wikipedia-catalog (catalog) ──content_recipe──▶  wikipedia-article (on_demand)
                                                         │
                                                         └─ parent_corpus_id ──▶ wikipedia
stackexchange ◀────────── pair (non-overlapping by intent) ──────────▶ stackexchange-knowledge
wikipedia-simple (Layer 0) ◀──── layered fallback ────▶ wikipedia (Layer 1)
```

## Adding a new recipe

1. Drop a `recipe.toml` into a new subdirectory here.
2. Run `sovereign recipe validate path/recipe.toml --offline` (field-level checks, no download).
3. Run `sovereign recipe test path/recipe.toml --sample-size 50 --no-embed` and commit the resulting `TEST_REPORT.md` next to the recipe.
4. Refresh the bundled snapshot: `cargo xtask update-registry-snapshot`.
5. If the recipe needs a domain or pipeline that doesn't exist yet, ship it Tier D — `[enrichment] enabled = false` — until the implementation lands.
