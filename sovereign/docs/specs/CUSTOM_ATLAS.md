# Recipe-Driven Custom Atlas — author your domain's ontology, feed it to chat

Status: **in progress** (2026-06-16). Owner: recipe-author UX.

## Problem

A domain expert builds a corpus to make chat *smarter about their domain*. Today the
enrichment that actually feeds chat is fixed:

- **Atlas atoms feed chat.** `runtime/evidence_loop.rs` reads
  `indexes/<id>/atlas/atoms.json` — that's the grounding the model retrieves against.
- **But atlas pipelines are code-defined.** `literary_atlas`, `philosophy_atlas`,
  `conversation_atlas`, `engineering_atlas`, `referential_atlas` — you *pick* one; you can't
  author a new one without writing Rust.
- **The custom-schema path (investigation) doesn't feed chat.** `type="investigation"`
  lets you declare your own `entity_types`/`relationship_types`/`patterns`, but its output
  (`entities.json`/`relationships.json`/`findings.json`) is consumed by CLI + benches, not
  the chat evidence loop.

So: **custom-typed ontology (investigation) ≠ chat-feeding; chat-feeding (atlas) ≠
customizable.** Closing that gap is the tool's core purpose.

## Key findings (why this is mostly reuse, not a new system)

1. **The atom ontology is already universal.** Atlas atoms are `entities / entity-states /
   relations / relation-states / events / claims / questions`
   (`pipeline/atlas.rs::SectionExtraction`). `literary_atlas` vs `philosophy_atlas` differ
   almost entirely in their **Phase-1 extraction prompt**, not their atom types.
2. **Entity/relation/etc. types are OPEN enums** (`string_enum_with_other!` →
   `EntityType::Other(String)`, same for relations/events/claims). A custom domain can
   introduce its own type labels (`"coin"`, `"hoard"`) with zero engine changes.
3. **Pipelines compose by delegation.** `LiteraryAtlasPipeline { inner: LiteraryPipeline }`
   customizes Phase 1 and delegates Phases 3–7 to `inner`
   (`pipelines/literary_atlas.rs`). `render_phase1_user_body` (`pub(super)`) and
   `phase1_section_extraction_schema` (`pub`) are reusable from a sibling module.
4. **Prompts are already data.** `phaseN_system()` returns `&'static str`, loaded via
   `prompts::load_or_baked(asset, baked)` which `Box::leak`s a disk override when
   `$SOVEREIGN_PROMPT_DIR` is set. Leaking a per-build prompt once is an established pattern.

## Design

A **`ConfigurableAtlasPipeline`** — same trait, same machinery, but its Phase-1 system
prompt and vocabulary come from the **recipe**, not `include_str!`.

```
recipe.toml  ──enrich init──▶  EnrichConfig (config.json)  ──enrich build──▶  atoms.json ──▶ chat
[enrichment]                    pipeline_id="custom_atlas"   resolve_pipeline()   (evidence_loop)
type="atlas"                    ontology: { guidance, … }    └▶ ConfigurableAtlasPipeline::from_spec
[enrichment.ontology]                                           { inner: LiteraryAtlasPipeline,
guidance = "…domain focus…"                                       phase1_system: leaked(base + guidance),
                                                                  vocabulary: from recipe }
```

- **`ConfigurableAtlasPipeline`** (`pipeline/pipelines/configurable_atlas.rs`):
  - `phase1_system()` → leaked `format!("{base}\n\n## Domain focus\n\n{guidance}")` where
    `base = inner.phase1_system()` (the universal extraction instructions). Domain guidance
    is **additive** — the universal atom schema/anchoring is preserved.
  - `compose_phase1()` → mirrors `literary_atlas` (calls `render_phase1_user_body` +
    `phase1_section_extraction_schema`) but with the custom system prompt.
  - `parse_phase1()` and Phases 3–7 + clustering → delegate to `inner` (atom schema +
    downstream are universal). `vocabulary()` → owned, from recipe (defaults to generic).
  - `id()` = `"custom_atlas"`, `name()` from the recipe's `domain` label.

- **SSOT pipeline resolution.** ~10 sites in `sovereign-cli-llm/src/enrich_cmd/*` call
  `PipelineRegistry::builtin().get(&cfg.pipeline_id)` (build, extract×2, seed, cascade,
  phase_cmd, atlas_phase_cmd, atlas_tensions_classify, atlas_resolve, atlas_configuration).
  Introduce one helper `resolve_pipeline(&EnrichConfig) -> Option<Arc<dyn Pipeline>>` that
  branches: `cfg.ontology.is_some()` → `ConfigurableAtlasPipeline::from_spec(...)`; else
  `registry.get(&cfg.pipeline_id)`. **Every** site calls it — no per-command divergence.

- **Recipe is SSOT.** `enrich init` reads `[enrichment.ontology]` and materializes it into
  `EnrichConfig.ontology` (+ `pipeline_id="custom_atlas"`). `enrich build` reads the config.

### Recipe schema (`[enrichment.ontology]`)

```toml
[enrichment]
enabled = true
type    = "atlas"
domain  = "medieval-numismatics"   # short label → pipeline name

[enrichment.ontology]
# Appended to the universal atom-extraction instructions. Tell the model what
# entities / relations / claims / events matter in THIS domain, in its language.
guidance = """
Medieval numismatic scholarship. Extract:
- Entities: coins (mint, ruler, denomination, metal), mints, rulers, hoards, scholars.
- Relations: minted_by, found_in_hoard, succeeds_ruler, attributed_by.
- Claims: attributions, datings, metrological/stylistic arguments.
- Events: hoard discoveries, reforms.
Prefer canonical names ("Offa of Mercia", not "the king").
"""

[enrichment.ontology.vocabulary]   # optional; sensible generic defaults
concern_term = "question"
position_term = "interpretation"
tension_term = "disagreement"
absence_term = "open question"
evidence_term = "passage"
```

Precedence when resolving the pipeline: `[enrichment.ontology].guidance` present →
**custom atlas**; else explicit `pipeline` pin; else `domain` heuristic
(philosophy → `philosophy_atlas`, else `literary_atlas`).

## Increments

- **CA1** — recipe schema: `OntologyConfig` + `[enrichment.ontology]` on
  `EnrichmentConfig` (`corpus-engine/src/recipe.rs`); `Recipe::custom_ontology()` accessor;
  `produces_enriched_atoms()` already true for `type="atlas"`. Tests parse.
- **CA2** — `CustomAtlasSpec` + `ConfigurableAtlasPipeline` (corpus-engine) mirroring
  `literary_atlas` Phase-1, delegating the rest. Unit tests (phase1_system contains
  guidance; parse/clustering delegate; entity `Other` labels survive).
- **CA3** — `EnrichConfig.ontology: Option<CustomAtlasSpec>` (cli-llm `config.rs`),
  serialized to `config.json`.
- **CA4** — `resolve_pipeline(&EnrichConfig)` SSOT helper; replace all ~10 registry sites;
  accept `custom_atlas` in `init.rs` validation + the `ends_with("_atlas")` gate.
- **CA5** — `enrich init` / `init_from_corpus`: read `[enrichment.ontology]` → write
  `EnrichConfig.ontology` + `pipeline_id="custom_atlas"`.
- **CA6** — desktop bridge: allow `custom_atlas`; map a recipe with `[enrichment.ontology]`
  to it. Skill: teach the agent to **interview the expert and author the ontology guidance**
  (this is the headline authoring experience), atlas-genre pick as the quick fallback.
- **CA7** — end-to-end: author a custom-domain recipe → install → init → build → `atoms.json`
  with domain-appropriate atoms (incl. `EntityType::Other`) → "Verify enrichment" → chat.

## Verification

Workspace lint + `svelte-check` green each increment; CA7 proves a corpus that did NOT
pre-exist gets a domain-tuned, chat-feeding atom graph authored entirely from the app.
