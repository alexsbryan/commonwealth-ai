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
a freshly ingested novel.

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
| **Evidence** | chunks: LanceDB vectors + Tantivy FTS, `CorpusIndex::search` | every answer cites a chunk | universal | unchanged |
| **Ideas** (the index) | atoms of the closed kinds (`AtomEnvelope`, `corpus-engine-vocab`), each with embed text, evidence anchors; typed edges; ANN seed table | `atoms.lance` + `edges.csr` + `atoms_ann.lance` are **mandatory** ingest artifacts, coverage reported | seed table optional (SEP: 22 of 1,770 atlases); wikipedia on `edges.lance` + SQLite; `atoms.rkyv` leftovers | one v2 store everywhere; `atoms.json` is export only |
| **Map** (the ontology) | `atlas/ontology.json`, one per atlas, from **every** pipeline | an atlas that cannot describe itself is not an atlas | custom pipelines only; SEP/literary vocabulary lives in prompt `.md` files | three sections: schema, navigation policy, vocabulary + prose (§2) |
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
| **SEP** (`philosophy_atlas`) | per-article `sep-<slug>` atlases; `sep` chunk index has an empty atlas; ANN on 22 | emit `ontology.json`; ANN mandatory + backfilled; chunk→atlas id derivation moves to corpus-engine |
| **Literary** (`literary_atlas`) | themes as concept entities (phase 1), claims/questions (phase 3), Configuration (phase 8); full book ready in ~4 min on the turbo path | emit `ontology.json` naming `theme`; ANN at ingest; the thematic walk in §2 |
| **Custom** (ontology-v1) | declared shape/assertion/identity/change/derivation | add the navigation section; nothing else |
| **RAPTOR** | `raptor_summaries.lance` + `raptor_grounding.rs`, injected as virtual chunks | summaries become `Summary` nodes with `EvidenceFor` edges to chunks and `Composes` edges to children; the walk reaches them; the separate injector retires |
| **Field model v1** (`field_skeleton.json`, 549 SEP questions) | spliced as a 250-token digest by `turn_prepass` | canonical questions → `Question` atoms, concerns → concept entities; the digest is rendered from the atlas; the v1 writer retires |
| **Tiered conversation** | atlas type already; per-conv entity graph for PPR | emit `ontology.json`; no store change |
| **Wikipedia** | `edges.lance` + `wikipedia_graph.db` (2.4 GB SQLite) | fold into the v2 store (the `ATLAS_STORAGE_V2` burn-down) |

Kinds stay a closed enum; what a pipeline *produces* is what its map lists.
No pipeline gets a private node kind.

## 4. The "just works" path

The client is a person who has installed an MCP-capable chat app and can
paste one JSON block. The whole experience must be:

```sh
ollama pull qwen3-embedding:0.6b          # or any OpenAI-compatible embedder
corpus-mcp --corpus sep                   # pulls the prebuilt if absent, detects the endpoint
```

and one block in the client's MCP config. Then they ask a question and the
model calls **one** tool:

- `ask(question, corpus?)` — the default. Embeds once, runs tier 1, applies
  the walk from the corpus's own map, resolves evidence, returns cited
  passages **and a map section**: the idea nodes traversed, their kinds,
  and the edges followed, so the answer can be connected and so the ledger
  is visible (principle 1). Degradations are in the result text: no seed
  table, no ontology, width mismatch, budget exhausted.
- `corpus_search`, `atoms_lookup`, `corpus_ontology`, `corpus_list` stay as
  the advanced surface for a client that wants the layers apart.
- `export(corpus, format=parquet)` for the technical peer.

What the host must do so that this holds: find an endpoint (`--base-url`
default `http://localhost:11434/v1`, then `:8080/v1`, then the OICP daemon;
each probed and named), pull the corpus from the HF dataset when it is not
under the data root, verify the index width against the endpoint, and load
the map. Every one of those can fail, and each failure is a sentence in
`corpus_list` and in stderr, never a default.

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
| `corpus-mcp/acceptance.sh` — add an `ask` step on a fresh literary corpus | HARD | `ask` returns ≥3 themes each with a cited passage, map section non-empty, no default-shaped degradation | passes on this host against `llama-server` and against Ollama |
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
5. Distribution: pull-if-absent and endpoint discovery in corpus-mcp;
   acceptance against Ollama.
6. Ports, one per commit, each measured on its lane: RAPTOR, field model,
   wikipedia store.

Each step is its own commit with the lane numbers in the body, and each
updates this file's §1 "Today" column and `SYSTEM_OVERVIEW.md` in the same
commit.
