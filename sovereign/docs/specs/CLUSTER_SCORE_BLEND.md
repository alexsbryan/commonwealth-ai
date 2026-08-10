# Spec: RAPTOR cluster-score as a retrieval blend term

**Status:** Shipped 2026-05-22.
**Lifecycle:** Spec preserved for design-rationale forensics
(failure-mode analysis + bench plan + choice-vs-alternatives); the
runtime surface is described in
[`../TIERED_RETRIEVAL.md`](../TIERED_RETRIEVAL.md) under
"Cluster-score blend (optional T3 re-ranking)."

**Prerequisites for reading:** [`../TIERED_RETRIEVAL.md`](../TIERED_RETRIEVAL.md)
for the wider architecture; [`../archive/RERANK_EXPERIMENT.md`](../archive/RERANK_EXPERIMENT.md)
for the structural pattern this proposal descends from.

## Why this exists

The book-report bench (May 2026 runs) consistently lost on two classes of attached-doc questions where the failure is *structural retrieval*, not synthesis:

- **T1 `winnie_fate` swings wildly (0% / 20% / 40% / 100% mech across consecutive runs)** — the answer is chunk 957 (Ossipon-reads-newspaper, an epilogue chunk with low central-entity density). PPR re-ranking *demoted* it in Run 6 because the entity graph diffused mass toward heavy-co-occurrence chunks elsewhere; cosine alone surfaces it only by luck on certain query phrasings.
- **T3 / T4 / T5 synthesis questions plateau at 0/5 judge on anti-canonical questions** — the model retrieves *some* canonical-scene chunks but rarely the *right neighborhood*. There's no retrieval-time signal that says "the answer is in the document's ending neighborhood" or "the answer is in the cluster about the Professor's deflation."

Both failure modes share a shape: **cosine knows which chunks resemble the query but nothing about which structural neighborhood the chunks belong to**. We have that information sitting unused in `raptor_nodes` — every chunk belongs to a leaf cluster, and that cluster has a `summary_embedding` capturing what the *neighborhood* is about. We just never use it at retrieval time.

The rerank experiment found the same shape on SEP (canonical-vs-tangential articles), and their `atlas_weight` blend term — `final = α·rerank + (1-α)·fusion + atlas_weight·atlas_norm` — captured +3 sources / 0-fact-regression on the validated config. This proposal is the attached-doc analogue.

## The mechanism

After the existing cosine top-K + PPR recall-boost pass in `attached_document_search.rs::execute`, but before the final truncate, compute per-chunk cluster_score:

```text
for each chunk c in the candidate pool:
    let leaf_node = raptor_node_containing(c)       // via direct_member_chunk_ids
    let cluster_score(c) = cosine(query_embedding, leaf_node.summary_embedding)

normalise cluster_score across the candidate pool via min-max → cluster_norm ∈ [0,1]
normalise cosine_score similarly → cosine_norm ∈ [0,1]

final_score(c) = (1 - cluster_weight) · cosine_norm(c) + cluster_weight · cluster_norm(c)

sort by final_score descending
truncate to top-16 (same as today)
proceed to ±1 neighbour expansion as today
```

Three things to notice:

1. **Cluster_weight is a single scalar knob.** Default 0.0 means behaviour-byte-identical to today's path. Recommended starting point: 0.25 (matches the rerank experiment's empirical-best `atlas_weight = 0.5` scaled for our different pool dynamics — see "Tuning notes" below).
2. **PPR is not in the blend.** PPR's contribution stays as the *recall-boost* (adding chunks beyond cosine top-K). The blend operates on the union of cosine top-K and PPR-boosted chunks.
3. **The chunk→cluster mapping is precomputed once per turn.** Build a `HashMap<u32, usize>` (chunk_id → raptor_node_index) from `direct_member_chunk_ids` at the top of `execute()`, reuse across all candidates.

## File map (concrete edits)

| File | Lines (approx) | Change |
|---|---|---|
| `sovereign-tools/src/attached_document_search.rs` | ~226-360 (the cosine + PPR block) | The main edit. See "Implementation order" below. |
| (no schema change) | — | `raptor_nodes` table already has `summary_embedding BLOB` and `direct_member_chunk_ids TEXT` populated by `build_and_persist_raptor_atlas`. |
| (no migration) | — | |

### Implementation order (recommended)

1. **At the top of `execute()`, after fetching the asset, also fetch raptor leaf nodes.** Add a call `store.list_raptor_nodes(asset_id).await` → filter to `level == 0` (leaf nodes only — these have populated `direct_member_chunk_ids`).
2. **Build the chunk→cluster lookup.** `HashMap<u32 chunk_id, usize node_idx>`. Iterate leaf nodes; for each chunk_id in `direct_member_chunk_ids`, insert the mapping.
3. **Compute cluster_score for each candidate.** Two ways:
    - For each chunk in `scored` (the existing `Vec<(f32, usize)>` of cosine results), look up its cluster, cosine the query_embedding against `leaf_node.summary_embedding`. Cache the per-cluster score so chunks in the same cluster don't redo the cosine.
4. **Min-max normalise** both cosine and cluster scores across the candidate pool (treat each `Vec<f32>` independently).
5. **Blend** per the equation above. Env var `SOVEREIGN_DOC_CLUSTER_WEIGHT` overrides the default; clamp to `[0.0, 1.0]`. When `cluster_weight == 0.0` and no env var, **early-return without computing cluster_score** — preserves byte-identical baseline behaviour.
6. **Sort by `final_score`, truncate to 16** (unchanged from today).
7. **±1 neighbour expansion** runs on the new ranked-top-K (unchanged from today).
8. **Narration log line.** Add a `tracing::debug!` after the blend with `cluster_weight`, number of chunks where cluster_score changed the rank, and the top-1 cluster's primary_entities. Useful for diagnosing on bench runs.

## Data already available

Everything this proposal needs is already persisted by the T3 phase of the tiered ingest pipeline (shipped 2026-05-22):

- `raptor_nodes.summary_embedding` — leaf cluster summary embeddings, same dimensionality as query embeddings (both go through the daemon's embed model)
- `raptor_nodes.direct_member_chunk_ids` — JSON array of chunk indices belonging to each leaf cluster
- `raptor_nodes.level == 0` filter selects only leaf clusters
- `raptor_nodes.primary_entities` — useful for narration log, not for scoring

The asset is at `MultiHopReady` before raptor_nodes exists; the blend should gracefully skip cluster scoring when `list_raptor_nodes` returns empty (which is the case at PartiallyReady or MultiHopReady states — fall back to pure cosine + PPR recall-boost). At `Ready`, the leaf nodes are guaranteed populated.

## Env vars + defaults

| Var | Default | Effect |
|---|---|---|
| `SOVEREIGN_DOC_CLUSTER_WEIGHT` | `0.0` | `0.0` → blend disabled (baseline). `0.25` → recommended starting point. Range `[0.0, 1.0]`. |
| `SOVEREIGN_DOC_PPR` | `on` | (existing) leave alone — PPR recall-boost composes orthogonally. |
| `SOVEREIGN_DOC_PPR_BOOST` | `6` | (existing) leave alone. |

A second knob worth supporting: `SOVEREIGN_DOC_CLUSTER_POOL` to widen the cosine candidate pool before the blend (matches the rerank experiment's k=200 finding that cluster-aware signals only earn their keep on wider pools). Default `16` (today's value) for backward compat; try `32` or `48` in the tuning sweep.

## Failure modes to watch

The rerank experiment's playbook applies almost verbatim here:

1. **Tangential-but-dense clusters out-scoring canonical ones.** A scene the document discusses densely in cosine-similar vocabulary can dominate the cluster_norm. *Diagnostic:* if T1 winnie_fate regresses when the blend is on, the ending cluster (chunks ~900-1006) is losing the cluster_score competition. Check the narration log for which cluster won the top spot.
2. **PartiallyReady / MultiHopReady regression.** Before T3 completes, `list_raptor_nodes` returns empty. The code path must gracefully skip the blend rather than dividing by zero or panicking. Verify with a unit test that fires `execute()` on an asset with no raptor_nodes.
3. **Cluster pool too narrow at top-16.** Per the rerank experiment's Part A finding, signals on the candidate pool don't earn their keep until the pool is wide enough for re-ordering to matter. If `cluster_weight=0.25` shows zero effect on the diagnostic triplet at pool=16, widen to pool=32 before declaring "the blend doesn't help."
4. **The pretraining-ghost failure on T1.** The model on T1 winnie_fate sometimes invents non-existent Conrad scenes (axe murders, wedding rings) — those are model-level confabulation, not retrieval failure. Cluster_score blend doesn't address them and can't be expected to.

## Verification

### Functional

- `cargo test --package sovereign-tools --lib --features corpus-engine/treesitter attached_document_search::` — unit tests for the new code path. Suggested coverage:
  - Blend disabled (`cluster_weight=0.0`) → ordering byte-identical to baseline
  - Blend enabled but no raptor_nodes → graceful fall-through to baseline
  - Blend enabled with raptor_nodes → ordering changes when cluster_norm dominates cosine_norm
  - Min-max normalisation: pool with all-equal cosine scores doesn't divide by zero

### Bench (the load-bearing measurement)

Use the existing `bench book-report --reuse-asset` path on the canonical Conrad asset:

1. **Diagnostic triplet, baseline-vs-blend A/B.** Run `--questions winnie_fate,stevie_circles_to_winnies_eyes,professor_menace_vs_impact` with `SOVEREIGN_DOC_CLUSTER_WEIGHT=0.0` then `=0.25`. Compare mech + judge scores. The hypothesis: blend lifts T1 winnie_fate (epilogue cluster surfaces), lifts T3 stevie_circles (motif-recurrence cluster surfaces), neutral or slight lift on T5 professor_menace.
2. **Regression guard.** Run `--questions verloc_double_role,professor_perfect_detonator,winnie_incurious_motif`. The hypothesis: T2 questions are already strong; blend shouldn't hurt them. If they regress, the blend is over-promoting wrong clusters.
3. **Tuning sweep.** If diagnostic triplet shows signal, sweep `cluster_weight ∈ {0.15, 0.25, 0.40, 0.60}` against the full 20-question bench. Pick the Pareto point (mech + judge balanced; the rerank experiment's k=200 / atlas_weight=0.5 finding suggests a similar mid-range optimum for us).

Per-question deltas — not aggregate scores — are what to look at, since single-run variance on this bench is ~15-20 points per tier (see earlier session memos).

### Honest expectations

Per the analysis in `RERANK_EXPERIMENT.md` (the SEP-vs-wiki divergence on `atlas_weight`), structural blend signals are **corpus-shape-specific**. For a literary novel like Conrad, where the chunker produces ~1006 chunks across 50 leaf clusters of ~20 chunks each, the cluster_score signal is *informative* (different clusters genuinely mean different scenes) but *not overwhelming* (the answer chunk for a question can sometimes live in a small dedicated cluster, sometimes in a larger thematic one). Expect:

- T1 winnie_fate: **plausible +20 to +60 mech points** if the ending cluster wins on `cluster_weight=0.25`. The volatility today is from cosine alone occasionally surfacing chunk 957 by luck; the blend should make that less luck-dependent.
- T3 stevie_circles: **plausible +1-2 judge points** if the motif-recurrence cluster surfaces. Same mechanism — the blend gives the right neighborhood a boost.
- T5 anti-canonical: **probably no movement** — these fail at *synthesis*, not retrieval (per the analysis in TIERED_RETRIEVAL.md). The cluster blend gives the model the right chunks but doesn't change what the model does with them. Expect neutral or marginal.

If the diagnostic triplet shows zero movement, the next debug step is to confirm the raptor_nodes are populated and the chunk→cluster mapping is being built correctly. The most likely silent-no-op cause is `level == 0` filtering returning an empty list because RAPTOR's tree on Conrad sometimes produces only mid-level + root (the recursion shape is 50 leaves → 10 mid → 3 root; `level == 0` should match the 50 leaves, but verify with `sqlite3 ~/.svrnmesh/sovereign.db "SELECT level, COUNT(*) FROM raptor_nodes GROUP BY level"` before declaring a code bug).

## Open questions to pin before shipping

1. **Should mid-level cluster summary embeddings also contribute?** A chunk belongs to ONE leaf cluster but multiple ancestors in the RAPTOR tree. A mid-level cluster's `summary_embedding` represents a thematic neighborhood that may score the query better than the leaf. Possible extension: `cluster_score(c) = max(cosine(query, ancestor.summary_embedding) for ancestor in path-from-leaf-to-root)`. Probably worth measuring once after the leaf-only baseline lands.
2. **What about the briefing-side render?** Today the briefing surfaces mid-level RAPTOR nodes with their evidence chunk ranges. If retrieval starts using cluster_score, the briefing might want to surface *which clusters were retrieved from* in the chunk metadata (helps the model understand neighborhood structure). Not a hard prerequisite for shipping the blend — additive polish.
3. **Should the blend be doc-type-aware?** Narrative documents have different cluster shapes than technical documents. The rerank experiment ended up with `dedup_corpus_filter` for the same reason. Probably not worth doing for v1; revisit if the swiss-army-knife use cases (conversations, Obsidian, SEP) show divergent tuning curves.

## Suggested commit shape

- Commit 1: precompute chunk→cluster map at the top of `execute()`; no scoring change. Pure refactor; verifies the lookup builds correctly on real assets.
- Commit 2: cluster_score computation + blend math + env var. Default off — byte-identical baseline.
- Commit 3: unit tests covering the four failure modes above.
- Commit 4: docs — add a "Cluster-score blend" subsection to `TIERED_RETRIEVAL.md` under "What's wired today," update the env var table.
- After bench validation: a fifth commit to flip the default `cluster_weight` from `0.0` to the empirically-best value if the sweep produces a clear winner.

## What to read first (cold pickup order)

1. This document.
2. `sovereign/docs/TIERED_RETRIEVAL.md` — the wider architecture context. Especially the "On HippoRAG 1 vs 2" section which explains why structural retrieval signals are the right next move, and the "Quality on synthesis tiers... didn't improve" section which calls out the specific failures this proposal targets.
3. `sovereign/docs/RERANK_EXPERIMENT.md` — the structural-signal blend pattern in detail, with measured outcomes on SEP/wiki that inform the tuning expectations here.
4. `sovereign-tools/src/attached_document_search.rs::execute` (the function this proposal modifies) — read end-to-end so you see the cosine + PPR composition this slots into.
5. `sovereign-core/src/types.rs::RaptorNode` — the data type the cluster_score reads.
6. `sovereign-tools/src/raptor_atlas.rs::build_raptor_atlas` — where `summary_embedding` and `direct_member_chunk_ids` come from. Helpful for understanding the data shape but not strictly required.

Pickup should be ~15 minutes of reading + ~1-2 hours of focused implementation + a bench-validation cycle (~30 min if `--reuse-asset` is used on the canonical Conrad asset).
