# Spec: Progressive Enrichment Pattern (RAPTOR + GliNER, layered)

**Status:** Shipped on `conversations-anthropic` 2026-05-23. Stays
in `docs/specs/` as a **reference pattern**, not an in-flight
spec — any future corpus port (vault, SEP, attached docs,
wikipedia subsets) should adopt this layered shape rather than the
early mutually-exclusive picker.

**Lifecycle:** Reference-pattern specs stay in `specs/`
indefinitely because they're prescriptive for future work, not a
record of intent. Distinct from in-flight specs (which retire on
ship) and canonical wire specs like [`oicp.md`](oicp.md) (which
evolve in-place).

**Prerequisites for reading:** [`../TIERED_RETRIEVAL.md`](../TIERED_RETRIEVAL.md)
for the T1/T2/T3 tier architecture;
[`CONV_TIERED_PORT.md`](CONV_TIERED_PORT.md) for the conv-specific
instance.

## Why this exists

When porting tiered retrieval to a new corpus, the temptation is to pick ONE entity-extraction strategy: either LLM-extracted cluster summaries (RAPTOR `primary_entities`) OR per-chunk NER (GliNER). The first port (`conversations-anthropic`) made them mutually exclusive — runtime picked one or the other based on data availability.

That was wrong. The two sources carry **orthogonal signals** that compose additively. Pure-NER misses LLM-judged distinctiveness; pure-LLM misses surface-form recall. **They should layer, not compete.**

## The signals

| Source | Captures | Density | Cost |
|---|---|---:|---|
| **RAPTOR `primary_entities`** | LLM-judged DISTINCTIVENESS at cluster scale — "what anchors this cluster's summary" | ~5 per leaf | Free byproduct of cluster summarisation (no new LLM call) |
| **GliNER chunk_entities** | Surface-form NER RECALL at chunk scale — "every named thing the model recognised" | ~10–25 per chunk | ~250ms/chunk CPU one-time |
| **Conv-clique baseline** | Structural background — "these entities co-exist in this conversation" | n² per conv | Pure math, runtime negligible |

**Cross-source agreement is the high-confidence signal.** A pair of entities mentioned in BOTH RAPTOR's cluster summary AND in the same GliNER-extracted chunk = strong topical bond. The graph builder must surface this.

## The graph

Per-conversation entity co-occurrence graph (`sovereign-core/src/conv_entity_graph.rs::ConvEntityGraph`). Nodes = entities (deduped by case-insensitive text). Edges = co-occurrence weights, additive across three layers:

```
total_edge_weight(A, B) =
    CONV_CLIQUE_WEIGHT                                          // 0.1   (always, if A,B in same conv)
  + sum over shared RAPTOR clusters: cluster_coherence          // 0.5-0.95
  + sum over shared chunks: CHUNK_CO_OCCURRENCE_WEIGHT          // 0.5 per shared chunk
```

Numerical example for `conversations-anthropic`'s "Borges in Music" conv:
- `Borges`↔`Bach` appears in RAPTOR leaf-2 (coherence 0.86) AND GliNER chunks 4001, 4003 (2 shared)
- Total edge: 0.1 + 0.86 + (0.5 × 2) = **1.96**
- `Borges`↔`Italo Calvino` appears only at conv-clique (no shared cluster, no shared chunk)
- Total edge: 0.1

PPR mass diffuses preferentially through the stronger bonds — RAPTOR-confirmed AND GliNER-confirmed pairs dominate.

## Three constructors, one struct

```rust
impl ConvEntityGraph {
    pub fn from_raptor_nodes(corpus_id, conv_uuid, &[ConvRaptorNodeRow]) -> Self;
    pub fn from_chunk_entities(corpus_id, conv_uuid, &[ChunkEntityRow]) -> Self;
    pub fn from_layered(corpus_id, conv_uuid, &[ConvRaptorNodeRow], &[ChunkEntityRow]) -> Self;
}
```

The two single-source constructors collapse to `from_layered` with one argument empty. Runtime should **always call `from_layered` unconditionally** and let the empty-collection branches no-op the unused layer. Don't branch at the call site on "which data exists" — the constructor handles it.

## Retrieval-time use

`Runtime::rerank_conv_chunks_via_ppr` (`sovereign-core/src/runtime.rs`):
1. For each unique `(corpus_id, conv_uuid)` in the hit set:
   - Read `chunk_entities` rows + RAPTOR nodes from `SqliteStateStore`.
   - Build layered graph.
   - Match query tokens against entity names → seed indices.
   - Run Personalized PageRank (damping 0.85, 20 iterations).
   - Project entity mass onto chunks via the reverse `entity_to_nodes` + `node_to_chunks` index.
2. Min-max normalise cosine and entity-mass scores across the conv chunks; blend: `score = (1-w) * cosine + w * entity_mass` (default `w = 0.25`).
3. Re-sort chunks. Surfaces entity-bridged hits ahead of cosine-only retrievals.

## Persistence

Two SQLite tables on `~/.svrnmesh/sovereign.db` (`sovereign-store/src/migrations.rs`):

```sql
-- RAPTOR side (existing, shared with attached-doc atlases)
conv_raptor_nodes (corpus_id, conv_uuid, node_id, level, summary,
                   primary_entities_json, cluster_coherence,
                   direct_member_chunk_ids_json, …)

-- GliNER side (Phase 1)
chunk_entities (corpus_id, chunk_id, text, label, char_start,
                char_end, score, conv_uuid, extracted_at)
  PRIMARY KEY (corpus_id, chunk_id, text, label)   -- idempotent
```

Both are corpus-namespaced. Both queryable by `(corpus_id, conv_uuid)` for the per-conv graph build.

## Propagating to a new corpus

Five concrete moves. Each can be done independently — they layer.

### 1. **Ingest** — write to `chunk_entities`

Wire the corpus's tiered ingest path to fire `ChunkEntityExtractor::extract_for_conversation` (or the per-source equivalent) ahead of the LLM-heavy `TieredEnrichmentProvider`. The daemon hook in `corpus-engine/src/enrichment/tiered.rs::run_tiered_enrichment` already does this — any corpus using the tiered runner gets entity extraction for free if `with_chunk_entity_extractor` is attached at daemon startup.

For corpora NOT using the tiered runner (attached docs, vault syncs):
- Add a similar hook at the end of T1.
- Use `GlinerChunkExtractor::extract_for_conversation` directly, or factor a `ChunkEntityExtractor` impl for that corpus's source-grouping semantics ("conv_uuid" for chats, "note_id" for vault, "asset_id" for attached docs).

### 2. **Choose your `source_doc_id` semantics**

`chunk_entities.conv_uuid` is the grouping key. For non-conv corpora, repurpose it as whatever logical grouping makes sense — vault note id, attached-doc asset id, Wikipedia article slug. The schema is corpus-agnostic; the column name is "conv_uuid" only because conv was the first port. **Do not add per-corpus columns** — the runtime's per-conv-group iteration works on any grouping that has a stable string id.

### 3. **Choose how the graph node maps to chunks**

In `from_chunk_entities`, each chunk becomes one graph node. For corpora where finer or coarser grouping helps:
- **Section-aware**: vault notes have headings — group entities by `(note_id, section_id)` instead of `(note_id, chunk_id)`. Smaller fan-out, stronger per-section bonds.
- **Position-windowed**: attached docs gain from sliding-window co-occurrence (entities within ±N chunks). Currently not implemented; the conv-clique baseline papers over it for short docs. Add when porting attached docs if recall suffers.

### 4. **Decide whether RAPTOR is even available**

Some corpora don't have RAPTOR enrichment:
- **Vault**: per-note RAPTOR is overkill for a 5-paragraph note. Skip the RAPTOR layer; the chunk-co-occurrence + conv-clique layers alone work.
- **Wikipedia subsets**: corpus-wide RAPTOR over millions of chunks is impractical. Skip.
- **Attached docs**: RAPTOR IS the existing flow (`document_assets.raptor_nodes`). Use it; it's the conv's exact analogue.

`from_layered` with an empty `raptor_nodes` slice automatically degrades to GliNER-only. **No code branching needed at the call site.**

### 5. **Retrieval surface — same `rerank_conv_chunks_via_ppr` path**

Currently the runtime branch is conv-corpus-specific (matches on `display_category == "conversation"`). To extend to other corpora:
- Generalise the display-category check to any corpus with entity data
- OR add per-corpus rerank flags

The PPR + graph code itself is corpus-agnostic. The branching is purely about **which corpora to apply the rerank to**.

## When to skip which layer

| Scenario | Skip what | Why |
|---|---|---|
| Corpus has no LLM enrichment pipeline | RAPTOR | No `primary_entities` to feed; fall back to GliNER-only |
| Corpus too small for NER pass (<100 chunks total) | GliNER | One-shot extraction is overkill; conv-clique + RAPTOR alone is sufficient |
| Privacy-sensitive corpus (third-party content) | Both, fall back to T1 only | NER + cluster summary both expose entity names to the operator. T1 chunks + cosine is the privacy-preserving fallback |
| Model file unavailable | GliNER | Daemon already does this — logs informative warning, falls back to RAPTOR-only |
| First-install before NER finishes | GliNER (transiently) | The fallback path serves retrieval during the ~20-min extraction window; no UX gap |

## Test surface

23 unit tests in `conv_entity_graph.rs`. Coverage:
- Per-constructor: empty input, single-source, cross-source agreement weight accumulation
- Builder symmetry: `from_layered` with one empty arg = same as the single-source constructor
- PPR convergence: tight triangle, uniform walk, isolated entity reachability
- Chunk-mass projection: mass distributes across owning chunks
- Stem-bridge seeding: `satire ↔ satirical`, `economy ↔ economics`
- Conv-clique reachability: isolated singleton still receives mass from co-conv seed

When porting, add per-corpus integration tests that:
1. Build the graph from real corpus data
2. Verify entity density is in expected range (rough sanity: GliNER ≈ 5-25 per chunk per corpus)
3. Verify PPR returns positive mass on at least one chunk for at least one expected query

## Incremental update strategy (load-bearing for live corpora)

The one-shot CLI (`sovereign corpus extract-entities`) is right for **static imports** — claude.ai zip dump, vault snapshot, attached doc. For **live, growing corpora** — Sovereign-internal `conversation-history`, ongoing personal exports — one-shot leaves the table stale after every new write.

Three concrete strategies, ordered by latency vs infrastructure cost:

### Strategy A — Post-write hook (right answer for chat-history)

For corpora where every write is small + infrequent (one chunk per user message):
- At the chunk persist site (`ChunkWriter::write` in corpus-engine, OR the chat-message-write path in `sovereign-core/src/runtime.rs`), fire `GlinerExtractor::extract` on the new chunk content immediately after Lance persist succeeds.
- Persist via `save_chunk_entities` (the non-conv-grouped variant — works on single rows).
- Idempotent by `(corpus_id, chunk_id, text, label)` primary key — re-firing on the same chunk is safe.

```rust
// Pseudocode at the chunk persist site
let chunks = lance_writer.persist_batch(...).await?;
if let Some(extractor) = self.gliner.as_ref() {
    for chunk in &chunks {
        let mentions = extractor.extract(&chunk.content)?;
        let rows = mentions.into_iter()
            .map(|m| m.into_row(corpus_id, chunk.id, conv_uuid, now_unix()))
            .collect::<Vec<_>>();
        store.save_chunk_entities(&rows).await?;
    }
}
```

Cost: ~250ms CPU per new chunk (single message). Imperceptible for chat write latency since persist is already async.

### Strategy B — Background sweep

For corpora where writes are batched + sweep is acceptable (vault sync, watched folders):
- Background sweep every N minutes scans for chunks in Lance that are missing from `chunk_entities` for the given corpus.
- SQL: `SELECT chunk_id FROM lance_table WHERE chunk_id NOT IN (SELECT chunk_id FROM chunk_entities WHERE corpus_id = ?)`.
- Process in batches via `extract_batch`.
- Updates `chunk_entity_progress.state = "running"` while sweeping; back to "complete" when done.

Cost: N-minute staleness window. Cheap on large dormant corpora (sweep finds nothing); only pays when writes happened recently.

### Strategy C — Lazy on-query

For corpora where extraction is expensive OR write rate makes A/B impractical:
- At retrieval time, when `list_chunk_entities_for_conv` returns < expected count, fire extraction on missing chunks before building the graph.
- High first-query latency for newly-added content; subsequent queries hit cache.

Avoid unless A and B are both infeasible.

### Recommended assignment

| Corpus | Strategy | Why |
|---|---|---|
| `conversations-anthropic` | none — static | One-shot CLI was the right call; re-runs only on full re-import |
| `conversation-history` | **A — post-write hook** | Chat writes are one-at-a-time, low latency tolerance |
| `conversations-personal` | **B — background sweep** | Personal export sync is already batched, the sweeper exists in local-corpus reconcile |
| Vault | **B — background sweep** | Same local-corpus reconcile path |
| Attached docs | **A — post-write hook** | Per-doc ingest is one-shot, hook into the existing T1 completion site |
| Wikipedia subsets | none — static | Re-extract only on corpus-id replacement |

### Phase B status (2026-05-23)

- `conversation-history` + `conversations-personal` Phase A (backfill via CLI) shipped.
- Phase B incremental hook LANDED at the **engine-ingest** seam (not the
  per-message write site originally sketched). Rationale: every live conv
  corpus already re-enters `CorpusEngine::ingest` on each refresh —
  `conversation-history` via `KnowledgeViewManager::ingest_view` debounced
  by the conversation-touched observer, `conversations-personal` via the
  Settings → Imports re-import — so a single hook there covers both
  paths without the manager + sweeper each having to thread the GliNER
  handle. The hook fires unconditionally when the recipe declares
  `display.category = "conversation"`, runs `extract_delta_for_corpus`
  on the wired `ChunkEntityExtractor`, and flips
  `chunk_entity_progress.state` from `"complete"` to `"incremental"`.
- Concrete moves landed:
  1. `ChunkEntityExtractor::extract_delta_for_corpus` — new trait method
     with a no-op default (so RAPTOR-only paths don't have to opt in).
     `GlinerChunkExtractor` overrides; persists delta rows via
     non-destructive `save_chunk_entities` so prior conv data survives.
  2. `CorpusEngine::ingest_inner_with_skipset` calls the trait method
     post-`mark_ingestion_complete` for any non-unit-scoped ingest of a
     conversation-category recipe.
  3. `chunk_entity_progress.state = "incremental"` — extractor writes
     this once the first delta lands AND each subsequent re-ingest
     reconciles `chunks_total` against the current Lance set so the UI
     progress denominator tracks corpus growth.
  4. Desktop UI: `AtlasIndex.svelte` renders a dashed-border "Smart
     highlights — auto-updating" pill for the `incremental` state and
     drops out of the 5s poll loop (incremental updates land on ingest
     events, not on the tick).
- Open follow-ups:
  - **Watched-folder sweeper** (`local_corpus::watched::worker`) doesn't
    go through `CorpusEngine::ingest` — it writes via
    `apply_watched_diff` directly into LanceDB. Conv-category watched
    folders are rare (vault is `display.category = "personal"`, not
    conversation), so this seam stays unwired for now. The day a
    conv-shaped live watched folder lands, fire
    `extract_delta_for_corpus` from `Worker::run_sweep_body` after the
    `SweepCompleted` emission, gated on the same `display.category`
    check.
  - **Attached docs**: `display.category` is unset; the attached-doc
    ingest path needs its own hook (or a recipe category tag) before
    Phase B helps there.
  - **Bench coverage**: the conv-tiered bench's `--judge-trials` pass
    should A/B `extract_delta_for_corpus` against snapshot-only to
    confirm steady-state recall doesn't regress as the corpus grows.

## Honest known gaps

These apply to the conv port as shipped and to any future ports.

- **Same-surface-form, different-type collapse.** "Swift" (Person, Jonathan Swift) and "SWIFT" (Organization, financial messaging) become one graph node. Future fix: typed nodes — key by `(text_lower, label)` instead of `text_lower`. Cost: graph density drops; rerank may need typed-edge weighting tuning.

- **Stem-bridge is heuristic.** 4-char prefix match catches `satire ↔ satirical` but also bridges `manager ↔ management ↔ manage` even when only one is what the writer meant. PPR's seed-restart treats false-positive seeds as gentle additions, so the cost is recall noise rather than precision collapse — but it's a tunable per-corpus.

- **No cross-conv structural signal.** Each conv's graph is independent. For "show me clusters that recur across my conversations" queries, a corpus-wide motif aggregator is needed. Out of scope for the per-conv graph; tracked as future work in `CONV_TIERED_PORT.md`.

- **PPR weight default is unbenchmarked across question shapes.** `SOVEREIGN_CONV_PPR_WEIGHT = 0.25` was picked empirically on the Swift/Borges/cult-psych triplet. Different corpora + different question shapes may want different weights. Bench harness should A/B at 0.0/0.15/0.25/0.4 per new port.

- **Graph cache is per-query rebuild.** Each retrieval call rebuilds the per-conv graph from SQL reads. Sub-millisecond on the conv corpus (typically <100 entities); could become a hotspot on dense corpora. Add an LRU cache keyed on `(corpus_id, conv_uuid, last_extracted_at)` when the bench shows it matters.

## Reference implementation files

| Concern | File |
|---|---|
| Graph + PPR | `sovereign-core/src/conv_entity_graph.rs` |
| Persistence types + traits | `sovereign-core/src/conv_tiered.rs` |
| SQLite schema | `sovereign-store/src/migrations.rs::run_chunk_entities_migration` |
| SQLite read/write methods | `sovereign-store/src/sqlite.rs` |
| GliNER extractor | `sovereign-tools/src/gliner_ner.rs` |
| Ingest hook trait | `corpus-engine/src/enrichment/tiered.rs::ChunkEntityExtractor` |
| Daemon wiring | `sovereign-cli-daemon/src/daemon_cmd.rs` (around `with_chunk_entity_extractor`) |
| Retrieval rerank | `sovereign-core/src/runtime.rs::rerank_conv_chunks_via_ppr` |
| CLI surface | `sovereign-cli-llm/src/corpus_extract_entities_cmd.rs` |
| Desktop UI | `sovereign-desktop/src/lib/components/settings/ImportsTab.svelte` + `atlas/AtlasIndex.svelte` |
| Spec history | `sovereign/docs/specs/CONV_TIERED_PORT.md` |

## Decision log

| Date | Decision | Why |
|---|---|---|
| 2026-05-22 | Per-conv (not corpus-wide) RAPTOR | Semantic coherence; bounded briefing budget; incremental-friendly |
| 2026-05-22 | Conv-clique baseline (0.1) | Prevents isolated single-entity clusters from being PPR-invisible. Observed live on Swift query before this layer existed |
| 2026-05-23 | gline-rs over Python subprocess | Pure Rust, ships in desktop binary, single-binary distribution |
| 2026-05-23 | Threshold 0.6 (not 0.5) + drop `Concept` label | Validation showed 0.5 + Concept produced 35% noise on common nouns |
| 2026-05-23 | Layered (not mutually exclusive) | User question 2026-05-23 — RAPTOR + GliNER carry orthogonal signals; union dominates either alone |
| 2026-05-23 | "Same surface form = same node" (Swift/SWIFT collapse) | Simpler. Typed nodes a future refinement when bench shows the collapse hurts |

## Next ports

Suggested order:
1. **Attached docs** — already has RAPTOR; add GliNER over chunk text; reuse `chunk_entities` schema with `conv_uuid = asset_id`. ~half-day of wiring (no new schema).
2. **Vault notes** — skip RAPTOR (per-note clusters not useful at note size); use GliNER + conv-clique only. ~half-day.
3. **Wikipedia subsets** — skip RAPTOR (corpus too large); GliNER pass is feasible but ~5h of CPU on a million-chunk corpus. Decide per-subset.
4. **SEP** — likely wants RAPTOR (long articles, dense ideas) + GliNER. Match the conv port closely.
