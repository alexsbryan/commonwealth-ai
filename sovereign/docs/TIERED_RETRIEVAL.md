# Tiered retrieval surface

A corpus-agnostic enrichment architecture that exposes retrieval
capability in three progressive tiers, so users can begin querying
*useful* answers within seconds of attach instead of waiting for the
full enrichment pipeline to complete.

> **Status:** Phase A (attached documents) shipped 2026-05-22.
> Phase B port to conversations shipped 2026-05-23. Phase B port to
> Obsidian vaults shipped 2026-05-24. Phase B port to additional
> corpora is in flight — see
> [`specs/TIERED_RETRIEVAL_PHASE_B.md`](specs/TIERED_RETRIEVAL_PHASE_B.md).

---

## Why

The naive ingest pipeline lumped embedding, entity extraction,
structural metadata, and synthesis enrichment into a single
monolithic phase. Empirically (book-report bench, 2026-05-20
through 2026-05-22) that gave a single ~20-min `attach → Ready`
window in which no useful queries could be answered, and the entire
window was opaque — users had no signal about which sub-phase was
running or when partial capability would become available.

The tiered surface replaces that single gate with three explicit
milestones, each unlocking a specific retrieval mode. The user can
start asking questions as soon as the first tier lands; quality
scales as the later tiers complete.

---

## The three tiers

| Tier | Available when | Retrieval mode | Backing data |
|---|---|---|---|
| **T1 — chunks** | Embedding done (~1.5 min on a 1000-chunk doc) | Embedding-cosine top-K | `document_chunks` rows with `embedding: Some(Vec<f32>)` |
| **T2 — entity graph** | Lean entity extraction + action atoms done (~6 min more) | T1 + Personalized PageRank over entity co-occurrence graph (HippoRAG-1-style multi-hop) | `skeleton.entity_index`, `skeleton.main_entities`, `skeleton.actions`, `skeleton.structural_moments`, `skeleton.sections` |
| **T3 — full atlas** | RAPTOR clusters + motifs + segments + overview done (~12 min more) | T2 + RAPTOR signposts (multi-scale summaries) + motif-index lookup + TextTiling segment map + hallucination-safe verbatim quote spans | `raptor_nodes` table, `asset_motifs` table, plus `skeleton.overview`, `skeleton.segments` |

**Key property: each tier composes additively.** A query at T3 uses
cosine retrieval (T1) + PPR re-ranking signal (T2) + RAPTOR
signpost briefing (T3). The model never has to know which tier is
active — it just gets richer retrieval and a fuller briefing as
enrichment lands.

---

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

`AssetState` (in `sovereign-core/src/types.rs`) is the durable
persistence form. `IngestProgress` events (in
`sovereign-tools/src/document_asset.rs`) are the runtime event
stream the desktop UI subscribes to via the `document:progress`
Tauri channel.

The `BuildingSkeleton` variant is reused for both T2 and T3
phases — the `chunks_done` counter restarts at 0 between them. The
progress bar briefly visually resets when the asset transitions
through `MultiHopReady`. That reset *is* the visual milestone
signal.

---

## Retrieval contract

Three rules govern how a query dispatches across tiers:

1. **`AssetState::is_queryable()` returns true at any of T1, T2, or T3.**
   All three states accept queries; quality scales with tier.
2. **The briefing builder (`runtime.rs::build_attached_doc_briefing`)
   tier-gates implicitly via per-section emptiness checks.** It
   renders only the sections whose backing data is populated —
   `overview` empty → skip the overview section, `raptor_nodes`
   empty → skip the cluster signposts section, etc. No explicit
   state-check needed.
3. **The retrieval tool (`attached_document_search.rs`) layers
   signals additively.** Cosine top-16 runs always (T1). PPR
   re-ranking layers on when `skeleton.actions` and
   `skeleton.entity_index` are non-empty (T2 done). RAPTOR
   signposts and motifs surface in the briefing when the
   corresponding tables are non-empty (T3 done). At T3 an optional
   cluster-score blend can re-rank the candidate pool using leaf-
   cluster summary embeddings — off by default, opt-in via
   `SOVEREIGN_DOC_CLUSTER_WEIGHT`.

This composition is what makes the tiered architecture quiet for
the caller — no branching on tier state in the query path.

---

## Builders (corpus-agnostic interfaces)

Each builder takes `(chunks: &[TextChunk], embeddings: &[Vec<f32>],
inference, store)` — no `DocumentAsset`, no per-corpus types. This
is the load-bearing portability hook: the same functions can be
invoked from any corpus ingest path.

See [`ARCH_PRINCIPLES.md §5.4`](../ARCH_PRINCIPLES.md) for the
underlying principle (pipeline stages parameterize on data, not
source identity). The portability work in §5.4 descended from this
architecture.

### T1 — embeddings

Existing `inference.embed_batch` loop in `document_asset.rs::ingest`
(the `embed_future` block). Persists `DocumentChunk` rows with the
`embedding` field populated. Already corpus-agnostic.

### T2 — lean entity extraction + action atoms

`build_skeleton` (refactored 2026-05-22 in `document_asset.rs`):

1. Splits chunks into batches of 4
2. Dispatches per-batch entity extraction via
   `futures::stream::iter(...).buffered(T2_BATCH_CONCURRENCY)`
   (default 6)
3. Each batch calls Speed::Slow LLM with a `lark_grammar`-enforced
   lean schema: exactly N newline-separated lines of comma-
   separated capitalised entity names
4. Merges results sequentially into `entity_mentions`,
   `entity_kinds`, `sections`, `structural_moments`
5. Ranks `main_entities` by `presence_rate`
6. Calls `extract_action_atoms` for the top entities (6 Fast-slot
   calls)
7. Returns a *partial* `DocumentSkeleton` with `overview` and
   `segments` empty — those are T3's responsibility

> **Dual-layer entity extraction on the conversation path (added
> 2026-05-26).** The LLM + `lark_grammar` extraction above is the
> *document-asset* T2. For **conversations**, a second source layers on:
> a real GLiNER ONNX model (`gline-rs`, `gliner_small-v2.1`, feature
> `gliner-ner`, module `sovereign-tools/src/gliner_ner.rs`) runs per-chunk
> NER, and `sovereign-core/src/conv_entity_graph.rs::from_layered` merges
> RAPTOR's cluster-summary `primary_entities` with the GLiNER per-chunk
> mentions into one entity graph (orthogonal signals: cluster-scale
> distinctiveness + raw NER). The hybrid retrieval scorer
> (`0.6·cosine + 0.4·jaccard`, MMR, `topic_context`) in
> `runtime/retrieval.rs` is **default-on** since this landing. See
> [`../../corpus-engine/ENRICHMENT.md`](../../corpus-engine/ENRICHMENT.md)
> for how this fits the three-system picture.

### T3 — RAPTOR atlas + motifs + segments + overview

Composed inside the `skeleton_future` async block in
`document_asset.rs::ingest`:

1. `extract_segments` (TextTiling — adaptive depth-score boundary
   detection on embedding cosine, see
   `document_asset.rs::detect_segment_boundaries`) — ~30s, zero
   LLM
2. `generate_overview` — 1 Slow LLM call, ~20s
3. *(1 and 2 run concurrently via `tokio::join!`)*
4. `build_and_persist_raptor_atlas` (`document_asset.rs:2084`):
   - K-means cluster chunk embeddings into ~50 leaf clusters
   - Per leaf: 1 Slow LLM call to summarise + identify primary
     entities, output via lark_grammar that forbids `"` so the
     hallucination contract holds
   - Recurse: cluster summary embeddings, summarise each cluster,
     until root branching ≤ 4
   - Persist `raptor_nodes` rows
   - TF-IDF motif candidate extraction (pure Rust)
   - 1 Slow LLM call to classify candidates as motif-vs-noise
   - Persist `asset_motifs` rows
5. Updates `skeleton.overview`, `skeleton.segments`,
   `structural_moments` with T3 outputs
6. Saves full skeleton and transitions to `Ready`

### Post-Ready guardrail

The final response is passed through
`sovereign-core/src/quote_verification.rs::verify_quotes` before
being packaged. Any `"..."` span ≥ 40 chars that doesn't appear
verbatim in the asset's chunks or in a RAPTOR `quote_span` is
demoted to `[unverified excerpt: ...]`. This catches composite
quotes (real fragments joined with ellipsis into a passage that
doesn't appear continuously) which are the user-facing failure
mode worse than a low-quality answer.

---

## Cluster-score blend (optional T3 re-ranking)

Cosine retrieval tells us which chunks resemble the query token-
for-token; it tells us nothing about which structural neighbourhood
the chunks belong to. RAPTOR's leaf clusters carry that information
— every chunk is a direct member of one leaf, and that leaf's
`summary_embedding` captures what the surrounding scene is about.
When the user's question is structural ("where does the novel
resolve Stevie's fate?", "the cluster where the Professor's image
deflates") the cosine signal often equidistant-bounces between
equally-similar chunks and the right neighbourhood loses on a
coin flip. The blend gives cosine a structural-prior partner.

Mechanism, after cosine + PPR have run, before the final top-16
truncate:

1. Fetch the asset's `raptor_nodes` at `level = 0` (leaf clusters).
2. Take the cosine top-`pool` candidates (default `pool = 16`)
   plus any chunks the PPR recall-boost surfaced, union them,
   dedupe — this is the candidate pool.
3. For each candidate, look up its leaf cluster, cosine the query
   embedding against the cluster's `summary_embedding`. Each
   cluster is scored once; chunks sharing a cluster share the
   score.
4. Min-max normalise the cosine scores and the cluster scores
   across the pool. (All-equal scores collapse to a constant
   `0.5` so the signal contributes a neutral midpoint instead of
   NaN.)
5. `final = (1 - cluster_weight) · cosine_norm + cluster_weight · cluster_norm`.
   Sort descending, truncate to 16. The ±1 chunk-neighbour
   expansion runs on the new top-16 unchanged.

Default `cluster_weight = 0.0` — byte-identical baseline. The
block early-returns before computing any cluster scores when the
env var is unset, so the cost is zero on the happy path. When the
asset hasn't reached T3 yet (`raptor_nodes` empty), the blend
falls through to cosine ordering rather than panicking — this is
what makes the feature safe to leave on across PartiallyReady /
MultiHopReady / Ready transitions.

The pattern descends from the SEP rerank experiment's
`atlas_weight` blend (see
[`archive/RERANK_EXPERIMENT.md`](archive/RERANK_EXPERIMENT.md)),
which lifted SEP sources 40 → 65 of 66 on the canonical bench.
Spec, failure-mode analysis, and bench-validation plan:
[`specs/CLUSTER_SCORE_BLEND.md`](specs/CLUSTER_SCORE_BLEND.md). The
blend is observable via `tracing::debug!` events under the name
`attached_doc_search: cluster-score blend applied`.

---

## Chunk-neighbour expansion (now OFF by default)

The attached-doc retrieval tool used to expand every HIT chunk to
its ±1 chunk-index neighbours, producing 3-chunk windows in the
tool result. That landed on 2026-05-21 with a quality claim of T3
judge +1.6 / T5 judge +1.2, at a measured cost of +75s per
question.

Re-measured 2026-05-22 after RAPTOR atlas landed in the briefing:
a 4-rep A/B on the diagnostic triplet showed ±1 ON at 611s wall
vs OFF mean 340s across 4 reps — a robust **−44% / −271s win**,
far outside this bench's variance band. Quality changes within
variance.

Working hypothesis: the RAPTOR atlas's scene-map signposts and the
motif index now in the briefing absorb what ±1 was previously
buying — the model gets thematic neighbourhood context through the
briefing without paying the prefill tax for raw neighbour chunks
on every tool-loop iteration.

Default flipped to OFF 2026-05-22. Operators wanting the prior
behaviour set `SOVEREIGN_DOC_CHUNK_NEIGHBOURS=1`. Flag stays in
place so a future full-20 bench A/B can flip back if the question-
class breakdown shows ±1 still earning its keep outside the
diagnostic triplet.

---

## Storage shape

| Tier | Tables / fields |
|---|---|
| T1 | `document_chunks` (existing schema) — `id, source, content, chunk_index, embedding, created_at, source_type` |
| T2 | `document_assets.skeleton_json` (JSON blob: partial `DocumentSkeleton` — `sections`, `main_entities`, `entity_index`, `actions`, `structural_moments`; `overview` and `segments` empty until T3) |
| T3 | `document_assets.skeleton_json` re-saved with `overview` + `segments` filled. Plus two T3-only tables: `raptor_nodes` (one row per cluster, BLOB-encoded f32 embeddings, JSON-encoded children IDs + chunk IDs + quote spans) and `asset_motifs` (term + tf_idf_score + occurrence_chunk_ids + is_distinctive flag) |

Schema at
`sovereign-store/src/migrations.rs::run_raptor_atlas_migration`.
Both tables have `ON DELETE CASCADE` from `document_assets(id)` so
cleanup is automatic.

Per-corpus measurements + known gaps live in NoteStore — query
`sovereign notes --query tiered-retrieval`.

---

## Typed-extension pass (bench-side atoms over RAPTOR summaries)

At the tail of every tiered corpus build,
`FolderTieredProvider::finalize_corpus` runs (1) `run_vault_synthesis`
→ `vault_themes`, then (2) `run_typed_extension`
(`sovereign-tools/src/typed_extension/`) → a golden-compatible
`atoms.json` under the corpus's `atlas/` dir. Two LLM passes: Pass A
per RAPTOR leaf extracts mechanism / named_position / evidence;
Pass B per vault theme extracts opposition / concession (cross-leaf
shapes). Idempotent via the `atoms.meta.json` manifest sidecar — an
unchanged corpus re-run makes zero LLM calls. Operator re-run surface
(prompt iteration without a rebuild):
`sovereign atlas typed-extension <corpus>`.

This is a **bench-side** artifact: no chat-path surface (briefing,
rerank) reads these atoms; `sovereign bench obsidian
--corpus <vault-corpus>` scores them against
`sovereign/bench/obsidian/golden.toml`. Rationale + atom shapes:
[`specs/TYPED_EXTENSION_PASS.md`](./specs/TYPED_EXTENSION_PASS.md)
(shipped 2026-05-24).

---

## Reading order for new contributors

1. **This document.**
2. `sovereign-tools/src/document_asset.rs::ingest` — the
   orchestration. Read from the top of the `embed_future` block
   (T1) through the `skeleton_future` block (T2 + T3) to see the
   state-machine flow.
3. `sovereign-core/src/types.rs::AssetState` — the state variants
   + the `is_queryable / label / progress_fraction` methods that
   drive UI gating.
4. `sovereign-tools/src/raptor_atlas.rs` — the corpus-agnostic
   RAPTOR builder. Self-contained module.
5. `sovereign-tools/src/entity_graph.rs` — the PPR multi-hop
   signal. Also self-contained.
6. `sovereign-core/src/runtime.rs::build_attached_doc_briefing` —
   the tier-gated retrieval surface that the model sees.
7. `sovereign-core/src/quote_verification.rs` — the post-
   generation guardrail.

---

## On HippoRAG 1 vs 2

Earlier session framing was sloppy about which HippoRAG paper our
T2 layer is descended from. Cleaning that up here:

**What we implemented — HippoRAG-1-style.** The `entity_graph.rs`
module mirrors the original HippoRAG (NeurIPS '24) shape: extract
subject-predicate-object triples per chunk, build an entity co-
occurrence graph with triple-anchored edge weights, run
Personalized PageRank seeded from query-mentioned entities at
retrieval time. Our triples come from `skeleton.actions` (the per-
entity action atoms the legacy skeleton produces); HippoRAG paper
used OpenIE. The PPR algorithm itself is standard.

**What we did NOT implement — HippoRAG 2 (ICML '25).** Specific
advances we did not port:

- *Richer evidence weighting in PageRank* — v2 weights chunks by
  passage-level relevance during diffusion, not just by entity-
  presence
- *Fact-nodes as first-class graph members* — v2 treats triples
  themselves as nodes, so PPR diffuses through fact-nodes
  alongside entity-nodes
- *Sense-making framing* — v2 explicitly targets "integrating
  large and complex contexts" as a separate retrieval mode

**Why this isn't called out as a Phase B item.** Honest assessment:
no measured evidence that the question classes failing on the
book-report bench today (especially T5 anti-canonical) would lift
on a v2 mechanism. The T5 failures are synthesis-side, not
retrieval-side — the model **already retrieves the complicating
chunks** and loses on mech by paraphrasing load-bearing vocabulary
and on judge by tripping contamination traps. Better retrieval
doesn't change what words the model writes.

So HippoRAG 2 is filed under "future research surface to evaluate
against MuSiQue / 2Wiki / LV-Eval-class benchmarks where its
mechanism is published-load-bearing" — not under "obvious Phase B
win for our current failure modes." The higher-leverage
interventions for T4/T5 quality gaps are noted in NoteStore (query
`sovereign notes --query t5-anti-canonical`).

---

## References

- HippoRAG 1 (NeurIPS '24) — the EntityGraph + PPR mechanism
  shipped here descends from this:
  https://github.com/osu-nlp-group/hipporag
- HippoRAG 2 (ICML '25) — related, NOT implemented; see the
  HippoRAG 1 vs 2 section above for honest framing.
- RAPTOR (Stanford ICLR 2024) — the hierarchical summarisation
  pattern the atlas builder implements.
- [`ARCH_PRINCIPLES.md §5.4`](../ARCH_PRINCIPLES.md) — the
  portability principle this architecture descended from.
- [`specs/TIERED_RETRIEVAL_PHASE_B.md`](specs/TIERED_RETRIEVAL_PHASE_B.md)
  — active per-corpus port matrix.
- [`specs/CLUSTER_SCORE_BLEND.md`](specs/CLUSTER_SCORE_BLEND.md) —
  T3 re-ranking design notes.
