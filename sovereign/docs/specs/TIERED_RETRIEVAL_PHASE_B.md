# Tiered retrieval — Phase B port matrix

**Status:** In flight.
**Lifecycle:** When a corpus port ships, its row promotes to the
stable feature doc ([`../TIERED_RETRIEVAL.md`](../TIERED_RETRIEVAL.md)).
When all rows ship, this spec retires (`decision` note pointing at
the stable doc).

This spec tracks per-corpus port work for the tiered-retrieval
architecture (T1 chunks / T2 entity graph / T3 RAPTOR atlas). The
builders are corpus-agnostic by signature
([`ARCH_PRINCIPLES.md §5.4`](../../ARCH_PRINCIPLES.md)); the
corpus-specific work per port is (1) persistence layer for
skeleton/atlas/motifs, and (2) trigger point that calls into the
tiered pipeline.

---

## Port matrix

| Corpus | Tier-1 store (exists) | Phase-B persistence | Trigger | Status |
|---|---|---|---|---|
| Attached documents | `document_chunks` | shipped 2026-05-22 ✓ | `DocumentAssetManager::ingest` | Shipped |
| Conversations | `messages` + embeddings | New `conversation_skeleton` sidecar table; reuse `raptor_nodes` + `asset_motifs` with `source_id` column (already present) | Run on conversation seal (conversation goes inactive past a threshold) | Shipped 2026-05-23 ✓ |
| Obsidian vault | Corpus-engine chunks per note | shipped 2026-05-24 ✓ — `[[wiki-links]]` parsed into `MarkdownChunkMetadata.wiki_links`, per-note RAPTOR via `FolderTieredProvider`, new `vault_themes` table for cross-note synthesis, incremental sweeper hook re-enriches only changed notes. `vault_skeleton` table NOT added — per-note `conv_skeletons` row keyed by `source_doc_id` was sufficient. | `LocalCorpusManager::ingest` (one-shot); incremental delta on watched-sweep via `CorpusEngine::reindex_changed_sources_tiered` | Shipped |
| Wikipedia corpus | Corpus-engine chunks per article | `WikipediaGraph` (already built, `corpus-engine/src/wikipedia_graph.rs`) IS the T2 entity graph — adapt the EntityGraph trait to read from it. T3 RAPTOR-on-corpus is the new work. | Run on corpus ingest completion | Not started |
| SEP (Stanford Encyclopedia of Philosophy) | Existing corpus index | SEP has its own atlas/atom infrastructure (`sovereign-tools::atlas_*`); heaviest port — needs translation layer between SEP's atom shape and the `RaptorNode` shape | Existing atlas-postinstall hook | Not started |

---

## What Phase B does NOT need to redo

The following are pure-Rust algorithms or runtime gates that
operate on the abstract `(chunks, embeddings, skeleton_data,
raptor_nodes, motifs)` quintuple. They do not need to be rewritten
per corpus:

- RAPTOR k-means clustering (`raptor_atlas.rs`)
- EntityGraph PPR algorithm (`entity_graph.rs`)
- Motif extraction + LLM classification pipeline
  (`document_asset.rs::extract_motif_candidates` +
  `classify_motifs`)
- TextTiling boundary detector
  (`document_asset.rs::detect_segment_boundaries`)
- Quote verification & demotion layer (`quote_verification.rs`)
- Briefing builder tier-gating logic
  (`runtime.rs::build_attached_doc_briefing`)

---

## Per-port acceptance checklist

A port row promotes when all of:

- [ ] Persistence: schema in `sovereign-store/src/migrations.rs`,
      `ON DELETE CASCADE` from owning row, `source_id` namespace
      collision-free with other corpora.
- [ ] Trigger: corpus-specific entry point calls the corpus-
      agnostic builders. No `DocumentAsset` leak into the builder
      signature.
- [ ] Bench: at least one bench fixture in the corpus's bench
      directory exercises a T2 + T3 query; baseline + tiered
      comparison.
- [ ] State machine: `AssetState` (or per-corpus equivalent)
      surfaces `is_queryable / label / progress_fraction` so UI
      can gate on tier.
- [ ] NoteStore: per-port lessons land as `decision` / `invariant`
      / `todo` notes.
- [ ] Doc: row in [`../TIERED_RETRIEVAL.md`](../TIERED_RETRIEVAL.md)
      updated with shipped status.

---

## References

- [`../TIERED_RETRIEVAL.md`](../TIERED_RETRIEVAL.md) — stable
  feature doc (architecture, contract, builders, storage).
- [`../../ARCH_PRINCIPLES.md`](../../ARCH_PRINCIPLES.md) §5.4 —
  pipeline-stages-parameterize-on-data principle.
- NoteStore queries: `sovereign notes --query tiered-retrieval`,
  `sovereign notes --query raptor`,
  `sovereign notes --query hipporag`.
