# Enrichment — the canonical map

_What "enrichment" means in Commonwealth AI, and which of the three
systems any given corpus uses._

This is the umbrella doc. Two deep-dives sit under it and stay
authoritative for their own system: [`ENRICHMENT_V2.md`](./ENRICHMENT_V2.md)
(System 2 — Atlas) and
[`../sovereign/docs/TIERED_RETRIEVAL.md`](../sovereign/docs/TIERED_RETRIEVAL.md)
(System 3 — Tiered retrieval). Per `sovereign/ARCH_PRINCIPLES.md §1.1`
this file is a contract — every path, table, and CLI claim below
resolves against the code on the commit it appears in.

---

## TL;DR — three systems, one selector

"Enrichment" is the **build-time** pass that turns indexed chunks into
*structure* a retriever or the synthesis prompt can exploit. The word
denotes **three different systems**, chosen per-corpus by a single
field — `[enrichment] type` in the recipe TOML (or, for non-recipe
corpora like attached documents, by the ingest path that runs them):

| `type` | System | One-liner | Deep-dive |
|---|---|---|---|
| `field_model` | **System 1 — Field Model (v1)** | 5-phase *holistic, whole-corpus* field analysis (skeleton → cluster → align → fault-lines → open-questions). `Domain` trait. | this doc |
| `atlas` | **System 2 — Atlas (v2)** | LLM-driven **typed atom graph** (Entity/Claim/Event/Question/…) *per document*. `Pipeline` trait. Writes `atlas/atoms.json`. | [`ENRICHMENT_V2.md`](./ENRICHMENT_V2.md) |
| `tiered` | **System 3 — Tiered retrieval (RAPTOR + GLiNER)** | 3 progressive tiers: T1 embeddings → T2 entity-graph + PPR → T3 **RAPTOR** cluster tree. SQLite-backed. **The gold standard for user-facing corpora.** | [`TIERED_RETRIEVAL.md`](../sovereign/docs/TIERED_RETRIEVAL.md) |

**Dispatch:** `corpus-engine/src/engine/ingest.rs:1581` branches on
`enrichment_config.enrichment_type == "tiered"`; otherwise the
`field_model` / `atlas` path runs. Schema: `EnrichmentConfig` in
`corpus-engine/src/recipe.rs`.

**The three are not a version ladder — they coexist by design.** A
single corpus can even run two (SEP runs `atlas` per-article *and*
retains `field_model` full-corpus parameters — see the matrix). Pick the
system by what the corpus is *for*, not by recency.

---

## System 1 — Field Model (v1) · `type = "field_model"`

- **Code:** `corpus-engine/src/enrichment/field_engine.rs` +
  `enrichment/domains/`.
- **Extension point:** the `Domain` trait + `DomainRegistry`
  (`enrichment/domain.rs`, `enrichment/domain_registry.rs`). One impl
  per knowledge field; all domain knowledge is prompts + config.
- **Phases (holistic, run over the *whole* corpus at once):** skeleton
  extraction (canonical questions from overview chunks) → HDBSCAN
  clustering + LLM cluster-labelling → optional Phase-1b entity
  extraction → alignment (questions ↔ cluster labels) → fault-lines
  (conceptual tensions between clusters) → open-questions (gaps in the
  skeleton).
- **Registered domains:** `philosophy, science, policy, legal,
  community, multi, engineering` (public) + `personal, conversational,
  business_email, institutional` (KnowledgeView).
- **Where it's live today:**
  - **KnowledgeView** landscape digests (personal / conversational /
    institutional domains), driven from
    `sovereign-tools/src/knowledge_view/debouncer.rs`.
  - The **full-corpus SEP "epistemic-research" flow**
    (`sovereign corpus build sep`) and `gutenberg-work`.
- **Status:** legacy but **still load-bearing** — not dead, and not the
  default for new typed work. New typed-graph work goes to System 2.

---

## System 2 — Atlas (v2) · `type = "atlas"` · deep-dive [`ENRICHMENT_V2.md`](./ENRICHMENT_V2.md)

- **Code:** `corpus-engine/src/enrichment/atlas/` +
  `enrichment/pipeline/`.
- **Extension point:** the `Pipeline` trait + `PipelineRegistry::builtin`
  (`enrichment/pipeline/registry.rs`). Domain knowledge lives in
  **markdown prompt assets**, not Rust branches — a new domain is a new
  prompt set + a one-line registry entry.
- **Registered pipelines (6):** `literary`, `literary_atlas`,
  `philosophy_atlas`, `referential_atlas`, `engineering_atlas`,
  `conversation_atlas` (`registry.rs:28-51`). The former `obsidian_atlas`
  was **removed** when the vault port moved vaults onto System 3 — vault
  corpora now route through the folder tiered provider, not this
  registry (operators wanting bench-scorable `atoms.json` for a vault
  pass `--pipeline literary_atlas` explicitly).
- **Phases (LLM-driven, *per document*):** seed (canonical entity list,
  threaded into every later prompt) → extract (per-section typed-atom
  sketches) → cluster (by facet) → name → resolve (3a entities/events +
  3b states/relations/claims; alias-merge, Levenshtein + cosine, salience
  tiebreak) → tensions (deterministic candidates + optional LLM
  classifier) → gaps (ungrounded claim / transition-without-trigger /
  open question) → configuration (opt-in interpretive readings).
- **Atom types:** Entity, State, Relation, Event, Claim, Question,
  Configuration, **Asset** (+ Gap-B typed extensions:
  ArgumentReconstruction, Position, Opposition). **Edge types:** Involves,
  Transition, Causes, Grounds, Tension, Composes, Configures, **Attaches**.
  Cross-corpus **Grounding** edges bridge two atlases.
- **On disk:** `<corpus>/atlas/{atoms.json, edges.json, trajectories.json,
  gaps.json, tensions.json, cross_corpus_edges.json, configurations.json}`
  (`SCHEMA_VERSION 2.2`).
- **CLI:** `sovereign enrich {init,seed,extract,cluster,resolve,tensions,
  gaps,configure,build,query,report,review,bridge}` —
  `sovereign-cli-llm/src/enrich_cmd/`. `query` is a **zero-LLM** traversal
  + brief assembler; `report` / `review` are the spec §12
  schema-validation surfaces.
- **Where it's live:** SEP (`philosophy_atlas`, per-article),
  `enron-sample` (`conversational` domain + Phase-4 multi-origin
  reconciliation), the literary corpora.

---

## System 3 — Tiered retrieval (RAPTOR + GLiNER) · `type = "tiered"` · deep-dive [`TIERED_RETRIEVAL.md`](../sovereign/docs/TIERED_RETRIEVAL.md)

**This is the gold standard** the most-benched, user-facing corpora use:
**attached documents, conversation history, and Obsidian + watched
folders.** It is corpus-agnostic and *progressive* — the user can query
within seconds (T1) and quality climbs as later tiers land.

**The three tiers (additive — a T3 query layers all three signals):**

- **T1 — embeddings.** Chunk vectors; cosine top-K. (~1.5 min on a
  1000-chunk doc.)
- **T2 — entity graph + PPR.** Per-chunk entity extraction → entity
  co-occurrence graph → Personalized PageRank (HippoRAG-1-style
  multi-hop) seeded from query entities. (~6 min.)
- **T3 — RAPTOR atlas.** K-means leaf clusters → recursive LLM
  summarisation tree (until root ≤ 4) → TF-IDF motifs → TextTiling
  segments → overview. Signposts + motifs feed the briefing. (~12 min.)

### The single RAPTOR builder + the dependency-injection seam

There is **exactly one** RAPTOR implementation —
`sovereign-tools/src/raptor_atlas.rs` (`build_raptor_atlas` /
`build_and_persist_raptor_atlas`). `corpus-engine` must **not** depend on
`sovereign-tools` (that edge would be cyclic), so the builder is
**injected**:

1. `corpus-engine` defines the `TieredEnrichmentProvider` trait
   (`enrichment/tiered.rs:108`).
2. The daemon calls `CorpusEngine::with_tiered_provider(Arc<dyn …>)`
   at startup (`engine/mod.rs:602`).
3. The concrete impl —
   `sovereign-tools/src/conv_tiered_provider.rs::ConvTieredProvider` —
   wraps the RAPTOR builder + the entity graph.
4. `ingest.rs` dispatches per-source-doc to the provider via
   `run_tiered_enrichment` (conversations, grouped by `conv_uuid`,
   `tiered.rs:225`) or `run_folder_tiered_enrichment` (watched
   folders / vaults, grouped per file, `tiered.rs:400`).

This is the canonical example of `ARCH_PRINCIPLES.md §5.4` (pipeline
stages parameterize on data, not source identity) — the same builder
serves attached docs, conversations, and vaults.

### GLiNER — a real model, not a nickname

`gline-rs` v1 + `orp` 0.9 (ONNX runtime), feature-gated `gliner-ner` in
`sovereign-tools/Cargo.toml`, enabled by the daemon. Loads
`gliner_small-v2.1` (~150 MB) from `~/.sovereign/models/gliner/`; module
`sovereign-tools/src/gliner_ner.rs`.

**Scope today:** GLiNER runs per-chunk NER on the **conversation** path,
*layered on top of* RAPTOR's cluster-summary `primary_entities`
(`sovereign-core/src/conv_entity_graph.rs::from_layered` merges the two
orthogonal signals — RAPTOR captures cluster-scale distinctiveness,
GLiNER captures raw per-chunk NER). The **document-asset** T2 still uses
the `Speed::Slow` LLM + `lark_grammar` extraction in
`document_asset.rs::build_skeleton` — GLiNER has not (yet) replaced it
there.

### Entity-aware hybrid retrieval scorer (conversation history)

In `sovereign-core/src/runtime/retrieval.rs`:
`0.6·cosine + 0.4·jaccard(entity-overlap)` (`HYBRID_COSINE_WEIGHT` /
`HYBRID_JACCARD_WEIGHT`), then **MMR** (λ = 0.5) for diversity, with
`topic_context` query enrichment (`context.rs::update_topic_context`, a
Fast-slot classifier appends `[topic:…][domain:…]` before embedding).

**Default-ON since 2026-05-26** (the `marathon_graceful` spike outcome):
`maybe_retrieve_relevant_history` runs unless
`SOVEREIGN_HISTORY_RETRIEVAL=0` is set to disable it for A/B compares
(`retrieval.rs:2942-2944`). When GLiNER isn't loaded it falls back to
pure cosine. _(Note: the function's own docstring at `retrieval.rs:2929`
still says "gated on =1 for the spike phase" — that line is stale; the
code below it is the truth.)_

### Storage (SQLite — `sovereign-store/src/migrations.rs`)

One RAPTOR builder, three tables: `raptor_nodes` (attached documents,
`run_raptor_atlas_migration`), `conv_raptor_nodes` (per-conversation),
plus a vault-wide themes table feeding the `vault_themes` briefing
section (`conv_tiered.rs` + `conv_briefing.rs`). All cascade-delete from
their owner. Optional T3 re-rank: the cluster-score blend
(`SOVEREIGN_DOC_CLUSTER_WEIGHT`), default `0.0` (byte-identical baseline).

---

## The "atlas" name collision (read this so you don't get confused)

Two unrelated mechanisms share the word **atlas**:

- **System 2's atlas** = a *semantic typed-atom graph* (`atoms.json` —
  Entity/Claim/Event/…).
- **System 3's "RAPTOR atlas"** = a *structural summarisation tree*
  (`raptor_nodes` — cluster summaries + embeddings).

They do **not** interoperate: RAPTOR nodes never become atoms, and atoms
never seed RAPTOR clusters. Same word, two different worlds.

---

## Enrichment vs retrieval are orthogonal — and retrieval differs by storage backend

Enrichment is build-time; retrieval is query-time. *Which* retrieval
composition runs depends on **where the corpus is stored**, not just how
it was enriched:

- **LanceDB corpora** (SEP, wikipedia, enron — the recipe-ingested ones):
  retrieval = vector cosine (LanceDB IVF-PQ) + Tantivy FTS, plus — for
  `atlas`-enriched corpora — System 2's atlas traversal / claim-search.
  These corpora do **not** have `raptor_nodes`.
- **SQLite asset corpora** (attached docs, conversations, vaults):
  retrieval = the System 3 tiered stack (T1 cosine + T2 PPR + T3 RAPTOR
  signposts), plus the hybrid entity scorer for conversation history.
  This is where GLiNER + RAPTOR actually feed query results.

---

## Corpus → system matrix (verified against recipes)

| Corpus | `[enrichment] type` | Enrichment system(s) | Retrieval | Source |
|---|---|---|---|---|
| **SEP** | `atlas` (`philosophy_atlas`) **+ retains field_model params** | **System 2 + System 1** | LanceDB cosine + FTS + atlas traversal | `sovereign-recipes/sep/recipe.toml` |
| **enron-sample** | `atlas` (`conversational`) + reconciliation | System 2 + Phase-4 reconciliation | LanceDB cosine + FTS | `sovereign-recipes/enron-sample/recipe.toml` |
| **gutenberg-work** | `field_model` (`literary`) | System 1 | LanceDB cosine + FTS | `sovereign-recipes/gutenberg-work/recipe.toml` |
| **conversations-anthropic** | `tiered` (`conversational`) | **System 3 (RAPTOR + GLiNER)** | T1+T2+T3 + hybrid scorer | `sovereign-recipes/conversations-anthropic/recipe.toml` |
| **attached documents** | `tiered` (no recipe) | System 3 | T1+T2+T3 | `sovereign-tools/src/document_asset.rs` |
| **Obsidian / watched folders** | `tiered` (no recipe) | System 3 (folder variant + `vault_themes`) | T1+T2+T3 + vault themes | `sovereign-tools/src/local_corpus/` + `tiered.rs::run_folder_tiered_enrichment` |

---

## Two truths worth stating plainly

1. **SEP is *not* a "RAPTOR + GLiNER" corpus.** Its production enrichment
   is System 2 (`atlas` / `philosophy_atlas`) plus retained System 1
   full-corpus parameters (the recipe says so verbatim: _"Atlas (v2,
   per-article) and field-model (v1, full-corpus) coexist — different
   surfaces, same source parquet."_). SEP is LanceDB-backed and has no
   `raptor_nodes`. The association of SEP with the tiered RAPTOR stack
   traces to the **archived** `sovereign/docs/archive/RERANK_EXPERIMENT.md`
   (the `atlas_weight` blend that lifted SEP sources 40 → 65) — an
   experiment, not the shipped retrieval path. The gold-standard
   RAPTOR + GLiNER corpora are **conversation, attached-doc, and
   Obsidian / folder-watch**.

2. **GLiNER is currently conversation-scoped.** It is a real ONNX model
   (above), but it augments the **conversation** entity graph today; the
   attached-document T2 still extracts entities with a grammar-constrained
   LLM call. Don't assume "tiered ⇒ GLiNER everywhere" yet.

---

## Reading order for a new contributor

1. **This document** — which system is which, and how a corpus selects one.
2. The selector: `corpus-engine/src/recipe.rs::EnrichmentConfig` +
   `engine/ingest.rs:1581` (the dispatch branch).
3. The deep-dive for the system you're touching:
   [`ENRICHMENT_V2.md`](./ENRICHMENT_V2.md) (atoms) or
   [`TIERED_RETRIEVAL.md`](../sovereign/docs/TIERED_RETRIEVAL.md) (RAPTOR/GLiNER).
4. The injection seam if you're on tiered:
   `enrichment/tiered.rs` (trait) →
   `sovereign-tools/src/conv_tiered_provider.rs` (impl) →
   `sovereign-tools/src/raptor_atlas.rs` (the builder).
