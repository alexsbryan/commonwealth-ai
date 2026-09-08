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
| **Build** (ingest + enrich) | acquire → extract → chunk → embed → index → enrichment phases → resolve → v2 store + seed table + `ontology.json` | runs against ANY OpenAI-compatible chat + embeddings endpoint, in a binary that carries no inference stack | **done (ei-5a-build-cut + ei-5b-build-verb, 2026-09-04)**: the seven sites are cut, `sovereign-enrichment-build` + `sovereign-enrichment-catalog` are in the `corpus-mcp` `[[package]]`, and `corpus-mcp ingest <recipe.toml>` drives the whole path — `corpus_engine::recipe_install::register` → `CorpusEngine::ingest` over an HTTP `EmbedFn` → one `config.json` → `build_with_progress_with_embedder`. Two endpoints, because llama-server loads one model per process: `--chat-url` / `--embed-url` (or `--base-url` for a host serving both), each probed and named before disk is touched, and both reaching `config.json` as `base_url` + `embed_base_url`. Closure 609 crates, no llama.cpp / ort / iroh / `sovereign-core` / `sovereign-tools` (`tests/no_inference_stack.rs`, boundary-gate). GLiNER is absent and the entity pass is the model's, printed as such | pull-if-absent and Ollama defaults (step 6) |
| **Evidence** | chunks: LanceDB vectors + Tantivy FTS, `CorpusIndex::search` | every answer cites a chunk | universal | unchanged |
| **Ideas** (the index) | atoms of the closed kinds (`AtomEnvelope`, `corpus-engine-vocab`), each with embed text, evidence anchors; typed edges; ANN seed table | `atoms.lance` + `edges.csr` + `atoms_ann.lance` are **mandatory** ingest artifacts, coverage reported | seed table mandatory at ingest (ei-3-index, 2026-09-04): `writer::write_atlas_full` takes an `AtlasSeeding` with no default, seeds through the one `backfill_ann` writer in the same write as the v2 store, and a `With` seed that fails fails the atlas write; coverage (`AnnSummary::embedded_atoms`) rides in `_summary.json` v5 and prints in `corpus_list` and `svrn corpus status`. SEP backfilled from 22 of 1,770. Wikipedia still on `edges.lance` + SQLite; `atoms.rkyv` leftovers | one store PROVIDER everywhere (operator 2026-09-04): the walk consumes one trait — atom by id, atoms of a kind, evidence anchors, edges from/to with a closed `EdgeType`, the seed table, the ontology — and a backend fulfils it; atom-class backends are `atoms.lance` + `edges.csr` + `atoms_ann.lance`, wiki-class is `articles.lance` + `edges.lance` + a seed table; `atoms.json` is export only |
| **Map** (the ontology) | `atlas/ontology.json`, one per atlas, from **every** pipeline | an atlas that cannot describe itself is not an atlas | every pipeline writes it (ei-2-map, 2026-09-04): built-in vocabularies as version-1 TOML under `pipelines/ontologies/`, the envelope names its `pipeline_id`, `navigation` carries the §2.2 table as defaults; existing atlases get it on their next build — nothing reads `navigation` yet | three sections: schema, navigation policy, vocabulary + prose (§2); the walker reads `navigation` (step 4) |
| **Walk** | `ground(question, embedding, atlases, graphs, selection, max_seeds) → evidence requests`, then resolve to chunks | ONE implementation, in corpus-engine, driven by the map | **done (ei-4-walk, 2026-09-04)**: `corpus-engine/src/enrichment/atlas/{ground,resolve}.rs` over `&[&dyn AtlasProvider]`, reading seed kinds / edge kinds / hops / budget from the corpus's `navigation` section; the question's kind is a centroid over the map's own exemplars, and an abstain runs `WalkPolicy::unfiltered` (the pre-policy behaviour, as data) and says so. `apply_atlas_grounding` and `corpus-mcp`'s `ask` are the two callers; `atlas_navigate_ann` is a thin caller under the unfiltered row. The chunk→atlas id derivation is `ground::candidate_atlas_ids`. The evidence budget round-robins across the ideas the walk reached — the loop it replaced spent all of it on the first. **Sole since ei-5c (2026-09-07)**: the retrieval-time RAPTOR injector (`apply_raptor_grounding`) was the second grounding implementation this row forbids and is deleted — not folded into a caller, because it read `conv_raptor_nodes` where the walk reads `Summary` atoms and had nothing to delegate to. What let it go is the per-kind seed QUOTA in §2.2; what it costs on a corpus whose atlases have no `Summary` atoms yet is a named degradation at the grounding call, never a silent absence. The walk's source split four ways at the same commit (`ground.rs` the walk, `ground/select.rs` which row, `ground/report.rs` the ledger, `ground/tests.rs` the tests) with every path unchanged by re-export | unchanged |
| **Surface** | MCP tools | the default tool composes the layers; the client never has to | **done (ei-4-walk, 2026-09-04)**: `ask` composes embed → tier 1 → walk → resolve and returns cited passages plus the map section, with every degradation as a sentence; the four earlier tools stay as the advanced surface | unchanged |
| **Distribution** | prebuilt snapshot (HF datasets, `ingest_prebuilt.rs`) | the snapshot carries all layers; absence is reported, never defaulted | **done (ei-6-distribution, 2026-09-05)**: `corpus-mcp serve --corpus <id>` on a machine without `<id>` installs it through `CorpusEngine::ingest(CorpusSpec::Builtin)` → the existing `engine/ingest_prebuilt.rs`, so the archive's chunks AND atlas land in one go and no second downloader exists. A recipe with no `[prebuilt]` block is refused by name, never turned into a silent acquire-and-embed; pull-if-absent applies only to ids the caller named. Width mismatch still degrades that corpus to full-text and says so. The three §4 commands are all verbs now — `recipe new` reuses `corpus_engine::recipe_templates` (nothing moved: `svrn recipe new` is the other caller of the same three functions), and neither `ingest` nor `serve` needs a URL, because `host::discover` walks Ollama `:11434` → llama-server `:8080` → `client_daemon_base()` and names every rung on stderr, at debug, and in `corpus_list`. A named `--base-url` is refused when it does not answer rather than swapped (§18.3) | **Ollama measured 2026-09-07 (ei-3c)**: v0.33.3 installed user-space, the ladder named it at rung 1, `qwen3-embedding:0.6b` is 1024-d so `sep` served as `vector + full-text` with no degradation, and `corpus_list` answered over stdio. One rough edge, banked against corpus-mcp: with no `--embed-model` the host sends the first id `/v1/models` returns, which on Ollama — the one shape serving chat and embeddings from a single URL — was the CHAT model, and `/v1/embeddings` answered 501. Evidence in `runs/ei3c-ollama-arm/evidence-2026-09-07-run3/` | prefer a listed id that answers an embedding probe, or refuse by name rather than send a chat model |

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
   the seed kinds, the edge kinds to walk, hops, budget, and the exemplar
   phrases that classify a question ONTO that kind. Pre-registered defaults,
   to be tuned on the lanes in §6.

   **Every row's budget is 12, not 6.** This document said "budget 6" in §1's
   Walk row and ei-2 minted `DEFAULT_BUDGET` from that sentence; the sentence
   was wrong about the code. Atlas grounding has kept
   `ceil(KQ_PER_CORPUS_LIMIT * 0.6)` with `KQ_PER_CORPUS_LIMIT = 20`
   (`sovereign-core/src/runtime/prompts.rs`) since the SEP calibration, i.e.
   12. Adopting 6 the moment the walk started reading this table would have
   halved atlas evidence on every corpus — a regression handed to the SEP lane
   by a typo. Corrected 2026-09-04 (ei-4-walk); the two are now pinned
   together by `the_default_budget_is_the_live_fetch_budget`.

   The **exemplars live in the map** (`WalkPolicy::exemplars`), not in the
   walker: a corpus whose readers phrase a kind their own way retunes its
   classifier by editing its own declaration, and the built-in defaults are
   this table's own glosses. An empty list switches a row off; every row empty
   means the map declares no classifier, which is reported rather than
   defaulted.

   `Summary` joined the thematic row in ei-7a. That is a REPAIR, not a
   tuning move: §3's RAPTOR row says the walk reaches Summary nodes, and a
   kind that no row seeds on is written, embedded and never reached — this
   table simply predated the kind. A Summary seed does not expand and does
   not score leaf evidence (`atlas::ground` R1/R2), so listing it changes
   what the walk can REACH, not how leaf evidence is ranked.

   | Question kind | Seed on | Seed quota | Walk | Hops |
   |---|---|---|---|---|
   | thematic ("what is this about", "themes") | Configuration, concept Entity, Summary | Summary: 8 | Involves → Tension → Grounds → Configures | 2 |
   | trajectory ("how does X change") | Entity, State | — | Transition, Causes | 2 |
   | tension ("where does it disagree") | Claim, Position | — | Tension, Opposition | 1 |
   | enumeration ("which X") | declared type + subtypes | — | none (enumerate) | 0 |
   | lookup ("who is X") | Entity by name | — | Involves | 1 |

   A pipeline that does not produce a kind simply does not list it; the
   walker skips absent kinds and says so in the ledger.

   **The seed QUOTA column and the `Configures` edge are ei-5c (2026-09-07),
   and both are repairs of the same shape as `Summary`'s row entry — the table
   saying a kind is reachable while nothing in the table can reach it.**

   A quota says how many of a walk's seed slots one kind may take. A kind that
   declares one draws from its OWN pool; every other admitted kind shares
   `max_seeds` as before, so a quota can never take a slot from a kind that has
   none. It exists because reachable was not enough: seeds come from one
   score-ordered pool, a rollup is written to be about a whole region and
   therefore outscores any single leaf on exactly this row's question, and the
   SEP subset A/B measured OFF 47/66 twice against ON 40/66 and 39/66 —
   −7.5/66 on a HARD lane — with Summary winning about 1.1% of slots against
   ~21k entity and argument seeds. Before the field, the only way to say
   "reachable but never at a leaf's expense" was to make the kind unreachable,
   which is why the injector had to stay. Eight is not a fresh number: it is
   `SOVEREIGN_RAPTOR_TOP_M`'s shipped default in the injector this replaces, so
   the volume of late-appended summary text is unchanged by the port and a lane
   delta is attributable to the walk reaching them (§18.4). The walk's R3
   truncation reads the row's quota rather than a constant of its own, so the
   count that may seed and the count that may be appended cannot drift.

   `Configures` joins the thematic edge list because the row SEEDS on
   `Configuration` and listed no edge kind a Configuration has, so a walk that
   landed on one could not leave it — a terminus by accident, unlike Summary's
   which is by rule. It is an existing kind (§3 forbids a private one): it has
   carried a CSR type byte and an edge weight since ATLAS_STORAGE_V2, minted
   for this exact relation and emitted by nothing. The atlas store's writer
   derives it from `Configuration.constituent_atoms` now, so the edge the walk
   follows and the field the atom carries are one fact.

   A derived edge kind needs the store to KNOW its derivation moved, or it
   ships and cannot be applied: `store_needs_build` reads a CSR header and an
   mtime, and neither changes when the code learns a new derivation. So
   `atlas/edges.csr.derivation` carries a version beside the graph and an
   absent-or-older one reads as stale — the same marker shape the seed table
   already uses one level down. Every installed atlas therefore rebuilds its
   store once, at its next `atlas migrate-all`; nothing on the read path
   consults the marker, so a corpus keeps grounding meanwhile.
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
| **RAPTOR** | `raptor_summaries.lance` + `raptor_grounding.rs`, injected as virtual chunks | **done (ei-5c, 2026-09-07)**: summaries are `Summary` atoms projected from the existing `raptor_summaries.lance` rows by `svrn enrich summary-atoms <corpus>` (no re-embed — the stored vector becomes the seed row), the walk seeds on them under the thematic row's quota, and the separate injector is DELETED. The chunk relation rides `Summary::evidence` rather than an edge, because an `Edge` is atom → atom and a chunk is not an atom; `Composes` to children is emitted. A corpus whose atlases have not been projected yet has no whole-work summaries in retrieval, and that absence is a named line at the grounding call rather than a quiet zero |
| **Field model v1** (`field_skeleton.json`, 549 SEP questions) | **done (ei-7b, 2026-09-05)**: `enrichment/field_atoms.rs` is the two-way projection — canonical questions → `Question` atoms, their positions → `Position` atoms (`Question.addressed_by`), fault lines → `Opposition`; `publish_to_atlas` is the one write path for both the pipeline's terminal step and the one-shot `svrn enrich field-atoms <corpus> [--into <corpus>]` migration, idempotent by content-derived id and writing NO seed row, so the walk is unchanged. `turn_prepass::splice_ambient_field_digests` asks `field_atoms::load_field_model` — the ONE accessor for where a corpus's field model lives: the atlas (census-gated on `_summary.json`'s `Question` count, falling through when that is stale), then `field_skeleton.json` as a MIGRATION FALLBACK, then `None` — and calls the SAME `render_landscape` on whatever it gets. The digest text is pinned byte-identical across the two sources by a round-trip test and all three precedence arms are pinned too. The fallback makes which-source-a-corpus-serves-from a DATA choice (`svrn enrich field-atoms <corpus>`) rather than a code one, so the port cannot take an un-migrated corpus's digest dark — `sep` has 549 questions in the v1 file and an EMPTY atlas. Which artifact a domain publishes is the existing `SkeletonStorage` decider with a new `AtlasAtoms` arm: `philosophy` (SEP) takes it, so `field_skeleton.json` is no longer WRITTEN for SEP; it is still READ for it until `enrich field-atoms sep` runs, and the audit line names which source served each digest. The three KnowledgeView domains stay on `JsonAndLance` — their reader (`knowledge_view::manager`: its own `format_landscape`, an mtime-keyed cache, a cross-view digest that embeds skeleton content) is a separate port, and moving them first would take three live views dark. The pipeline's phase-1 resume state moved out of the artifact either way, into `_field_skeleton_checkpoint.json`, which carries the fields (proponents, cluster ids, centroids) the atom vocabulary has no home for. Concept entities are NOT emitted: no consumer in this phase reads them | port the KnowledgeView reader onto the atlas so the last three domains move; seed the field-model atoms once there is evidence the walk should reach them |
| **Tiered conversation** | atlas type already; per-conv entity graph for PPR | emit `ontology.json`; no store change |
| **Wikipedia** | `edges.lance` + `wikipedia_graph.db` (2.4 GB SQLite) | fulfil the store provider over `articles.lance` + `edges.lance` (the six link labels ride as data beside a closed `Involves` kind — no new `EdgeType` arm, spec §3); the seed table is MIGRATED from the chunk index's existing vectors (`first_appearance.chunk_id` → `chunks.lance`, same embed model), validated by a cosine sample first; the SQLite retires (operator ruling 2026-09-04, replacing "fold into the v2 store") |

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
   **DONE 2026-09-04 (ei-4-walk).** One caveat the step surfaced and did not
   own: `writer::seed_atlas` seeds the ANN table through
   `AtlasContextFilter::default()`, whose `include_configurations` and
   `include_tensions` are `false`, so every seed table is Entity-only. Of the
   five rows above, `lookup` and `enumeration` seed fully, `thematic` seeds
   only its concept-Entity half, and `tension` (Claim + Position) seeds
   nothing on any corpus built to date. That is a contract gap between this
   §2.2 table (which kinds a row seeds on) and step 3's table (which kinds are
   populated), and it belongs to the seed population, not to the walk.
5. Build: the seven-site cut (§4.1); `sovereign-enrichment-build` joins the
   package; `corpus ingest <recipe>` lands and the acceptance runs the whole
   of §4 on the wessex fixture against a bare endpoint.
   **DONE 2026-09-04 (ei-5a-build-cut + ei-5b-build-verb).** Two things the
   step surfaced that §4 had not said. (a) `EnrichConfig` carried ONE
   `base_url` for chat and embeddings, which is right for the daemon, Ollama
   and vLLM and wrong for llama-server; the resolution embeddings of every
   phase went wherever the chat model was. It carries `embed_base_url` now,
   read through the one accessor `EnrichConfig::embed_base`, `None` on every
   corpus built before this and meaning "one host". (b) `config.json`'s
   `base_url` is a ROOT, not a `/v1` base — `probe_daemon`, `embed_one` and
   `providers::local_daemon_base` each append the version segment themselves.
   The acceptance's ingest leg is opt-in (`ACCEPT_INGEST=1 CHAT_GGUF=…`)
   because it wants a second llama-server and tens of minutes; unset it
   reports NEVER-RAN by name rather than passing quietly.

   **The endpoint is proven; the recall number is not yet taken.** One
   chapter through the whole verb against two bare `llama-server` processes
   (chat + embed) returns rc 0 and the acceptance PASSes: v2 store,
   `atoms_ann.lance` seed table 11/11 resolved, `ontology.json`, 17 atoms,
   8 enrichment steps, and `ask` answering with 17/17 cited passages, with
   both degradations printed. What the 20-chapter run against the bar needs
   is a chat model whose reasoning TERMINATES on a schema-constrained
   extraction. A thinking model that does not returns an empty `content`
   rather than an error — llama-server holds the reasoning in
   `reasoning_content` and fills `content` only once thinking closes — so an
   exhausted budget reads as `<empty response>` at phase 1. Measured
   2026-09-04 on `Qwen3.5-4B-UD-MTP-Q6_K_XL`: 2 of 3 chapters failed the
   first pass that way, recovered only by the terse retry at double budget,
   putting a 20-chapter ingest into hours. The same endpoint returned
   conforming JSON for a schema at a 16,384-token budget and an empty string
   for the identical request at 64, so the endpoint's `json_schema` support
   is not in question — the model's budget is.

   **The bar was then taken on `Qwen3.6-35B-A3B`, twice** (`runs/ei5b-stage2/`,
   2026-09-05; walls 5,418 s and 4,796 s against the daemon control's 333-360 s
   for the same 20 chapters). Both atlases PASS `truth.json` standing alone —
   every declared row met, and both extract `die-link`, which the control never
   did:

   | row (required) | control | run a | run b |
   |---|---|---|---|
   | catalogue_ref (7) | 7 | 7 | 7 |
   | coin family (7) | 19 | 22 | 20 |
   | mint (3) | 3 | 3 | 3 |
   | ruler (4) | 4 | 4 | 4 |
   | attribution (7) | 49 | 44 | 40 |
   | grade values | 3 of 4 | **4 of 4** | **4 of 4** |
   | atoms | 159 | 172 | 165 |

   The acceptance nonetheless reports FAIL, on `attribution: 40 vs 49`. That
   is a real disagreement about what the bar means and it is left open rather
   than resolved by editing the comparison: the first column of each row is
   how many atoms matched, the second is how many `truth.json` REQUIRES, and
   every run clears its requirement. Comparing the first numbers to the
   control's asks whether the bare endpoint YIELDED as many atoms; comparing
   coverage of the requirement asks whether it RECALLED what was declared.
   Under the second reading both runs equal the control everywhere and beat it
   on `grade values`; under the first they trail it on one row, twice, by
   10-18%. The scorer implements the first and this section's bar is worded as
   the second. Do not change the rule to make a run pass (ARCH §18.6).

   **Two phases return nothing on a bare endpoint, on both runs.** Phase 3
   (cluster naming) reports `0/4` and `0/7` named, all ParseDrift; phase 6
   (the tension classifier) reports 0 of 164 and 0 of 124 classified, every
   candidate a parse failure. Extraction and resolve are unaffected — 172 and
   165 atoms against 159 — so what a bare endpoint loses is the classified
   layer above the atoms, not the atoms. Phase 1 also needed the terse retry
   on 18 of 20 chapters and recovered all 18, spending 201,596 completion
   tokens against the daemon's 17,940 for the same extraction. Whether the
   classified layer is a daemon-only capability under §4's own kill rule is
   the open question this step hands on.

   One caveat the step surfaced and did not own, the sibling of step 4's.
   §6 row 3 asks the map to name `coin` / `attribution` **and** a `Tension` or
   `Grounds` edge. The `attribution` half holds: the seed table populates all
   six kinds since ei-3c, so a `tension` question seeds on the recipe's own
   claim type and the declared noun reaches the answer. The connected half
   cannot: the `tension` row's §2.2 default walks `Tension` and `OpposesIn`,
   and a declared-ontology corpus of this shape produces `Involves`
   (claim → the coin it is about) and `Grounds` (claim → its evidence), so
   from an attribution there is no hop of a kind the row traverses and `coin`
   is unreachable however good the extraction was. That is a contract gap
   between the §2.2 navigation defaults and the edge vocabulary a declared
   ontology actually produces — the same shape as step 4's seed-kind gap, on
   the walk side rather than the seed side. Widening the row to make the bar
   green would be tuning a navigation default to the bench, so the acceptance
   judges the connected half only when the row's walk kinds intersect the
   atlas's, and reports COULD-NOT-JUDGE naming both sets otherwise. It belongs
   to the navigation defaults, not to the build.
6. Distribution: pull-if-absent and endpoint discovery; acceptance against
   Ollama; `corpus serve` and `corpus recipe new`. **Landed 2026-09-05
   (ei-6-distribution)**, with one arm reported rather than measured: the
   development host has no Ollama (`command -v ollama` empty, `:11434`
   refusing), and installing software on it is the operator's call, so the
   Ollama acceptance leg reports COULD-NOT-RUN BY NAME — not a pass, not a
   fail — and this order's fallback clause applies: the llama-server path
   ships and is measured, the Ollama default is a documented option. The
   `sovereign contract census` half of the done-when is COULD-NOT-JUDGE by
   construction: `cli-contract.toml` declares itself the manifest of "the
   commands `sovereign` promises", `Contract::binary` is a closed enum of the
   four sovereign siblings, and a journey step's `run` is argv for the `svrn`
   runner — so no `corpus-mcp` verb is representable in it (ei-5b's `ingest`
   has no row either). The verbs' falsifiable proof is `corpus-mcp/tests/verbs.rs`
   instead; widening the shared manifest is banked.
7. Ports, one per commit, each measured on its lane: RAPTOR, field model,
   wikipedia store.

Each step is its own commit with the lane numbers in the body, and each
updates this file's §1 "Today" column and `SYSTEM_OVERVIEW.md` in the same
commit.
