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

Forward-looking work lives one layer up:
[`ENRICHMENT_ROADMAP.md`](./ENRICHMENT_ROADMAP.md) (2026 frontier review
+ best-in-class plan) — intent per §1.2, not a contract.

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
| _(verb, not a `type`)_ | **System 4 — Code intelligence** | Per-**symbol** intent summaries over a SCIP-indexed *code* corpus + a SCIP call-graph trace injected into chat evidence (the `CodeQuery` route). Plain-English question → right symbol → callers/callees. | [`CODE_INTEL_CHAT.md`](../sovereign/docs/specs/CODE_INTEL_CHAT.md) |

**Dispatch:** `corpus-engine/src/engine/ingest.rs:1581` branches on
`enrichment_config.enrichment_type == "tiered"`; otherwise the
`field_model` / `atlas` path runs. Schema: `EnrichmentConfig` in
`corpus-engine/src/recipe.rs`. **System 4 is the exception** — it is not
selected by `[enrichment] type` at ingest; it is a separate pass run by the
`sovereign enrich code-intel <corpus>` verb against a corpus that already has a
SCIP graph (`scip_graph.db`), and unlike Systems 1–3 it has a load-bearing
*retrieval-time* half (the `CodeQuery` route), not only a build-time one.

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
- **Registered domains:** `philosophy` (public) + `personal,
  conversational, business_email, institutional` (KnowledgeView). Only
  fully-implemented domains are registered — `todo!()` stubs (science,
  policy, legal, community, multi, engineering) were removed so a
  `--domain` selection errors cleanly instead of panicking.
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
  (`SCHEMA_VERSION 2.4`; since 2.4 Relation / Event / Claim carry
  `attributes` and Claim a `subject`, all default-empty).
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
`gliner_small-v2.1` (~150 MB) from `~/.svrnmesh/models/gliner/`; module
`sovereign-tools/src/gliner_ner.rs`.

**Scope today:** GLiNER runs on **both** the conversation and
document-asset paths. On the **conversation** path it runs per-chunk
NER *layered on top of* RAPTOR's cluster-summary `primary_entities`
(`sovereign-core/src/conv_entity_graph.rs::from_layered` merges the two
orthogonal signals — RAPTOR captures cluster-scale distinctiveness,
GLiNER captures raw per-chunk NER). On the **document-asset** T2,
`document_asset.rs::build_skeleton` now tries the same
`LazyGlinerExtractor` first, off the executor, for zero-LLM-token
extraction (`document_asset.rs:1814-1831`); only when the extractor is
absent or returns nothing does it fall back to a `lark_grammar`-enforced
LLM call routed via `Workload::EnrichBulk` (Fast-class, not
`Speed::Slow`) (`document_asset.rs:1858-1893`).

### Entity-aware hybrid retrieval scorer (conversation history)

In `sovereign-core/src/runtime/retrieval/history.rs` (moved here by the
`runtime/retrieval.rs` module split):
`0.6·cosine + 0.4·jaccard(entity-overlap)` (`HYBRID_COSINE_WEIGHT` /
`HYBRID_JACCARD_WEIGHT`, `history.rs:602-603`), then **MMR** (λ = 0.5)
for diversity, with `topic_context` query enrichment
(`context.rs::update_topic_context`, a Fast-slot classifier appends
`[topic:…][domain:…]` before embedding).

**Default-ON since 2026-05-26** (the `marathon_graceful` spike outcome):
`maybe_retrieve_relevant_history` (`history.rs:431`) runs unless
`SOVEREIGN_HISTORY_RETRIEVAL=0` is set to disable it for A/B compares
(`history.rs:444`). When GLiNER isn't loaded it falls back to pure
cosine. _(Note: the function's own docstring at `history.rs:423`
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

## System 4 — Code intelligence · `sovereign enrich code-intel` · deep-dive [`CODE_INTEL_CHAT.md`](../sovereign/docs/specs/CODE_INTEL_CHAT.md)

**The conceptual→code bridge.** Lets a user ask a plain-English question of a
SCIP-indexed codebase ("how does inference run", "what calls `gate_answer`",
"where is X implemented") and get a code-level answer grounded in the
compiler-resolved call graph — no function names required in the question.

- **Code (build-time pass):** `corpus-engine/src/enrichment/code_intel/` —
  `mod.rs` (`SymbolEnrichment`, `enrich_symbol`, the intent-forcing prompt,
  `extract_body`, blake3 `body_hash`), `scip_source.rs` (enumeration),
  `store.rs` (chunk storage), `pass.rs` (the composed pass + body-hash cache).
- **The SCIP substrate:** `corpus-engine-scip` — the **lean read crate**
  (rusqlite + prost, **no tree-sitter grammars**, so the chat runtime can depend
  on it; the grammars stay in the indexing path that *writes* the graph).
  `scip_graph.rs` (`caller_qualified_names`, `find_callers`,
  `find_callees_qualified`), `trace.rs` (`build_symbol_trace` / `render_trace`),
  `scip_export.rs` (`export_all`; **the body span comes from the occurrence's
  `enclosing_range`, not its name `range`** — the name range is just the
  identifier and yields a one-line body).
- **Enumeration is the call graph, not `kind`.** rust-analyzer's SCIP leaves most
  Rust fns `unknown`/`trait`, so the pass enumerates the **caller set**
  (`refs.caller_qualified` — every symbol with a body that calls something),
  drops `#[cfg(test)] mod tests` fns (a `/tests/` SCIP path segment), and is
  file-scopable via `--files=a,b`.
- **What it writes:** one chunk per symbol into the corpus's existing
  `chunks.lance` — `content` = the user-vocabulary summary + the questions it
  answers (what retrieval matches), `source_doc_id = "codeintel:<qualified>"`
  (stable upsert key), metadata `source = "code_intel_summary"`,
  `content_hash = body_hash` (unchanged body ⇒ no re-embed, no model call).
  **Unlike RAPTOR (System 3), these summaries DO surface in normal leaf
  retrieval — that *is* the bridge.**
- **CLI:** `sovereign enrich code-intel <corpus> [--files=a,b,…]`
  (`sovereign-cli-llm/src/enrich_cmd/code_intel.rs`). The SCIP graph itself is
  built/refreshed out-of-band — `sovereign project refresh --local`
  (`sovereign-cli-dev`, in-process `export_all`) or the daemon's Reindexer.
- **Retrieval-time half (the load-bearing difference vs Systems 1–3):** a
  first-class `Intent::CodeQuery` route (`types/routing.rs`) → `handle_code_query`
  (`sovereign-core/.../handlers/code_query.rs`) **scopes retrieval to code
  corpora** (detected by `scip_graph.db` presence, kind-tag-independent) so the
  30+ non-code corpora can't dilute it, then delegates to the knowledge path. At
  each synthesis-evidence site (`knowledge_query.rs`, the DeepQuery path in
  `retrieval.rs`, `metalingual.rs`) `code_trace::build_code_trace_block` opens the
  corpus graph and appends a caller/callee trace for the matched symbols
  (dyn-dispatch boundaries flagged). `reweight_by_query_relevance` boosts
  `code_intel_summary` chunks on `vector_distance` (the key `cross_corpus_sort_cmp`
  actually sorts by) so the user-vocabulary summaries out-rank the far more
  numerous raw code chunks; a `CODE_SYNTHESIS_DIRECTIVE` steers the model to read
  callers/callees off the trace.
- **Status (2026-06-25):** new; validated end-to-end on `commonwealth-ai`
  (plain-English question → `CodeQuery` → summary-bridge surfaces the right
  symbols → call-graph trace → cited answer naming callers at file:line). Today
  the enrichment is **scoped** (run per-file-set, `fast`/Qwopus-4B summaries), not
  yet a full-corpus default. The summaries **also feed System 2**: the
  `structure_first` code branch uses them as Entity descriptions, so code is now a
  queryable + patchable typed-atom graph (see the next section) — not only the
  retrieval-surfaced per-symbol summaries this section describes.

---

## System 2 ∩ System 4 — Code as a queryable, patchable Atlas (v2)

The `structure_first` strategy (`atlas/strategies/code_walk.rs`) lifts a SCIP-indexed
code corpus into a **System-2 typed-atom graph** (Crate→Module→Item containment +
`ScipStructural` call edges + Cargo edges, no LLM) and joins it to the **System-4**
code-intel summaries — code as a first-class, v2-backed, incrementally-patchable Atlas
you can ask architecture questions of.

- **Patch-ready atoms:** content-hash ids (`AtomId::entity_content_hash`, stable across
  rebuilds) anchored to the qualified-name doc-id, so `apply_atom_delta` patches in place
  and the CSR interns stably. Item descriptions are the System-4 **summaries** (rustdoc
  fallback); functions are persisted, bounded (callee fanout ≤ `MAX_CALLEE_FANOUT`).
- **v2 store + flip:** `atoms.lance` + `edges.csr` (`CSR_VERSION 2` carries per-edge
  **provenance**, so call edges are distinguishable from containment) + an ANN seed table
  over the summary embeddings; read behind `AtlasGraph` when flipped (`atlas/.read_v2`).
  `atlas verify-v2` audits lossless v1↔v2.
- **Query — multi-hop CallChain:** `AtlasGraph::call_chain` BFS-walks the CSR call edges
  (callees = "how does X work", callers = "what calls X"), bounded + cycle-safe, cited in
  call order with `[dyn-dispatch]` markers. Two surfaces: the deterministic
  `enrich atlas-query <corpus> "…" [--depth N] [--callers]` brief (named seed via symbol
  match, conceptual seed via ANN over the summaries), and the **chat**
  (`code_trace::build_code_trace_block` emits the N-hop chain as evidence when a v2 code
  atlas exists, 1-hop fallback otherwise).
- **Patch:** `enrich atlas-patch-code <atlas-corpus>` diffs the code-intel cache by
  `(qualified_name → body_hash)`, re-derives only changed symbols' atoms+edges
  (`extract_atoms_for_symbols`), `apply_atom_delta`s `atoms.json`, **then rebuilds
  `atoms.lance`/`edges.csr` and refreshes the ANN** — the load-bearing step that keeps a
  flipped corpus from reading stale. The watcher (`update/delta.rs`) does the structural
  slice automatically. *Limit:* salience + incoming call edges are full-build-only.
- **Live:** `semver-self-atlas` (built, flipped, ANN-backfilled, `verify-v2` PASS);
  generalizes to any SCIP-indexed corpus.

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
| **commonwealth-ai** (code) | _(none — `enrich code-intel` verb)_ | **System 4 — Code intelligence** | LanceDB cosine + FTS, code-intel summary chunks boosted, + SCIP call-graph trace via the `CodeQuery` route | `scip_graph.db` + `sovereign enrich code-intel` |
| **maple-house** (governance probe) | `atlas` (`custom_atlas`, ontology version 0) | System 2, recipe-declared genre | LanceDB cosine + FTS + governance step | `sovereign-recipes/maple-house/recipe.toml` |
| `recipe new --ontology numismatics` / `governance` | `atlas` (`custom_atlas`, ontology version 1) | System 2, recipe-declared genre | as above; declared types reach the Phase-1 schema, prompt and parser | `sovereign-recipes/_templates/ontology-v1/<name>/recipe.toml` |

### Custom ontology (version 1)

A recipe's `[enrichment.ontology]` block is versioned. `version` (absent = 0)
selects a declaration language in `src/enrichment/ontology/language.rs`;
every language parses to `OntologyPolicies` (`src/enrichment/ontology/mod.rs`)
— shape, assertion, identity, change, derivation, prose — and that struct is
all the pipeline reads. Version 0 is the prose `guidance` block every
existing recipe uses; version 1 declares types (`[[enrichment.ontology.types]]`,
one atom kind each, attributes in four value families) plus `voices`,
`change`, `tension` and `derive`. Three load-time rules refuse loudly: an
unknown version names the highest supported; a version-1 key in a block
without `version = 1` names the line to add; a claim type without `force`
does not load. `svrn recipe validate` resolves every reference, enforces the
caps (12 types per kind, 8 attributes per type, 12 enum values) and prints
the derived facets — clock, tension selector, identity default per entity
type, question shapes. Templates: `svrn recipe new --ontology <name>`
(`src/recipe_templates.rs`, data under `sovereign-recipes/_templates/ontology-v1/`).
Migration: `svrn recipe migrate --ontology-version 1 <recipe>`.

The declarations reach the model and the reader. `pipelines/ontology_schema.rs`
generates the Phase-1 response schema by EDITING the shipped
`phase1_section_extraction_schema` — declared names extend the `entity_type`
enum (the generic six stay), relation / event / claim sketches gain their type
slot and one union `attributes` object per kind, claims require `claim_kind`
instead of `discourse_act`, and `argument_reconstructions` is dropped unless
`derive.arguments`. The same module renders the `## Declared types` prompt
block from the same `TypeIndex::effective_attributes` the reader validates
against, so the grammar, the prompt and the parser cannot disagree.
`pipelines/parse_policy.rs` + `pipelines/ontology_parse.rs` enforce it: a
declared type survives as `EntityType::Other("<name>")`, attributes are kept
only when declared on that type (inherited through `specializes`) and only in
their family, stored normalised — a number for a quantity, the declared
spelling for a closed set; a declared voice never becomes an entity and never
holds an attribution; a declared claim takes its `discourse_act` from the
type's `force` and is dropped when it has no anchor. Every drop is traced with
its reason. A corpus that declares nothing runs the identical code under
`ParsePolicy::default()`, and all three compose/parse hooks return `None` —
that, not a remembered branch, is what makes an empty version-1 block compose
version-0 bytes (pinned by `tests/main/ontology_prompt_snapshots.rs`). After
resolution, `atlas/ontology.json` records the policies the atlas was built
under and `_summary.json` (schema 3) carries an `OntologySummary`. Design and
phase plan: `sovereign/docs/specs/ONTOLOGY_PRIMITIVES.md`,
`ONTOLOGY_MIGRATION.md`; field reference: `sovereign-recipes/SCHEMA.md`.

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

2. **GLiNER now runs on both paths.** It is a real ONNX model (above);
   it augments the **conversation** entity graph, and it is also the
   primary NER path for the attached-document T2
   (`document_asset.rs::build_skeleton`), with the grammar-constrained
   LLM call demoted to a fallback for windows where GLiNER isn't loaded
   or returns nothing (`document_asset.rs:1814-1893`).

---

## Reading order for a new contributor

1. **This document** — which system is which, and how a corpus selects one.
2. The selector: `corpus-engine/src/recipe.rs::EnrichmentConfig` +
   `engine/ingest.rs:1581` (the dispatch branch).
3. The deep-dive for the system you're touching:
   [`ENRICHMENT_V2.md`](./ENRICHMENT_V2.md) (atoms),
   [`TIERED_RETRIEVAL.md`](../sovereign/docs/TIERED_RETRIEVAL.md) (RAPTOR/GLiNER),
   or [`CODE_INTEL_CHAT.md`](../sovereign/docs/specs/CODE_INTEL_CHAT.md) (code
   intelligence — the per-symbol summary bridge + SCIP call-graph trace).
4. The injection seam if you're on tiered:
   `enrichment/tiered.rs` (trait) →
   `sovereign-tools/src/conv_tiered_provider.rs` (impl) →
   `sovereign-tools/src/raptor_atlas.rs` (the builder).
