# The epistemic index — one architecture for every enrichment

Status: **design anchor, pre-registration** (2026-09-04). Nothing here is
built. It fixes what we are converging on before any code moves, so that the
SEP atlas, the literary atlas, the custom ontology-v1 path, RAPTOR and the
older field model all port to the same shape, and so that `corpus-mcp` can
serve that shape to a client who is barely technical and wants it to work
out of the box. Siblings: [`ONTOLOGY_PRIMITIVES.md`](ONTOLOGY_PRIMITIVES.md)
(what a declaration says), [`ATLAS_STORAGE_V2.md`](ATLAS_STORAGE_V2.md)
(how atoms are stored), `corpus-mcp/README.md` (the host as built).

The criterion, in the operator's words: the ontologies act as an epistemic
index that provides a map for retrieval, so RAG does not operate only on
cosine distance of terms but can map similarity of ideas and concepts and use
that for rich, connected answers. It must be able to talk about the themes in
a freshly ingested novel. **The real end to end is that someone operates
purely from a TOML recipe, defines their custom ontology, ingests the corpus,
and then gets the epistemically indexed retrieval from their own
llama-server.** Nothing of ours runs as a daemon anywhere in that sentence.

## 0. The claim

Term retrieval seeds on chunks. Idea retrieval seeds on nodes that *are*
ideas — a claim, a question, a tension, a configuration — each carrying an
embedding of the idea's own text, typed edges to the ideas it grounds or
contradicts, and anchors to the passages that evidence it. The embedding
alone never gets from a term to an idea: it is the same cosine over the same
model whatever the text. The graph does. The nearest claim to a question has
a `Tension` edge to its rival and a `Grounds` edge to its premise, and the
walk brings all three back with their evidence.

We already run this walk (`atlas_navigate_ann`, corpus-engine
`enrichment/atlas/context.rs`; applied in sovereign-core
`runtime/retrieval/atlas_grounding.rs`; validated on the SEP bank). What is
missing is not the mechanism. It is that the **ontology is not the map**:
the walk's policy is four constants in sovereign-core, the built-in pipelines
write no ontology file, the seed table is optional, and the host serves the
tiers as separate tools the client must compose.

## 1. Six layers, one invariant each

| Layer | What it is | Invariant | Today | Target |
|---|---|---|---|---|
| **Recipe** | one TOML: acquire, extract, chunk, index, `[enrichment]`, `[enrichment.ontology]` (`sovereign-recipes/SCHEMA.md`; templates under `_templates/ontology-v1/`) | the recipe is the whole declaration; nothing is configured anywhere else | as built (`svrn recipe new --ontology numismatics`, `recipe validate`) | unchanged in shape; the navigation section (§2) is added to the ontology block |
| **Build** (ingest + enrich) | acquire → extract → chunk → embed → index → enrichment phases → resolve → v2 store + seed table + `ontology.json` | runs against ANY OpenAI-compatible chat + embeddings endpoint, in a binary that carries no inference stack | the orchestrator (`sovereign-enrichment-build`) talks plain HTTP (`DaemonInferenceClient`: `/v1/chat/completions` with `response_format: json_schema`, `/v1/embeddings`) but its closure carries llama.cpp via `sovereign-inference`, plus `sovereign-core` and `sovereign-tools`, through seven import sites (§4.1); `corpus install` embeds through the daemon | the seven sites move to leaves; build joins the `corpus-mcp` package as `corpus ingest <recipe>` |
| **Evidence** | chunks: LanceDB vectors + Tantivy FTS, `CorpusIndex::search` | every answer cites a chunk | universal | unchanged |
| **Ideas** (the index) | atoms of the closed kinds (`AtomEnvelope`, `corpus-engine-vocab`), each with embed text, evidence anchors; typed edges; ANN seed table | `atoms.lance` + `edges.csr` + `atoms_ann.lance` are **mandatory** ingest artifacts, coverage reported | seed table mandatory at ingest (ei-3-index, 2026-09-04): `writer::write_atlas_full` takes an `AtlasSeeding` with no default, seeds through the one `backfill_ann` writer in the same write as the v2 store, and a `With` seed that fails fails the atlas write; coverage (`AnnSummary::embedded_atoms`) rides in `_summary.json` v5 and prints in `corpus_list` and `svrn corpus status`. SEP backfilled from 22 of 1,770. Wikipedia still on `edges.lance` + SQLite; `atoms.rkyv` leftovers | one v2 store everywhere; `atoms.json` is export only |
| **Map** (the ontology) | `atlas/ontology.json`, one per atlas, from **every** pipeline | an atlas that cannot describe itself is not an atlas | every pipeline writes it (ei-2-map, 2026-09-04): built-in vocabularies as version-1 TOML under `pipelines/ontologies/`, the envelope names its `pipeline_id`, `navigation` carries the §2.2 table as defaults; existing atlases get it on their next build — nothing reads `navigation` yet | three sections: schema, navigation policy, vocabulary + prose (§2); the walker reads `navigation` (step 4) |
| **Walk** | `ground(question, embedding, atlases, policy) → evidence requests`, then resolve to chunks | ONE implementation, in corpus-engine, driven by the map | glue in sovereign-core (`apply_atlas_grounding`: 2 hops, budget 6, ×0.05, seeds ≥12; chunk→atlas id as a format string) | corpus-engine owns the walk and the id derivation; sovereign-core and corpus-mcp both call it |
| **Surface** | MCP tools | the default tool composes the layers; the client never has to | four tools, client composes | `ask` (§4) plus the four as advanced |
| **Distribution** | prebuilt snapshot (HF datasets, `ingest_prebuilt.rs`) | the snapshot carries all layers; absence is reported, never defaulted | snapshot carries chunks + atlas; host does not pull | `corpus-mcp --corpus sep` pulls if absent; width mismatch degrades and says so |

Principle 8 runs through the table: one store, one map format, one walk, one
id derivation.

## 2. The map: what `ontology.json` must carry

The declaration today (`OntologyPolicies`) has five axes — shape, assertion,
identity, change, derivation — plus prose. Two of them already do index
work: `shape` is the enumeration vocabulary (`is_subtype_of`), `derivation`
names the inferred edges. What is missing is the third role.

1. **Schema** — the node kinds, subtypes and edge kinds this atlas uses,
   with their labels. For a custom corpus this is the declared block as
   built. For a built-in pipeline it is *written down* from the pipeline's
   fixed vocabulary: the literary pipeline says that a `concept` with subtype
   `theme` is a theme and that a `Configuration` is "the interpretive
   structure the work as a whole enacts"; the philosophy pipeline says what
   an `ArgumentReconstruction` is. Same struct, same reader. This doubles as
   the descriptor for the interchange export (Parquet nodes/edges/evidence),
   which is how a technical peer gets the graph into Neo4j or DuckDB in one
   command.
2. **Navigation policy** — a small table of *question kinds* and, for each,
   the seed kinds, the edge kinds to walk, hops and budget. Pre-registered
   defaults, to be tuned on the lanes in §6:

   | Question kind | Seed on | Walk | Hops |
   |---|---|---|---|
   | thematic ("what is this about", "themes") | Configuration, concept Entity | Involves → Tension → Grounds | 2 |
   | trajectory ("how does X change") | Entity, State | Transition, Causes | 2 |
   | tension ("where does it disagree") | Claim, Position | Tension, Opposition | 1 |
   | enumeration ("which X") | declared type + subtypes | none (enumerate) | 0 |
   | lookup ("who is X") | Entity by name | Involves | 1 |

   A pipeline that does not produce a kind simply does not list it; the
   walker skips absent kinds and says so in the ledger.
3. **Vocabulary + prose** — as today (`concern`, `position`, `tension`,
   `absence`, `evidence` terms; guidance).

Question-kind classification is open text over a closed set: a centroid per
kind (principle 9, the router's existing method), seeded from the policy's
kinds, not the keyword matcher in `atlas_traversal/classifier.rs`.

## 3. The port table

| Enrichment | Where it is today | What changes to fit §1 |
|---|---|---|
| **SEP** (`philosophy_atlas`) | per-article `sep-<slug>` atlases; `sep` chunk index has an empty atlas; ANN on 1,770 (backfilled ei-3-index; ledger `sovereign/bench/sep_atlas/`) | emit `ontology.json`; chunk→atlas id derivation moves to corpus-engine |
| **Literary** (`literary_atlas`) | themes as concept entities (phase 1), claims/questions (phase 3), Configuration (phase 8); full book ready in ~4 min on the turbo path | emit `ontology.json` naming `theme`; ANN at ingest; the thematic walk in §2 |
| **Custom** (ontology-v1) | declared shape/assertion/identity/change/derivation; built through the daemon (wessex-hoard: 20 chapters, phase 1 with `schema=true`) | add the navigation section; the build runs against a bare endpoint (§4) |
| **RAPTOR** | `raptor_summaries.lance` + `raptor_grounding.rs`, injected as virtual chunks | summaries become `Summary` nodes with `EvidenceFor` edges to chunks and `Composes` edges to children; the walk reaches them; the separate injector retires |
| **Field model v1** (`field_skeleton.json`, 549 SEP questions) | spliced as a 250-token digest by `turn_prepass` | canonical questions → `Question` atoms, concerns → concept entities; the digest is rendered from the atlas; the v1 writer retires |
| **Tiered conversation** | atlas type already; per-conv entity graph for PPR | emit `ontology.json`; no store change |
| **Wikipedia** | `edges.lance` + `wikipedia_graph.db` (2.4 GB SQLite) | fold into the v2 store (the `ATLAS_STORAGE_V2` burn-down) |

Kinds stay a closed enum; what a pipeline *produces* is what its map lists.
No pipeline gets a private node kind.

## 4. The real end to end

The person is barely technical. They have an MCP-capable chat app, a folder
of documents, and a machine that can run llama-server or Ollama. The whole
experience, from nothing to an epistemically indexed answer, is one recipe
and three commands:

```sh
corpus recipe new --ontology numismatics --id my-coins   # writes my-coins.toml; they fill path + guidance + types
corpus ingest my-coins.toml                              # acquire → chunk → embed → enrich → index, against their endpoint
corpus serve --corpus my-coins                           # the MCP host; one JSON block in the chat app
```

`corpus` is the binary `corpus-mcp` is today, grown two verbs. It is the
one package in `quality/ARCH_LAYERS.toml` whose closure the boundary-gate
already holds to the leaves: no llama.cpp, ort, iroh, mesh transport, or
agent runtime. `ingest` needs two model endpoints, chat and embeddings.
Ollama serves both from one URL, which is the default; llama-server serves
one model per process, so `--chat-url` and `--embed-url` are the explicit
form. Each is probed and named at start; `GET /oicp/v1/capabilities` 404 is
the normal case. Structured output is `response_format: json_schema`, which
llama-server and Ollama both honour; a phase whose schema the endpoint
rejects is reported as could-not-run, not skipped.

Then they ask a question and the model calls **one** tool:

- `ask(question, corpus?)` — the default. Embeds once, runs tier 1, applies
  the walk from the corpus's own map, resolves evidence, returns cited
  passages **and a map section**: the idea nodes traversed, their kinds,
  and the edges followed, so the answer can be connected and the ledger is
  visible (principle 1). Degradations are in the result text: no seed
  table, no ontology, width mismatch, budget exhausted.
- `corpus_search`, `atoms_lookup`, `corpus_ontology`, `corpus_list` stay as
  the advanced surface for a client that wants the layers apart.
- `export(corpus, format=parquet)` for the technical peer who asked about
  Neo4j.

Every failure along the path is a sentence in stderr and in `corpus_list`,
never a default: endpoint not found, width mismatch, a phase that could not
run, a corpus with no seed table.

### 4.1 What binds the build to the stack today

The enrichment orchestrator's model client is already plain HTTP. What
pulls the inference stack into its closure is seven import sites, none of
them inference:

| Import | From | Where it belongs |
|---|---|---|
| `InferenceProvider` (the backfill embedder) | `sovereign-core` | replaced by the same `EmbedFn` corpus-mcp already builds over `/v1/embeddings` |
| `StepOutput`, `ToolContext`, `DeclaredTool` | `sovereign-core` | `kernel-types` / `sovereign-contracts` (workflow envelope types) |
| `backfill_ann`, `AtlasContextFilter`, `BackfillOutcome` | `sovereign-tools` | `corpus-engine` (it is an atlas write) |
| `EXIT_CANCELLED` | `sovereign-tools` | `sovereign-contracts` |
| `fetch_manifest` | `sovereign-inference` | behind a trait the host implements, or the OICP capabilities probe corpus-mcp already has |
| egress `model_client`, `verify`, `ConsentGrant` | `sovereign-core` | `sovereign-contracts` (the F26 census already treats these as a leaf concern) |

That is a dependency cut, checked by the boundary-gate the moment
`sovereign-enrichment-build` is listed in the package, not a rewrite of the
phases. GLiNER is optional at the engine (`with_chunk_entity_extractor`);
without it the entity pass is the LLM's, slower and reported as such.

## 5. Non-goals

- A graph server. The hot path is seed → two hops over an mmapped CSR →
  FTS fetch; a server adds a process and a hop and breaks the file-shipped
  snapshot. The export is how Neo4j users get the graph.
- Formal inference (OWL, reasoners). The index is embeddings + typed edges
  + evidence anchors; the map says what the kinds mean and how to walk them.
- A new embedding model. Idea similarity comes from the graph, not from a
  better cosine.
- A private node kind per pipeline.

## 6. Baseline — pre-registered before any code moves

The bar exists before the data (§18). Lanes, in `sovereign/bench/`:

| Lane | Kind | What it measures | Bar the work must clear |
|---|---|---|---|
| `literary` (bk-book-1, dubliners-3) — add a **thematic** question set | HARD | themes named, tensions cited, ≥1 evidence passage per claim | ≥ today's score on every existing question; thematic set: recorded as baseline, then +1 theme cited with evidence over baseline |
| `sep` retrieval-prod | HARD | sources / essay / dialectical breadth | unchanged within the noise band (RUNBOOK §6) |
| `corpus-mcp/acceptance.sh` — becomes the end to end of §4 | HARD | scaffold from the numismatics template with the in-repo fixture (`sovereign-recipes/wessex-hoard/wessex-hoard.md` + `truth.json`), `corpus ingest` against a bare chat + embed endpoint, `corpus serve`, `ask` | `ask` returns attribution claims with cited passages, the map section names `coin` / `attribution` nodes and a `Tension` or `Grounds` edge, `truth.json` recall ≥ the daemon-built wessex-hoard's; then the same on a fresh literary corpus: ≥3 themes each with a cited passage. Passes against `llama-server` and against Ollama |
| `atlas_retrieval` | TRACKED | walk yield / drop ledger | reported per question kind |

Two runs per lane before a delta is read (principle 7). The first thing built
is the thematic question set and its baseline against today's path, run
twice, committed with the numbers. A regression below the floor on any HARD
lane names an owner and a scheduled measurement before it is accepted.

## 7. Order of work

1. Bar: the thematic set + baseline (§6). No code under `corpus-engine`
   moves until it is committed.
2. Map: every pipeline writes `ontology.json`; the navigation section lands
   with defaults from §2; `corpus_ontology` shows it.
3. Index: seed table mandatory at ingest, coverage in `corpus_list` and in
   the atlas summary; backfill SEP.
4. Walk: `ground()` and the chunk→atlas derivation move into corpus-engine;
   sovereign-core calls it; then corpus-mcp calls it — `ask` lands.
5. Build: the seven-site cut (§4.1); `sovereign-enrichment-build` joins the
   package; `corpus ingest <recipe>` lands and the acceptance runs the whole
   of §4 on the wessex fixture against a bare endpoint.
6. Distribution: pull-if-absent and endpoint discovery; acceptance against
   Ollama; `corpus serve` and `corpus recipe new`.
7. Ports, one per commit, each measured on its lane: RAPTOR, field model,
   wikipedia store.

Each step is its own commit with the lane numbers in the body, and each
updates this file's §1 "Today" column and `SYSTEM_OVERVIEW.md` in the same
commit.
