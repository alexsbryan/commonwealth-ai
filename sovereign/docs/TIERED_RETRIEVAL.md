# Tiered retrieval surface

A corpus-agnostic enrichment architecture that exposes retrieval capability in three progressive tiers, so users can begin querying *useful* answers within seconds of attach instead of waiting for the full enrichment pipeline to complete.

> **Status:** Phase A (attached documents) shipped 2026-05-22. Phase B (port to other corpora) is the next sprint, scoped below.

## Why

The naive ingest pipeline lumps embedding, entity extraction, structural metadata, and synthesis enrichment into a single monolithic phase. Empirically (book-report bench, 2026-05-20 through 2026-05-22) this gave a single ~20-min `attach → Ready` window in which no useful queries could be answered, and the entire window was opaque — users had no signal about which sub-phase was running or when partial capability would become available.

The tiered surface replaces that single gate with three explicit milestones, each unlocking a specific retrieval mode. The user can start asking questions as soon as the first tier lands; quality scales as the later tiers complete.

## The three tiers

| Tier | Available when | Retrieval mode | Backing data |
|---|---|---|---|
| **T1 — chunks** | Embedding done (~1.5 min on a 1000-chunk doc) | Embedding-cosine top-K | `document_chunks` rows with `embedding: Some(Vec<f32>)` |
| **T2 — entity graph** | Lean entity extraction + action atoms done (~6 min more) | T1 + Personalized PageRank over entity co-occurrence graph (HippoRAG-1-style multi-hop; see "On HippoRAG 1 vs 2" below for the disambiguation) | `skeleton.entity_index`, `skeleton.main_entities`, `skeleton.actions`, `skeleton.structural_moments`, `skeleton.sections` |
| **T3 — full atlas** | RAPTOR clusters + motifs + segments + overview done (~12 min more) | T2 + RAPTOR signposts (multi-scale summaries), motif-index lookup, TextTiling segment map, hallucination-safe verbatim quote spans | `raptor_nodes` table, `asset_motifs` table, plus `skeleton.overview`, `skeleton.segments` |

**Key property: each tier composes additively.** A query at T3 uses cosine retrieval (T1) + PPR re-ranking signal (T2) + RAPTOR signpost briefing (T3). The model never has to know which tier is active — it just gets richer retrieval and a fuller briefing as enrichment lands.

## State machine

```
Pending
  → Indexing { chunks_done, chunks_total }
  → PartiallyReady                                          [T1 done]
  → BuildingSkeleton { chunks_done, chunks_total }          [T2 in flight]
  → MultiHopReady                                           [T2 done]
  → BuildingSkeleton { chunks_done, chunks_total }          [T3 in flight — reused variant]
  → Ready                                                   [T3 done]
  | Failed { reason }
```

`AssetState` (in `sovereign-core/src/types.rs`) is the durable persistence form. `IngestProgress` events (in `sovereign-tools/src/document_asset.rs`) are the runtime event stream the desktop UI subscribes to via the `document:progress` Tauri channel.

The `BuildingSkeleton` variant is reused for both the T2 and T3 enrichment phases — the `chunks_done` counter restarts at 0 between them. The progress bar briefly visually resets when the asset transitions through `MultiHopReady`, which is intentional: that reset *is* the visual milestone signal.

## Retrieval contract

Three rules govern how a query dispatches across tiers:

1. **`AssetState::is_queryable()` returns true at any of T1, T2, or T3.** All three states accept queries; quality scales with tier.
2. **The briefing builder (`runtime.rs::build_attached_doc_briefing`) tier-gates implicitly via per-section emptiness checks.** It renders only the sections whose backing data is populated — `overview` empty → skip the overview section, `raptor_nodes` empty → skip the cluster signposts section, etc. No explicit state-check needed.
3. **The retrieval tool (`attached_document_search.rs`) layers signals additively.** Cosine top-16 runs always (T1). PPR re-ranking layers on when `skeleton.actions` and `skeleton.entity_index` are non-empty (T2 done). RAPTOR signposts and motifs surface in the briefing when the corresponding tables are non-empty (T3 done). At T3 an optional cluster-score blend (see "Cluster-score blend" below) can re-rank the candidate pool using leaf-cluster summary embeddings — off by default, opt-in via `SOVEREIGN_DOC_CLUSTER_WEIGHT`.

This composition is what makes the tiered architecture quiet for the caller — no branching on tier state in the query path.

## Builders (corpus-agnostic interfaces)

Each builder takes `(chunks: &[TextChunk], embeddings: &[Vec<f32>], inference, store)` — no `DocumentAsset`, no per-corpus types. This is the load-bearing portability hook: the same functions can be invoked from any corpus ingest path.

### T1 — embeddings

Existing `inference.embed_batch` loop in `document_asset.rs::ingest` (the `embed_future` block). Persists `DocumentChunk` rows with the `embedding` field populated. Already corpus-agnostic.

### T2 — lean entity extraction + action atoms

`build_skeleton` (refactored 2026-05-22 in `document_asset.rs`):

1. Splits chunks into batches of 4
2. Dispatches per-batch entity extraction via `futures::stream::iter(...).buffered(T2_BATCH_CONCURRENCY)` (default 6)
3. Each batch calls Speed::Slow LLM with a `lark_grammar`-enforced lean schema: exactly N newline-separated lines of comma-separated capitalised entity names
4. Merges results sequentially into `entity_mentions`, `entity_kinds`, `sections`, `structural_moments`
5. Ranks `main_entities` by `presence_rate`
6. Calls `extract_action_atoms` for the top entities (6 Fast-slot calls)
7. Returns a *partial* `DocumentSkeleton` with `overview` and `segments` empty — those are T3's responsibility

### T3 — RAPTOR atlas + motifs + segments + overview

Composed inside the `skeleton_future` async block in `document_asset.rs::ingest`:

1. `extract_segments` (TextTiling — adaptive depth-score boundary detection on embedding cosine, see `document_asset.rs::detect_segment_boundaries`) — ~30s, zero LLM
2. `generate_overview` — 1 Slow LLM call, ~20s
3. *(1 and 2 run concurrently via `tokio::join!`)*
4. `build_and_persist_raptor_atlas` (`document_asset.rs:2084`):
   - K-means cluster chunk embeddings into ~50 leaf clusters
   - Per leaf: 1 Slow LLM call to summarise + identify primary entities, output via lark_grammar that forbids `"` so the hallucination contract holds
   - Recurse: cluster summary embeddings, summarise each cluster, until root branching ≤ 4
   - Persist `raptor_nodes` rows
   - TF-IDF motif candidate extraction (pure Rust)
   - 1 Slow LLM call to classify candidates as motif-vs-noise
   - Persist `asset_motifs` rows
5. Updates `skeleton.overview`, `skeleton.segments`, `structural_moments` with T3 outputs
6. Saves full skeleton and transitions to `Ready`

### Post-Ready guardrail

The final response is passed through `sovereign-core/src/quote_verification.rs::verify_quotes` before being packaged. Any `"..."` span ≥ 40 chars that doesn't appear verbatim in the asset's chunks or in a RAPTOR `quote_span` is demoted to `[unverified excerpt: ...]`. This catches composite quotes (real fragments joined with ellipsis into a passage that doesn't appear continuously) which are the user-facing failure mode worse than a low-quality answer.

## Cluster-score blend (optional T3 re-ranking)

Cosine retrieval tells us which chunks resemble the query token-for-token; it tells us nothing about which structural neighbourhood the chunks belong to. RAPTOR's leaf clusters carry that information — every chunk is a direct member of one leaf, and that leaf's `summary_embedding` captures what the surrounding scene is about. When the user's question is structural ("where does the novel resolve Stevie's fate?", "the cluster where the Professor's image deflates") the cosine signal often equidistant-bounces between equally-similar chunks and the right neighbourhood loses on a coin flip. The blend gives cosine a structural-prior partner.

Mechanism, after cosine + PPR have run, before the final top-16 truncate:

1. Fetch the asset's `raptor_nodes` at `level = 0` (leaf clusters).
2. Take the cosine top-`pool` candidates (default `pool = 16`) plus any chunks the PPR recall-boost surfaced, union them, dedupe — this is the candidate pool.
3. For each candidate, look up its leaf cluster, cosine the query embedding against the cluster's `summary_embedding`. Each cluster is scored once; chunks sharing a cluster share the score.
4. Min-max normalise the cosine scores and the cluster scores across the pool. (All-equal scores collapse to a constant `0.5` so the signal contributes a neutral midpoint instead of NaN.)
5. `final = (1 - cluster_weight) · cosine_norm + cluster_weight · cluster_norm`. Sort descending, truncate to 16. The ±1 chunk-neighbour expansion runs on the new top-16 unchanged.

Default `cluster_weight = 0.0` — byte-identical baseline. The block early-returns before computing any cluster scores when the env var is unset, so the cost is zero on the happy path. When the asset hasn't reached T3 yet (`raptor_nodes` empty), the blend falls through to cosine ordering rather than panicking — this is what makes the feature safe to leave on across PartiallyReady / MultiHopReady / Ready transitions.

The pattern descends from the SEP rerank experiment's `atlas_weight` blend (`sovereign/docs/RERANK_EXPERIMENT.md`), which lifted SEP sources 40 → 65 of 66 on the canonical bench. Spec, failure-mode analysis, and bench-validation plan: `sovereign/docs/specs/CLUSTER_SCORE_BLEND.md`. The blend is observable via `tracing::debug!` events under the name `attached_doc_search: cluster-score blend applied`.

## Storage shape

| Tier | Tables / fields |
|---|---|
| T1 | `document_chunks` (existing schema) — `id, source, content, chunk_index, embedding, created_at, source_type` |
| T2 | `document_assets.skeleton_json` (JSON blob containing the partial `DocumentSkeleton` — `sections`, `main_entities`, `entity_index`, `actions`, `structural_moments`; `overview` and `segments` are empty until T3) |
| T3 | `document_assets.skeleton_json` re-saved with `overview` + `segments` filled in. Plus two T3-only tables: `raptor_nodes` (one row per cluster, BLOB-encoded f32 embeddings, JSON-encoded children IDs + chunk IDs + quote spans) and `asset_motifs` (term + tf_idf_score + occurrence_chunk_ids + is_distinctive flag) |

The schema is at `sovereign-store/src/migrations.rs::run_raptor_atlas_migration`. Both tables have `ON DELETE CASCADE` from `document_assets(id)` so cleanup is automatic.

## Performance (measured, 2026-05-22)

Single-peer mesh (one daemon, no fan-out), Conrad's *The Secret Agent* (~1006 chunks):

| Milestone | Time from attach | Notes |
|---|---:|---|
| `PartiallyReady` (T1) | **~1.5 min** | Embedding-only; unchanged from baseline |
| `MultiHopReady` (T2) | **~7.7 min** | NEW capability milestone; PPR multi-hop available from here |
| `Ready` (T3) | **~19.7 min** | Roughly unchanged from pre-tiered baseline (~20 min) |

**The wall-clock to full Ready barely moved.** The plan's projected 3-5× speedup didn't materialise because the `buffered(6)` parallelism in T2 only pays off on a multi-peer mesh. With a single Slow slot serving the daemon, the 6 concurrent requests queue and execute serially through the same KV cache.

**The real win is the milestone, not the absolute time.** Users can now do *multi-hop entity-walk retrieval* at 7.7 min instead of waiting 20 min for any non-cosine retrieval. That's a meaningful UX shift even without total-time reduction.

When ≥ 2 peers join the mesh and the Slow-slot inference can actually fan out, T2 should drop to ~2 min and total Ready to ~10 min. That's an architectural property: the same code path, different runtime topology. Phase B (porting to other corpora) doesn't have to wait for the mesh speed-up to land.

## Porting to other corpora (Phase B sketch)

The builders are corpus-agnostic by signature. The corpus-specific work per port is (1) the persistence layer for skeleton/atlas/motifs, and (2) the trigger point that calls into the tiered pipeline.

| Corpus | Tier-1 store (already exists) | Phase-B persistence work | Trigger point |
|---|---|---|---|
| Attached documents | `document_chunks` | shipped ✓ | `DocumentAssetManager::ingest` |
| Conversations | `messages` + embeddings | New `conversation_skeleton` sidecar table; reuse `raptor_nodes` + `asset_motifs` with a `source_id` column already present | Run on conversation seal (when the conversation goes inactive past a threshold) |
| Obsidian vault | Corpus-engine chunks per note | New `vault_skeleton` table per vault root; treat `[[wiki-links]]` parsed from markdown as pre-built entity-co-occurrence edges (skip the T2 LLM pass entirely for vaults — the user did the entity tagging by hand) | Run on vault sync; incremental per-note re-enrichment when a file changes |
| Wikipedia corpus | Corpus-engine chunks per article | `WikipediaGraph` (already built, lives in `corpus-engine/src/wikipedia_graph.rs`) IS the T2 entity graph — just adapt the EntityGraph trait to read from it. T3 RAPTOR-on-corpus is the new work | Run on corpus ingest completion |
| SEP (Stanford Encyclopedia of Philosophy) | Existing corpus index | SEP has its own atlas/atom infrastructure (`sovereign-tools::atlas_*`); the heaviest port — likely needs a translation layer between SEP's atom shape and our `RaptorNode` shape | Existing atlas-postinstall hook |

### The three architectural commitments that protect portability

1. **Builder signatures are corpus-free.** `build_raptor_atlas`, `EntityGraph::build`, `extract_motif_candidates`, `detect_segment_boundaries` — none of them accept a `DocumentAsset`. They take chunks + embeddings + inference + store handles.

2. **Storage tables key on `asset_id`-shaped IDs, not document-specific identifiers.** `raptor_nodes` and `asset_motifs` both use string IDs. A conversation, a vault, a corpus, or an SEP article can all populate these tables under their own ID namespace without schema changes.

3. **The state machine is per-source, not per-document.** `AssetState` happens to live on `DocumentAsset` for the attached-doc case, but the variant set (Pending → Indexing → PartiallyReady → BuildingSkeleton → MultiHopReady → BuildingSkeleton → Ready → Failed) is universal. Phase B can either embed `AssetState` into each corpus's own metadata schema or introduce a generic `CorpusEnrichmentTier` trait if it proves load-bearing across more than two ports.

### What Phase B does NOT need to redo

- The RAPTOR k-means clustering code (`raptor_atlas.rs`)
- The EntityGraph PPR algorithm (`entity_graph.rs`)
- The motif extraction + LLM classification pipeline (`document_asset.rs::extract_motif_candidates` + `classify_motifs`)
- The TextTiling boundary detector (`document_asset.rs::detect_segment_boundaries`)
- The quote verification & demotion layer (`quote_verification.rs`)
- The briefing builder's tier-gating logic (`runtime.rs::build_attached_doc_briefing`)

All of those are pure-Rust algorithms or runtime gates that operate on the abstract `(chunks, embeddings, skeleton_data, raptor_nodes, motifs)` quintuple. They don't need to be rewritten per corpus.

## Honest known gaps

These are real things the architecture as-shipped doesn't yet handle. The Phase A scope didn't include them; calling them out so future sessions don't re-derive the surprise.

### Parallelism speed claim is mesh-dependent

The `buffered(6)` in T2 only delivers speedup with ≥ 2 mesh peers. On a single-daemon deployment, T2 takes the same wall-clock as the prior sequential skeleton. Time-to-MultiHopReady is therefore *currently* dominated by the same per-chunk-batch LLM cost the legacy pipeline had — the architectural separation buys us the *capability milestone* but not the *wall-clock win*.

This is honest and not a defect of the refactor — it's the topology being underpowered. The fix is mesh capacity, not more refactoring.

### T3 progress is coarse-grained

`build_and_persist_raptor_atlas` emits progress events at 4 fixed checkpoints (chunks-fetched ~20%, RAPTOR tree built ~75%, RAPTOR persisted ~80%, motifs done ~95%). The UI bar visibly jumps between these points. Per-cluster progress inside `build_raptor_atlas` is not exposed; making it granular would require threading a callback through the recursive tree builder.

For the swiss-army-knife UX this is acceptable — the user sees "things are happening" instead of a frozen 0/N bar — but a power-user view that shows actual cluster-completion would be valuable.

### Bench hallucination detector overcounts on markdown formatting

Documented earlier in session memos; not a Phase A concern but flagged here because the user-trust framing of T3 (verbatim verification) makes this worth noting. The bench's `"([^"]{30,240})"` regex picks up the model's own `**bold**` formatting as if it were a quotation, leading to inflated `⚠ N fabricated` counters even when the actual quote-verification layer caught zero real fabrications. The fix lives in the bench rubric, not the runtime.

### Quality on synthesis tiers (T5 anti-canonical questions) didn't improve

Phase A targeted *speed-to-usable* and *portability*; it explicitly did not commit to lifting bench quality. The book-report bench's T5 anti-canonical questions remained at judge 0/5 across all three test questions in the May-22 full-20 run. That's its own investigation surface, addressed in a separate sovereign note (`raptor-hipporag-session-handoff-2026-05-22`).

The failure mode on T5 specifically is **synthesis-side, not retrieval-side**. The complicating chunks the anti-canonical rubric anchors on (e.g. chunks 296-298 for the Professor / Heat encounter, 943 / 967 for the Professor's physical deflation) **were already in the model's retrieval set** in the failing runs. The model lost on mech-fact matching (paraphrasing load-bearing Conrad vocabulary instead of quoting it verbatim) and on judge-rubric contamination traps (leaning on received critical opinion when the rubric expects textual pushback). Better retrieval alone won't fix that — see the "On HippoRAG 1 vs 2" section for why v2-style retrieval enhancements aren't the right tool for this specific failure class, and what the actual higher-leverage interventions look like.

## Reading order for new contributors

If you're picking this up for Phase B work or just trying to understand the architecture:

1. **This document.**
2. `sovereign-tools/src/document_asset.rs::ingest` — the orchestration. Read from the top of the `embed_future` block (T1) through the `skeleton_future` block (T2 + T3) to see the state-machine flow.
3. `sovereign-core/src/types.rs::AssetState` — the state variants + the `is_queryable / label / progress_fraction` methods that drive UI gating.
4. `sovereign-tools/src/raptor_atlas.rs` — the corpus-agnostic RAPTOR builder. Self-contained module.
5. `sovereign-tools/src/entity_graph.rs` — the PPR multi-hop signal. Also self-contained.
6. `sovereign-core/src/runtime.rs::build_attached_doc_briefing` — the tier-gated retrieval surface that the model sees.
7. `sovereign-core/src/quote_verification.rs` — the post-generation guardrail.

## On HippoRAG 1 vs 2

Earlier session framing was sloppy about which HippoRAG paper our T2 layer is descended from. Cleaning that up here:

**What we implemented — HippoRAG-1-style.** The `entity_graph.rs` module mirrors the original HippoRAG (NeurIPS '24) shape: extract subject-predicate-object triples per chunk, build an entity co-occurrence graph with triple-anchored edge weights, run Personalized PageRank seeded from query-mentioned entities at retrieval time. Our triples come from `skeleton.actions` (the per-entity action atoms the legacy skeleton produces); HippoRAG paper used OpenIE. The PPR algorithm itself is standard.

**What we did NOT implement — HippoRAG 2 (ICML '25).** Specific advances we did not port:
- *Richer evidence weighting in PageRank* — v2 weights chunks by passage-level relevance during diffusion, not just by entity-presence
- *Fact-nodes as first-class graph members* — v2 treats triples themselves as nodes, so PPR diffuses through fact-nodes alongside entity-nodes
- *Sense-making framing* — v2 explicitly targets "integrating large and complex contexts" as a separate retrieval mode

**Why this isn't called out as a Phase B item.** The honest assessment: we do not have measured evidence that the question classes failing on the book-report bench today (especially T5 anti-canonical and the weaker T4 thematic questions) would lift on a v2 mechanism. Reading those failures carefully:

- T5 (all three 0/5 judge across professor_menace / yundt_as_threat_check / novel_politics_check): the model **already retrieves the complicating chunks** the rubric anchors on. It loses on mech by paraphrasing load-bearing Conrad vocabulary (`frail` → "psychologically fragile"; `explosives` → "bomb"; `monologue` → never written) and on judge by tripping the contamination traps when the answer leans on received critical opinion. These are synthesis-side failures. Better retrieval doesn't change what words the model writes.

- T4 weak (deflation_of_revolutionaries 3/5, marriage_as_compact 2/5): need multi-passage thematic anchoring. The shipped HippoRAG-1-style PPR theoretically helps here, but the bench answers show the relevant chunks were already in the retrieval set — the synthesis is what's thin, not the recall.

So HippoRAG 2 is filed under "future research surface to evaluate against MuSiQue / 2Wiki / LV-Eval-class benchmarks where its mechanism is published-load-bearing" — not under "obvious Phase B win for our current failure modes."

The higher-leverage interventions for the T4/T5 quality gaps look different:

1. **Vocabulary-fidelity prompt rule** — instruct the model to use the document's exact words when they appear in retrieved chunks (catches mech misses without bench-specific teaching-to-the-test)
2. **Shape-level anti-canonical question recognition** — questions of the form "X is often read as Y; complicate that reading" get briefing-level guidance to argue *with* the text against received opinion, not just summarise
3. **Distinctive-vocabulary reranking at retrieval** — a chunk containing the rare distinctive Conrad word (`frail`) outranks a chunk containing the common paraphrase (`slender`). This is the motif index doing additional work at retrieval time, not just briefing time
4. **Sparser retrieval** — fewer, higher-quality chunks so synthesis grounds tightly per chunk and the model has to quote verbatim instead of paraphrasing

Those are the right candidates for the next quality sprint. HippoRAG 2 is interesting but the published mechanism doesn't target the specific failure modes we see here.

## References

- Architecture plan: `/home/alexbryan/.claude/plans/let-s-make-a-proper-curried-peach.md`
- Session handoff with bench-result framing: sovereign note `raptor-hipporag-session-handoff-2026-05-22`
- Skeleton-additive-not-replacement caveat: sovereign note `raptor-additive-not-replacement`
- WikipediaGraph mmap-pragma fix (RAM headroom for tiered ingest to actually run): sovereign note `corpus-engine-mmap-pragma-sizing`
- Prompt parsimony lesson driving the briefing v3 wording: sovereign note `small-model-prompt-parsimony`
- HippoRAG 1 paper (NeurIPS '24) — the EntityGraph + PPR mechanism shipped here is descended from this: https://github.com/osu-nlp-group/hipporag (the v1 branch / original paper)
- HippoRAG 2 paper (ICML '25) — related, NOT implemented here; see "On HippoRAG 1 vs 2" section above for the honest framing
- RAPTOR paper (Stanford ICLR 2024): the hierarchical summarisation pattern the atlas builder implements
