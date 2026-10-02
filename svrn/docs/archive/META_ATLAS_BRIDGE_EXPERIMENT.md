# Meta-Atlas Bridge — Experiment Arc & Research Opportunities

**Status:** Parked (2026-06-17). Mechanism built, tested, and shipped *gated off*
(`SOVEREIGN_META_BRIDGE`, default disabled). End-user value proven only in the
**scoped/sealed** case; **not** justified as a retrieval feature for the common
unscoped cross-corpus query on current evidence. The defensible value is the
**explorable artifact** (a typed cross-corpus correspondence map for the Atlas
viewer), not retrieval augmentation.

---

## 1. What we set out to do

Promote the meta-atlas from name-equality `Entity` clustering to a **typed
topic-to-topic concept-alignment graph** between corpora (e.g. SEP ↔ Wikipedia),
so inference gets a *stereo* view — one corpus's *what* fused with another's
*why/contested* — across surface-form mismatch and granularity differences, plus
subsumption zoom. The thesis: richer cross-corpus connection and synthesis than
plain retrieval delivers. Held to whole-game validation (gate on QA value, never
on an alignment metric in isolation).

## 2. What we built (in the tree, gated off)

The mechanism is real, tested, and sound. It is *not* the question; the question
is whether it delivers end-user value.

- **`corpus-engine/src/meta_atlas/bridge/`** — corpus-agnostic typed alignment.
  `left`/`right` (driver/candidate) throughout; no corpus string literals.
  - `topic_node.rs` — `BridgeTopic` (concept text, entity keys, atom profile,
    articulation, embedding); built from a per-article atlas or a chunk hit.
  - `signals.rs` — graded `AlignmentSignal`s (NameMatch, Embedding,
    SharedEntities, LinkGraphCoNeighbor, ArticulationComplementarity,
    WikidataAnchor) → bands AutoSame / Uncertain / Drop (τ_same=0.70, τ_low=0.38).
  - `adjudicate.rs` — injected `AdjudicateFn`; LLM types the uncertain band as
    same / broader / narrower / related / different, with a persisted rationale.
  - `edges.rs` — typed `BridgeEdge` + reversible append-only oplog.
  - `build.rs` — `build_bridge` orchestrator, **resumable** (per-topic checkpoint
    + `bridge_progress.json`; `--fresh` rebuilds).
  - `lookup.rs` — `BridgeIndex` runtime lookup, keyed on titles **and**
    `left_entity_keys`, with a significant-token fallback and data-driven
    document-frequency stopword pruning (no hardcoded corpus knowledge).
- **Runtime consumption** — `Runtime::bridge_boost` (`runtime/retrieval.rs`),
  gated `SOVEREIGN_META_BRIDGE`, registered as `step_bridge_boost` in the shared
  retrieval pipeline. **Query-aware fetch**: blends the topic-anchor embedding
  with the live query embedding (`blend_query_aware` in `retrieval_helpers.rs`,
  `ANCHOR_WEIGHT=0.5`) so the cross-corpus pull is steered by the question.
- **CLI** — `sovereign meta-atlas align | explain | probe` (`meta_atlas_cmd.rs`).
  `explain` is the glassbox read: a concept's edges, relations, confidences,
  signals, rationales.
- **Validation** — 34 bridge unit tests + the blend helper tests; full workspace
  lint (22 crates) and test (6,759) green at park time.
- **Eval banks** — `sovereign/bench/cross-corpus/questions.toml` (8 dual-corpus
  Qs — see Finding 2, it is effectively SEP-only) and the existing
  `sovereign/bench/sep/questions.toml` (21 Qs).

## 3. The experimental arc (chronological, with the honest findings)

**Finding 1 — "byte-identical" was *inert*, not redundant.** The first SEP↔Wiki
A/B showed byte-identical retrieval OFF vs ON. Root cause via instrumentation:
`matched_entities=0` every question. The index was keyed only on concept *titles*,
but retrieval surfaces *people* (Kant, Gödel). Zero overlap → the bridge added
nothing → identical code paths. **Fixed** with `left_entity_keys` + token
fallback; proven to fire afterward.

**Finding 2 — SEP↔Wikipedia is a *redundant* pair.** SEP-only fact coverage 0.871
≈ both-corpora 0.869. Wikipedia contributed **zero** facts because SEP subsumes it
for philosophy. This is a property of the *pair*, not a bug — so SEP↔Wiki
structurally cannot demonstrate cross-corpus retrieval value, no matter how good
the bridge is.

**Finding 3 — the parametric-knowledge confound.** For famous content (canonical
law, mainstream philosophy) the model answers from training, so retrieval changes
don't move fact coverage. Measuring retrieval value requires content *outside*
training — private, obscure, or recent.

**Finding 4 — the one clean positive: scoped seal-break (Enron).** With retrieval
*sealed* to Wikipedia, a hand-seeded edge let the bridge reach an
`enron-sample-multi-wide` corpus of private emails. On a question about Enron's
2001 plan to rescue deregulation, the OFF run **abstained** ("I don't have
reliable information"); the ON run produced a **grounded** answer anchored on the
sealed internal email ("Enron's secret bid to save deregulation / PLAN TO RESCUE
DEREGULATION / Chairman pitches"). Honest caveats: (a) the answer was a *blend* —
the load-bearing *seed* came from the sealed corpus, the surrounding specifics
were public/parametric; (b) on a deliberately hyper-specific needle question the
bridge surfaced the right document but not the exact chunk, and the model
correctly **abstained rather than hallucinating** (the grounding gate held). The
value is real but specifically a property of **reaching otherwise-unreachable
content** — i.e. the scoped case.

**Finding 5 — query-aware fetch helped grounding, within a limit.** Blending the
query into the fetch tightened it (12→8 chunks) and surfaced a sealed `SF Gate`
news chunk the answer then *cited* — grounding shifted from parametric to an
attributed sealed source. But it could not separate near-duplicate chunks whose
embeddings are dominated by a shared repeated title-prefix (a *chunking* artifact
upstream of the bridge), so the needle question stayed unanswered (correctly).

**Finding 6 — the strategic realization (why this is parked).** Almost all real
end-user queries are unscoped cross-corpus. On that path, retrieval **already**
runs ANN across every enabled corpus, so cross-corpus synthesis already happens
without the bridge. The bridge-as-chunk-injector only adds value when the linked
content is **unreachable** (scoped — the Enron win, the minority case) or
**buried** by similarity dominance (unproven, and contradicted on SEP↔Wiki by
Finding 2). So as a retrieval feature for the case that matters, the value is
unproven and likely marginal.

## 4. Honest conclusion

- The **mechanism is sound and shipped** (gated off). Nothing here is broken.
- As a **retrieval feature for the common unscoped cross-corpus query**, it is
  **not justified** on current evidence. The only demonstrated value was the
  scoped/sealed minority case.
- The bridge-as-**injector** is probably the **wrong mechanism**. The one time
  structural knowledge actually moved a cross-corpus metric in this project was
  the **rerank experiment** (`atlas_weight` lifted SEP source coverage 40→65; see
  `RERANK_EXPERIMENT.md`) — a *structural prior in reranking*, not chunk injection.
- The **defensible, unique value is the explorable artifact**: a typed,
  rationale-bearing cross-corpus correspondence map. Modest, but real, and a
  different surface (the Atlas viewer) from retrieval.

## 5. Research opportunities

Ordered roughly by expected value-per-effort.

1. **The decisive complementary-pair test (do this first if chasing retrieval).**
   On a pair where corpus B genuinely holds what A lacks, measure whether *plain
   unscoped retrieval already surfaces B* or lets the query-dominant corpus bury
   it. Null hypothesis to beat: cheap per-corpus balancing / MMR diversification
   in the retriever (no edge set, no offline build). Only if plain retrieval
   *buries* B and balancing *doesn't* recover it is there a real gap for the
   bridge to fill. This single experiment settles "is there retrieval value here
   at all" before any more injector code is written.

2. **Bridge-as-rerank-prior, not injector.** Feed typed edges as a structural
   feature into `RerankConfig` (the proven lever) — boost chunks whose topic is
   bridge-linked to the query's topic — instead of injecting score-lifted chunks.
   Measure on the complementary pair from (1).

3. **The Atlas-viewer explorer layer (the artifact — most defensible value).**
   `AtlasGraph.svelte` already renders *typed* edges with a `crux` label (Tension
   fault-lines). Bridge edges are the missing **inter-corpus** dimension: a new
   `edge_type` per relation, the rationale as the crux, signals + confidence +
   Deterministic/Adjudicated provenance in the detail panel. A Tauri command reads
   the persisted `bridge_edges.json`; this is the visual sibling of the existing
   `meta-atlas explain` CLI. Seed data already exists (see §6, `.sep-partial`:
   22 typed edges). Note: SEP↔Wiki being *redundant for retrieval* does not make
   it *uninteresting to browse* — the type asymmetries (broader/narrower) are the
   substance.

4. **Subsumption zoom (broader/narrower).** The one cross-corpus move plain
   similarity structurally *cannot* make: a narrow query surfacing a stub, with
   the bridge pulling the broader argued treatment (or vice versa). Possibly the
   bridge's most defensible *retrieval* niche; untested. Needs broader/narrower
   edges (the build already produces them).

5. **Typed relation as a *synthesis* signal (not retrieval).** Tell the
   synthesizer "these two retrieved chunks are a `contested`/`broader`
   correspondence" so it can write "Wikipedia states X; SEP contests it." Uses the
   edge *type*, which neither retrieval nor reranking exploits today. Untested.

6. **Chunking artifact (orthogonal, but it bit us).** Chunks sharing a repeated
   title-prefix become embedding-near-duplicates, flattening needle retrieval
   (Finding 5). Strip repeated prefixes before embedding; consider a bridge fetch
   depth past the shared `CANONICAL_PRIMARY_LIMIT=3`.

## 6. Artifacts & how to resume

- **Code:** `corpus-engine/src/meta_atlas/bridge/`, `runtime/retrieval.rs`
  (`bridge_boost`), `runtime/retrieval_helpers.rs` (`blend_query_aware`),
  `runtime/retrieval_pipeline.rs` (`step_bridge_boost`), `meta_atlas_cmd.rs`.
- **Gate flag:** `SOVEREIGN_META_BRIDGE` (default off).
- **Edge artifacts** (`~/.svrnmesh/meta-atlas/`):
  - `bridge_edges.json` — the proven Enron seal-break edge (canonical).
  - `bridge_edges.json.sep-partial` — **22 typed SEP↔Wiki edges** (12 same,
    4 related, 3 broader, 3 narrower) over 20 SEP-bank topics. The explorer seed.
  - `.probebak` (17 earlier SEP edges, pre-entity-keying), `.legalbak`,
    `.enronbak`.
- **Rebuild / extend the edge set (resumable):**
  ```
  sovereign meta-atlas align --bank=sovereign/bench/sep/questions.toml \
    --k=6 --fresh --right=wikipedia
  ```
  ~50s/topic (bottleneck is ANN over the ~13 GB wikipedia chunk index +
  co-neighbor queries on the 2.4 GB link graph — a one-time offline build).
- **Glassbox read:** `sovereign meta-atlas explain "<concept>"`.
