# Commonwealth AI — System Overview

What exists, how it fits, and where to look. Read it whole — that is what its
size is for. `ARCH_PRINCIPLES.md` is the rules of engagement;
`docs/ARCHITECTURE_TOUR.md` is the ten-minute newcomer path.

A contract per `ARCH_PRINCIPLES.md §1.1`: every claim verifiable against the
code on the commit it appears in. Change a subsystem, update its entry in the
same commit.

**It states what IS.** How each shape was arrived at — the incidents, the
measurements, the orders — is
[`SYSTEM_OVERVIEW_DETAIL.md`](./SYSTEM_OVERVIEW_DETAIL.md), which keeps this
numbering, so `§4` here is `§4` there. Open it before changing something, to
learn what already went wrong. Also [`HISTORY.md`](./HISTORY.md) (how the
shape came to be), [`DEFAULTS_LEDGER.md`](./DEFAULTS_LEDGER.md) (everything
shipped default-off or dark, with its flip condition and review-by date).

**This file has a ceiling and it is deliberate.** It reached 10.4k lines by
accreting an incident log inside a map — single table cells ran three thousand
words — and at that size it was sampled, never read. Add a fact here only if a
reader needs it to ORIENT. If they need it to ACT, it belongs in the detail
file or the subsystem's own doc.

---

## 1. The projects

```
commonwealth-ai/
├── oicp-types/                # OICP wire types — no other deps
├── kernel-types/              # The neutral kernel — identity, provenance, trust, the released turn
├── oicp-client/               # OICP pure-HTTP client (OpenAI-compat + manifest routing)
├── oicp-conformance/          # Standalone OICP v0.4 host conformance tester
├── oplog/                     # Op/Oplog/Journaled — the append-only JSONL journal (tier-0)
├── serving-policy/            # Fair-share scheduling + pipeline aliases (tier-0)
├── corpus-engine/             # Knowledge layer (LanceDB + Tantivy)
├── corpus-index/              # Retrieval read-port leaf — CorpusIndex, persisted settings, the engine Error
├── corpus-engine-scip/        # SCIP call graph + per-language exporter dispatch
├── corpus-engine-notes/       # NoteStore + project_docs index
├── corpus-engine-atos/        # ATOS feature store + plan items (opt-in, `--features atos`)
├── corpus-engine-archaeology/ # Git archaeology + rough-edges + atom-provenance
├── corpus-engine-yield/       # YieldHook cooperative-yield contract (tier-0 leaf)
├── corpus-engine-sections/    # Section detectors as a regex-only leaf
├── corpus-engine-watchers/    # Lint/test/project-index watchers + result stores
├── code-next-edit/            # Code-intel package's next-edit crate
├── code-facts/                # Code-intel package's tree-sitter fact base
├── understanding-vocab/       # Atlas vocabulary — AtomsFile/AtomEnvelope, Edge, kinds, OntologyPolicies
├── understanding-atlas/       # Understanding's pure tier — arithmetic over the published language
├── understanding-host/        # Understanding's host tier — the ports and the knot
├── corpus-mcp/                # Thin knowledge host — recipe new · ingest · serve, any OpenAI-compatible endpoint
├── sovereign-recipes/         # Canonical recipe TOMLs + catalog (vendored into corpus-engine at build)
├── sovereign/                 # Local AI assistant (CLI / desktop / server / daemon)
├── commonwealth/              # Mesh coordination daemon
├── studio/                    # Liftable authoring package
├── quality/                   # Quality program — layer map, gate baselines, arch-layers crate
├── packages/chat-ui/          # Shared Svelte chat render surface (desktop + mobile)
├── packages/vscode-sovereign/ # First-party VSCode FIM extension
└── sovereign-mobile/          # Thin Tauri 2 mobile client (iOS + Android)
```

Outside the map: `vendor/` (pinned `llama-cpp-4`), `scripts/`, `docs/`,
`landing/`, `gym/`, `baselines/`, and `corpus-engine/xtask` — the gate
binaries plus the workspace-hygiene generators in `xtask/tests/`, which live
there because they read the repo root and so cannot sit in a liftable crate.
`models/` holds downloaded GGUF weights, gitignored.

| Project | Role | Depends on |
|---|---|---|
| `oicp-types` | OICP wire types + scoring helpers | — |
| `kernel-types` | The neutral kernel: identity and provenance (`ContentHash`, `CorpusId`, `NodeId`, `Origin`, `Custody`, `Attribution`), the trust vocabulary (`Verdict`, `Reason`, `Freshness`, `Judgement`), the released turn (`Seal`, `Citation`, `Draft`, `Answer`, `PeerAnswer`, `Refused`), the wire-form decider, the requirement registry. The SECOND layer-0 membrane beside `oicp-types`: oicp is what a node ADVERTISES, this is what content IS. May name nothing above it | `serde`, `getrandom`, `hex`, `blake3` |
| `workspace-hack` | cargo-hakari feature-unification crate, so a `-p` build resolves what `--workspace` resolves | — |
| `corpus-engine` | Acquire → extract → filter → chunk → embed → index | `oicp-types`, `kernel-types`, `corpus-index`, `corpus-engine-yield`, `corpus-engine-scip`, `corpus-engine-notes`, `corpus-engine-atos` |
| `sovereign` | Local agent runtime | `corpus-engine`, `corpus-engine-scip`, `oicp-types`, `kernel-types` |
| `commonwealth` | Symmetric mesh daemon | `corpus-engine`, `oicp-types`, `kernel-types` |

Dependency direction is one-way. Sovereign optionally embeds cmnwlth
in-process via `sovereign-mesh` — the only place the two upper projects meet.

```
       oicp-types          sovereign-recipes
            │                       │ build.rs include_bytes!
            │                ┌──────▼──────┐
            │                │ corpus-engine│  (LanceDB + Tantivy)
            │                └──────┬──────┘
            │                       │  EmbedFn / InferenceFn
            ├───────────┬───────────┼──────────────┐
        Sovereign       │      both call          cmnwlth
       (sovereign/)     │   identical APIs        (commonwealth/)
            │           │                              │
            └─ sovereign-mesh (in-process embed) ──────┘
```

Two protocols cross that boundary. **OICP** is declared in
`commonwealth/docs/oicp-v0.4.md` (v0.4 extends v0.3 additively), types in
`oicp-types/src/lib.rs`, re-exported as `sovereign_core::oicp` and
`commonwealth_core::oicp` — downstream crates use the re-exports.
**`EmbedFn` / `InferenceFn`** are closures `corpus-engine` accepts from any
caller; each project supplies its own.

`serving-policy` sits BELOW the boundary rather than crossing it: `fair_sched`
(fair-share caps, `EtaEwma`, reciprocity, the `SchedCore` the REST scheduler
and the mesh admission gate share) plus `pipeline_aliases`. Its in-repo
dependency list is empty, and `quality/ARCH_LAYERS.toml` forbids a dep on
`sovereign-*` or `commonwealth-*` in either direction.

---

## 2. Workspace map

One line per crate. For detail, read the crate's `lib.rs`; `sovereign/docs/`
holds the subsystem deep dives.

### corpus-engine

Between "raw source on the internet" and "ranked search hits with provenance."
See [`corpus-engine/README.md`](../corpus-engine/README.md),
[`ENRICHMENT.md`](../corpus-engine/ENRICHMENT.md) (the umbrella reconciling
all three enrichment systems — read it before assuming "enrichment" means one
thing), [`ENRICHMENT_V2.md`](../corpus-engine/ENRICHMENT_V2.md),
[`ATLAS.md`](../corpus-engine/ATLAS.md),
[`DECOMPOSITION.md`](../corpus-engine/DECOMPOSITION.md).

- `corpus.rs` — `Corpus`: which corpus, where it lives. The ONE decider of
  on-disk layout; ratchet `cargo xtask layout-gate`.
- `engine/` — `CorpusEngine` façade (`ingest`, `expand`, `reindex`).
- `acquirers/`, `extractors/`, `chunkers/`, `filters/` — pipeline stages.
- `asset_store/` — content-addressed store for binary payloads.
- `recipe.rs`, `registry.rs` — TOML schema + recipe catalog.
- `index/` — LanceDB (IVF-PQ) + Tantivy FTS, `IndexMeta`, `ScopeMeta`.
- `enrichment/` — v1 field engine, v2 atlas, `reconciliation/` (multi-origin
  merge with reversible oplog; signals are identity-grade only).
- `atlas_traversal/`, `update/`, `meta_atlas/`, `freshness.rs`, `pii.rs`,
  `alignment_projector.rs`.

**The wall clock has one decider per dependency island.**
`sovereign_core::time`, `sovereign-time` (the zero-dep leaf),
`corpus_engine_yield::time` and `commonwealth_core::clock` duplicate a
three-line body once each because they cannot import across one another
without a cycle. Everything else asks. Ratchet `cargo xtask clock-gate`,
shrink-only.

### sovereign

```
crates/
├── sovereign-contracts      # The vocabulary — traits, wire types, skills, setup config
├── sovereign-core           # Traits, runtime, planner, executor, router, memory
├── sovereign-inference      # llama.cpp slots, remote OpenAI-compat, hybrid, idle residency
├── sovereign-store          # SQLite + Postgres + in-memory StateStore
├── sovereign-tools          # Built-in tools (search, knowledge, docs, web, MCP, code-intel)
├── sovereign-gliner         # GLiNER (ONNX) NER — its own crate to keep ONNX off sovereign-tools
├── sovereign-atos           # ATOS lib — opt-in behind `--features atos`
├── sovereign-work-atlas     # Coordination atlas for agents on the mesh
├── sovereign-enrichment-catalog # The enrichment store below every host that reads it
├── sovereign-enrichment-build   # The enrichment orchestrator, outside the inference stack
├── sovereign-runtime-recipe # THE recipe that commissions a `Runtime` — all four hosts are on it
├── sovereign-turn-client    # THE client half of the turn protocol + reachability (`ServingHost`)
├── sovereign-mesh           # In-process cmnwlth embed; roster, rail, identity, gossip/ring loops
├── sovereign-daemon         # The node's host crate — assembly, surface shells, edge, adapters
├── sovereign-peer-wire      # Wire types both ends of an internal exchange must spell alike
├── sovereign-compute        # Supervised compute-child boundary — crash isolation, not parallelism
├── sovereign-pods           # Compute's remote isolation — leasing a rented machine
├── sovereign-scheduler      # Serving's pure tier — ranker, decision records, replay ("The two tiers", SERVING_BOUNDARY.md)
├── sovereign-serving-host   # Serving's host tier — peer_inference, admission, entry_endpoint
├── sovereign-grants         # GuestGrant, EphemeralGrantStore, `Scope` — per-turn authorization
├── sovereign-server         # Axum REST + WebSocket, multi-tenant (the phone-facing host)
├── sovereign-desktop        # Tauri 2 + Svelte 5
├── sovereign-cli            # User-facing dispatcher — execs into sibling binaries
├── sovereign-cli-shared     # Shared lib (dirs, repo, help, prompts, tracing init, cli-contract)
├── sovereign-cli-daemon     # Long-running host + lifecycle; owns Windows GPU backend selection
├── sovereign-cli-dev        # Workbench: ATOS + project lifecycle + code intel + tools
├── sovereign-cli-llm        # Model interaction + heavy retrieval (chat/bench/eval/atlas/mesh/ring/job)
├── sovereign-time           # Wall-clock helpers — zero-dep leaf for crates off sovereign-core
├── sovereign-pipeline       # Pipeline / pod-lifecycle helpers
├── sovereign-eval           # Pure scorers
├── sovereign-authoring-harness # Recipe-authoring verdict ladder over harness StageOutputs
├── sovereign-meshapp        # Mesh-app explorer ops — pure path-in/DTO-out lib
├── sovereign-meshapp-registry  # Mesh-app manifest, registry, port map, proxy
├── sovereign-mesh-test-harness # SimulatedMesh/SimulatedNode/MockLlamaServer, fault injection
├── sovereign-service        # Service installation (launchd / systemd / Windows task)
├── sovereign-agent-bench    # Eleven-problem agent-coding battery
├── sovereign-agent-tools    # Canonical agent-tool primitives (cross-runner contract)
└── sovereign-tdd            # Unified TDD solver loop (HTTP + MCP transports)
```

Top-level: `modes/` (skills), `models.toml`, `models/`, `bench/`,
`inquiries/`, `router/`, `sovereign-server.toml`, `deploy/onprem/`.

### commonwealth

```
crates/
├── commonwealth-core         # Shared types — ids, mesh, capabilities, ledger, clock
├── commonwealth-transport    # PeerTransport seam — (peer, traffic class) → endpoints
├── commonwealth-discovery    # Founding + joining: join keys, mDNS, local hardware survey
├── commonwealth-rail-core    # The ring rail's FOLD — Person/Roster/RailAct/SignedOp. Zero I/O
├── commonwealth-rail         # The ring rail's JOURNAL — one JSONL log per namespace
├── commonwealth-work         # The WORK PLANE on the rail — WorkAct codec, unit seal, lease predicate
├── commonwealth-state        # MeshStore — SQLite KV; a local PROJECTION of the ring rail
├── commonwealth-media        # Federated media — who offers a library, who may reach one
└── commonwealth-rails        # `cw-rails` — the minimal daemon a shim author installs
```

Nine crates, and nine is the whole directory. Six left in 2026-09 because
their names described a family they were not in: `commonwealth-api` and
`-inference` became `sovereign-api` / `sovereign-serving` and were then
deleted; `-knowledge` became `sovereign-grants`; `-app` became
`sovereign-meshapp-registry`; `-test-harness` became
`sovereign-mesh-test-harness`; `oicp-conformance` moved to a repo-root
sibling. `contrib/` ships `install.sh`, a systemd unit and a launchd plist.

### studio

The liftable authoring package, buildable against only the OICP contract
crates, enforced by `cargo xtask boundary-gate` (contract
`studio/BOUNDARY.md`). One of four declared packages — the others are
`code-intel` (`docs/CODE_TOOLING_BOUNDARY.md`), `corpus-mcp`
(`corpus-mcp/README.md`) and `commonwealth` (`commonwealth/BOUNDARY.md`).
Crate sets and shared-leaf budgets are `[[package]]` / `[[package_leaf]]`
blocks in `quality/ARCH_LAYERS.toml`, beside the layer map and behind the same
parser, so layer-gate, boundary-gate and `arch_report` cannot drift on what a
boundary means.

```
crates/
├── sovereign-workflow       # Step·Artifact·Runner — typed dataflow over local-model steps
├── sovereign-workflow-host  # Daemon-runnable workflow host + the NL workflow-author bundle
├── sovereign-tools-base     # Pure leaf workflow tools (shell/web/chunk/file/json/csv/zip/vector/MCP)
├── sovereign-recipe-author  # Recipe-authoring tool bundle + RecipeProject model + project store
└── sovereign-studio         # Headless studio CLI — the proof the package is independently usable
```

### quality

`ARCH_LAYERS.toml` is the declared layer map, enforced by `cargo xtask
layer-gate` (Cargo-declared edges) and the code-intel arch report
(SCIP-observed edges); `arch-layers/` is the shared evaluator both use.
`baselines/` holds machine-written ratchet baselines, regenerated only via
`--update-baseline`, banked via `--tighten`. `cargo xtask quality` runs every
fast local gate with one table carrying FOUR verdicts: passed / failed /
could-not-judge / never-ran.

`twin-plants.toml` + `scripts/twin-census.py` are the sabotage runner for the
one-decider censuses: prove the census green, apply a real second
implementation, require a FAIL naming the expected substring, restore
byte-for-byte. 19 families.

Also here: `CONCEPTS.toml` (the concept register), `TARGET_ARCHITECTURE.md`,
`env-flags.toml`, `requirements.toml` + `requirements-enforceability.toml`,
`instruments.toml` (every instrument in the repo, one table), `DOMAINS.md`,
`DELETION.md`, `CLEANUP.md`, `REFACTOR_FACTORY.md`, `REFACTOR_LEDGER.md`.

### sovereign-recipes

The single source of truth for corpus recipes; corpus-engine vendors the tree
at build time, so there is no second copy. Catalog is `registry.toml` (27
recipes: the `wikipedia*` family, `sep`, `stackexchange*`, `openalex`,
`gutenberg*`, `crs_reports`, `us-code`, `olc-opinions`, `scotus-opinions`,
`federal-register-presidential`, `conversations-anthropic`,
`conversations-chatgpt`, `sec-filings-company`, five `enron-sample*`, three
`uap-blue-book*`). Field reference is `SCHEMA.md`, generated from `recipe.rs`
and gated by the `recipe_schema` test. Outside the catalog: `codebase`,
`arch-principles`, `system-overview`, `chaos-secret-agent`, `chaos-saltgrass`,
`maple-house`, `proxy-company`, `search-gym`, `sf-assessor-roll`.

### Bench harnesses

Fixtures under `sovereign/bench/`; orchestrators in `bench_cmd/`; pure scorers
in `sovereign-eval/`.

`scripts/sovereign-ci-bench.sh` is the full nightly (~2-4h) and **the primary
way to catch a regression anywhere in the inference + retrieval stack** — one
command spanning retrieval recall, enrichment atom-F1, intent routing,
synthesis answer-equivalence, tool-use gyms, multi-turn degradation, chaos
honesty, mechanism fidelity and governance, each diffed against a committed
baseline. Deterministic baseline-diffed lanes are **hard** (build-breaking);
the synthesis judge lane is **soft**; chaos, mechanism, multi-turn and
governance run **tracked** (advisory), each paired with a hard
`svrn bench gate <lane>` failing only on regression.

Named harnesses: **model attribution** (resolves a slot alias to the concrete
GGUF at run time, so a baseline is not worthless once the alias is repointed);
**reasoning-fidelity** (`bench mechanism-fidelity`, metamorphic, with a
provably-blind negative control); **Chaos-Monkey** (answer-when-present /
abstain-when-absent under a two-red-line scorer that never blends the two);
**the rubric core** (`bench_cmd/rubric/`, shared forced-choice-per-criterion
apparatus with a calibration gate and Wilson-CI reporting; tenants `bench
moral` and `bench situated`); **governance** (FR-9); **inner-work chaos**
(`eval inner-chaos`, two-tier scored and never averaged).

---

## 3. corpus-engine — the shared knowledge layer

Both upstream projects use it through the same public API; neither knows the
other exists.

```
Acquirer → Extractor → Filter → Chunker → Embedder → Index
                                          (caller-supplied EmbedFn)
```

| Stage | Built-ins |
|---|---|
| Acquirer | `bulk_download`, `huggingface_dataset`, `local_file`, `http_api`, `web_crawl`, `custom` (runtime-registered seam) |
| Extractor | `mediawiki_xml`, `stackexchange_xml`, `jsonl`, `json`, `markdown`, `xml_sections`, `wikipedia_jsonl`, `wikipedia_structured`, `wikipedia_catalog`, `wikipedia_api_article`, `gutenberg_catalog`, `html`, `html_sections`, `csv`, `parquet`, `plaintext`, `code`, `email`, `anthropic_export` / `chatgpt_export`, `alignment_workspace`, `custom`, `described_asset`, `tabular_atoms`. `ExtractorConfig` in `recipe.rs` is the SSOT. `column_aware` is an *enrichment-time* extractor, not a recipe `type =` value |
| Filter | `pageview_rank`, `title_list`, `knowledge_density`, `boilerplate`, composed via `[[filter]]` (`Any` / `All`) |
| Chunker | `paragraph`, `sentence`, `fixed`, `semantic`, `passthrough`, `portal_event_bullet`, `threaded_turns` |
| Index | `CorpusIndex` over LanceDB (IVF-PQ) + Tantivy FTS |

**No confabulated numbers, on the corpora that carry typed figures.** The SF
land-value demo (`tabular_atoms` + `parcel_analytics`) and the SEC corpora
(`sec_facts`) share one contract: the model narrates only the tool's compact
figures, the synthesizer appends the tool's `derivation` verbatim as
system-rendered text, and `runtime::numeric_audit` value-matches every figure
in the prose against the tool's outputs. Financial answers carry BARE
numerals, so `sec_facts` declares an opt-in bare-numeral audit; on violation
the narration is WITHHELD and replaced by the tool's own rendering.
`runtime/authority_guard.rs` binds that audit to the ANSWER EXIT on every
dispatch surface rather than to a routing decision. Detail
`docs/specs/FINANCIAL_CORPORA.md`.

### Storage

LanceDB vectors + Tantivy keyword; one dir per corpus, identical schema for a
full index or a shard.

```
~/.svrnmesh/indexes/<corpus>/
├── _corpus_meta.json        # authoritative metadata
├── chapters.json            # sections + the chunk_ids join
├── chunks.lance/
├── assets/                  # content-addressed asset store (raw + parsed + ledger)
└── atlas/
    ├── atoms.json           # AtomsFile — the canonical export
    ├── atoms.lance/         # columnar atom store — the query-path reader
    ├── edges.csr            # mmap'd CSR adjacency (+ .derivation marker)
    ├── atoms_ann.lance/     # the walk's ANN seed table
    ├── atoms_ann.population # what that table was seeded UNDER
    ├── ontology.json        # what the atlas was extracted under
    └── _summary.json        # counts, ontology summary, ANN + edge census
```

`wikipedia` is WIKI-CLASS and differs: an `articles.lance` + `edges.lance`
pair instead of an atom store, because its per-edge strings cannot fit a
10-byte CSR record. Which face a host gets is not the host's choice —
`enrichment::atlas::open_walk_provider` is the one decider, so a corpus cannot
be walkable to one caller and invisible to another. For a wiki-class atlas the
seed table is REQUIRED, not an optimisation: seeding has two sources, the ANN
table and name-matching over an atom bag, and a wiki store has no bag.

`(corpus_id, chunk_id)` is the citation handle and is structurally unique.

**Three readiness questions, three accessors — do not conflate them.** A
directory under `indexes/` is not an installed corpus; an ingest in flight
writes `<corpus_id>-partition-<node_id>/`. `installed_indexes()` answers "is a
writer active" (the resume paths). `usable_indexes()` answers "can I search
this", filtering on `IndexInfo::indexes_built` and tracing every drop.
`corpus_readiness` (`ready` / `building` / `absent`) is what `corpus status`
and `corpus install --wait` share.

### Index maintenance — the decay nothing reports

lancedb answers a query by running the index over indexed data AND a flat scan
over everything appended since. Nothing fails: results stay correct, no error
is logged, every ANN knob reports healthy. It only gets slower, and mutation
rate predicts the decay. Two surfaces, one implementation
(`corpus_engine::index::maintain`): `svrn corpus optimize` for the operator,
`sovereign-daemon/src/corpus_maintenance.rs` for the product, because the
person who most needs a healthy corpus will never open a terminal.

Three invariants that are easy to get backwards:

- **`OptimizeAction::Index` is NOT idempotent.** Every unconditional call
  writes new index versions and removes none, so the phase is gated on
  `unindexed_rows_before > 0 || fragments_removed > 0`. Ungated on a cadence,
  the healer is the leak.
- **Compaction is non-destructive, so disk GROWS until you prune.** Pruning is
  destructive and irreversible, so the CLI has no default.
- **Age alone bounds nothing on an appended corpus, and the two gates must
  stay separate.** `Retention { min_age_days, keep_versions }`: age is reader
  safety, count is the space bound. Getting the pair wrong cost 153.9 GB.

### Injection contract

`corpus-engine` never embeds or generates text itself.

```rust
pub type EmbedFn     = Arc<dyn Fn(&str) -> Pin<Box<dyn Future<Output = Result<Vec<f32>>> + Send>> + Send + Sync>;
pub type InferenceFn = Arc<dyn Fn(&ChatPrompt, Option<u32>) -> Pin<Box<dyn Future<Output = Result<String>> + Send>> + Send + Sync>;
```

`InferenceFn` is the ONE completion closure port. Sovereign wraps its local
slots; cmnwlth wraps `/v1/embeddings` and the mesh inference endpoint; tests
use a zero-vector and a canned-JSON mock.

Default embedding model `qwen3-embedding-0.6b` (1024 dims,
`DEFAULT_EMBED_DIM`); `_corpus_meta.json` records it and a mismatch fails with
`Error::IncompatibleEmbedding`. The Embed slot is a cross-peer
interoperability contract — nodes sharing a corpus must produce
bit-compatible vectors (`EmbedModelInfo` must match) — and
`sovereign_contracts::embed_quirks` is the one decider for how an input is
assembled (`prepare_document` / `prepare_query`).

### Sharding, budget, peer-assisted ingest

`index_stats`, `extract_shard`, `merge_shards`; shards are structurally
identical to full indexes. The per-node ceiling set in Settings → Knowledge is
enforced once, at `build_local_capabilities`, which clamps published
`free_storage_gb`; every scheduler reads that one value.

**Blanket** hands a chosen subset of peers a one-time, revocable, ephemeral
grant to shoulder compute for a personal source, riding the existing
collaborative-ingest work queue. The on-disk metadata is never mutated — that
IS the "no standing share". Four parts: `CorpusMeta.grantable`,
`CollaborateRequest.allowed_peers`, `EphemeralGrantStore`, and teardown with
`verify_merge_sample` re-embedding a sample locally to cosine-check
peer-produced vectors.

### What a retrieved chunk vouches for

`ChunkProvenance` is a required field with no `Default` and no `Deserialize`.
`Acquired(Acquisition)` is stamped by a door; `Manufactured { producer, grain }`
names what built it and is **not citable**. The `Acquired` arm has no public
constructor, so `sovereign` reads provenance and writes only `Manufactured`.
Doors: `CorpusIndex::search`, `acquire_chunks`, `acquired_from_estate`, and
`acquired_from_peer`, which JOINs the peer's custody claim with this node's
own `Custody::Peer` at maximum restrictiveness. `custody()` asks what class
this is; `stamped_custody()` asks whether a door recorded one; `grain()`
answers leaf-vs-summary. Ratcheted by `chunk_provenance_census.rs`.

### The chunk → section join

`ChapterEntry::chunk_ids` in `chapters.json` connects what retrieval carries
(a LanceDB row id) to what the system cites (a section). Three production
readers depend on it: `chunk_to_section_map`, the retrieval pipeline's
governance active-set step, and the atlas mesh-app adapter's `read_chunk`.
`JoinStatus` has three states, not two — `NoSectionStructure`, `JoinMissing`,
`Present` — and conflating the first two is what let it rot invisibly.
`svrn enrich backfill-sections` fills it; `svrn corpus snapshot publish`
REFUSES an unjoined bundle, because a downloader has `chapters.json` and no
source document and so can never repair it.

Only an `Exact` quote match may carry a locator, and it releases the SOURCE's
own characters rather than the model's copy, so a labelled quote cannot be
demoted by the strict post-hoc re-check. Locators sit OUTSIDE the quote marks
and are deliberately not folded into `chunk_labels`.

### Enrichment

**Three coexisting systems**, selected per corpus by `[enrichment] type` and
resolved through ONE table, `engine/pass.rs::EnrichmentPassRegistry`. Every
question the pipeline asks about a type is a method on the resolved
`EnrichmentPass` (`runs_at_install`, `declared_artifacts`, `resumable_at_boot`,
`produces_atoms`, `run`). An unregistered type is refused at recipe load.

- **`field_model`** — five-phase *whole-corpus* pipeline (skeleton → cluster →
  align → fault lines → open questions) over a `Domain` trait. Its artifact is
  the atlas: it publishes `Question` and `Position` atoms, with
  `field_skeleton.json` as a migration fallback for the three KnowledgeView
  domains whose reader has not moved.
- **`atlas`** — *per-document* typed atom graph over a `Pipeline` trait +
  registry + `ExemplarBank` + `PhaseCache`. Built-ins `literary`,
  `literary_atlas`, `philosophy_atlas`, `referential_atlas`,
  `engineering_atlas`, `conversation_atlas`, plus `custom_atlas`, the
  recipe-declared genre from a versioned `[enrichment.ontology]` block. A
  version-1 declaration drives the Phase-1 prompt, the generated response
  schema, the parser's `ParsePolicy`, resolution, reconciliation identity and
  the navigation map. **Both tension axes degrade by REPORTING, never by
  enforcing a criterion the extraction did not fill.** Every pipeline writes
  `atlas/ontology.json`, so a reader can tell an author's declaration from a
  genre writing its fixed vocabulary down; built-in vocabularies are DATA at
  `pipelines/ontologies/<id>.toml`.
- **`tiered`** — three progressive tiers (T1 embeddings → T2 entity-graph +
  PPR → T3 RAPTOR cluster tree). The RAPTOR builder is in
  `sovereign-tools/src/raptor_atlas.rs`, injected via
  `TieredEnrichmentProvider` to avoid a cyclic dep. GLiNER augments the
  conversation path through the `LabeledEntityExtractor` seam. **The NER seam
  is input-bounded** — `BoundedInputs` caps batches at 16 chunks and holds
  back anything over 2,048 chars; an over-cap chunk is REPORTED
  (`refused_over_cap_chunks`), never truncated, and a large count is a signal
  about the CHUNKER, not a reason to raise the ceiling.

`Summary` is the twelfth atom kind and the only one whose `AtomType::grain` is
`Grain::Summary` — the atlas face of one RAPTOR node. **The walk holds Summary
seeds OUT of leaf scoring**: a Summary seed does not expand, its reach never
accumulates into a leaf chunk's evidence score, and its text leaves on
`Grounding::summaries` without consuming the walk's budget. Injecting
summaries early cost 14 points of source coverage when it was measured. RAPTOR
is COMPOSED rather than deleted — the navigation row's `summary_sources` list
(`[atoms, raptor]` by default) asks each source through one
`SummaryStage::supply`.

**A DEAD enrichment is never resumed automatically.**
`EnrichmentState::declared_dead()` is the one decider (`phase == Stalled`, or
any stamped error); four boot-time scans consult it, and resume is an explicit
operator action via `POST /internal/corpus/enrich-reset`.

### Registry, authoring, back-compat

Resolution order is local override on disk → remote → bundled, SHA-256
verified when the entry declares one. `build.rs` vendors `registry.toml` into
`OUT_DIR`, so the engine works offline with no checked-in snapshot to drift.

The schema is open — a domain expert authors a TOML and the engine runs it.
Generic primitives: the `http_api` acquirer (URL templating, four pagination
strategies, bounded-concurrency document follow, token-bucket rate limit),
`[recipe.parameters]`, the `html_sections` extractor with a `MissReport`
sidecar, the authoring-harness verdict ladder, the investigation pipeline
(recipe-declared entity/relationship types → JSON-Schema → llguidance
grammar), and the recipe-author agent loop. Lifecycle
`svrn recipe {validate,test,publish,list}`.

Recipes live outside the repo, so a TOML written six months ago must keep
loading: new fields carry `#[serde(default)]`, renamed fields keep the old
name as an alias, removed variants get a deprecation arm in
`translate_parse_error`, and `[corpus] schema_version` bumps only when readers
must opt in. Enforced by `corpus-engine/tests/recipe_back_compat.rs`.

Delta updates are `update/delta.rs`: per-document revision ids,
`ManifestDiff::compute`, three-phase apply, `_update_progress.json` for
resume. Crawl safety is hardcoded and not per-recipe: robots.txt compliance,
1s/domain rate limit, a declared UA, scope enforced against the seed domain.

---

## 4. Sovereign — the local agent

Desktop, CLI, HTTP server or daemon against the same `Runtime`. No data leaves
the machine unless the user opts in to web search or a mesh.

### Trait architecture

`sovereign-contracts/src/traits.rs`, re-exported as `sovereign_core::traits` —
the contract crate is carved out so packages build against the vocabulary
without the runtime hub.

| Trait | Surface |
|---|---|
| `InferenceProvider` | `complete`, `complete_stream`, `complete_stream_with_id`, `embed`, `embed_query`, `capabilities`, `code_model_id` |
| `Router` | `classify(message, ctx, tools) → RouterClassification` |
| `Planner` | `plan(goal, context, tools)`, `replan(...)` |
| `Tool` | `descriptor`, `execute`, `validate`, `retry_config`, `required_permissions` |
| `LandscapeDigestProvider` | `splice_landscape_digests(ctx, active_skill)` |
| `ApprovalChannel` | Human-in-the-loop tool approval (CLI / Tauri / Server / Auto) |
| `MeshKnowledgeSource` | Fan-out to peers → `MeshSearchOutcome` (hits **and** the corpora it could not reach) |
| `SensitiveCorpusOracle` / `FolderMetadataOracle` | Watched-folder privacy + UI surface |
| `InsightStore` / `InsightSink` | Long-term insight extraction + persistence |

`StateStore` is decomposed per ISP into 12 sub-traits aggregated by a blanket
impl (`ConversationStore`, `TaskStore`, `MemoryStore`, `RoutingStore`,
`DocumentStore`, `CorpusStateStore`, `BudgetStore`, `PermissionStore`,
`StepExecutionStore`, `HealthStore`, `DocumentSessionStore`,
`DocumentAssetStore`). Callers narrow bounds to what they need.

### Runtime data flow

```
User message
  → Router.classify           (Quick slot, two-pass coarse → refine)
  → decide_policy(classification, ConfidenceThresholds)   (pure fn)
       → RoutingPolicy { tier, move_kind: Commit | Propose | Ask, … }
  → SessionStore.begin → QuerySession (CancellationToken-bearing)
  → Dispatch by Intent:
       ├─ SimpleQuery / DeepQuery / KnowledgeQuery → search → synthesize
       └─ ComplexTask → Planner.plan (Main slot)
                      → Executor (topological batches)
                          ├─ ReasonWithTools loop
                          ├─ Best-of-N sampling (LlmJudge / Random / Best)
                          └─ Tool steps with permission + approval
  → Provenance recorded into Message.metadata
  → Memory extraction at conversation end
```

The router emits **facts**; the runtime applies **policy**. Splitting them
keeps classification testable without a model and lets thresholds calibrate
without touching the trait.

**Every per-intent attribute is a column of `IntentRow`** (`Intent::row()` in
`sovereign-contracts/src/types/routing.rs`): recorded name, wire slug, banner
and chip phrasings, trace label, OICP `(capability hint, latency class)`,
retrieval slot with and without evidence, output-budget floor, referential
`Operation`, `ToolAccess`. Adding an intent is a variant, a row, and exemplars
in `sovereign/router/exemplars.toml`. `IntentRow` has no `Default`, so a row
omitting a column does not compile. What did NOT move into it: handler
dispatch (control flow over a closed set is what enums are for), payload
guards, and `authority_guard::guard_story`.

`Plan` is a flat JSON DAG; `StepKind` is `Reason`, `Tool`, `UserInput`,
`Branch`, `ReasonWithTools`, `AwaitUserInfo`, `Delegate`. The one textual
grammar planner and executor share is the `{N.key}` placeholder, owned
end-to-end by `sovereign-core/src/plan_grammar.rs`.

**Idempotency ledger.** Before a `NonIdempotent` tool step runs the executor
writes a durable `Started` row keyed by a CONTENT-derived key
(`task:tool:hash(params)`, not `(task, step_id)`, so it matches across a
replan that re-issues the same action under a new id). A `Completed` row on
resume means the action already ran; a `Started`-but-not-`Completed` row means
a crash interrupted it, so the executor halts rather than blind-replaying an
email send.

**Delegate is the context firewall.** It runs a scoped tool loop in its OWN
context: raw observations accumulate in the worker's local transcript and only
a typed contract — the `return_schema` fields plus an always-present
`anomalies` channel — flows back. `crate::tool_loop` is the crate's ONE
tool-call protocol, and all four loops drive it.

**The router classifier stack has one wiring path.** Before the coarse→refine
LLM cascade, `classify` consults five embedding-centroid pre-checks — embed
router, scope, effort, current-info, archive — plus a locator axis scored
one-vs-rest. All are assembled by `router_bootstrap.rs::build_llm_router`,
which EVERY surface calls, and `router_bootstrap_parity.rs` asserts
`all_wired()`. Exemplars are `include_str!`'d so the stack works regardless of
CWD or bundle layout.

**Two embedding spaces, not interchangeable.** `router_instruction.rs` is the
single decider for both the instruction text and the axis→space map. Intent
and locator run in a **speech-act** space (what the speaker is DOING); scope,
archive and current-info run in the **retrieval** space, because they separate
their classes by subject matter — precisely the signal a speech-act
instruction deletes. Thresholds are comparable only within a space, and the
embed cache keys the instruction into its hash. Per-turn embed count is three.

`svrn router fit` is the calibration surface, sweeping exhaustively with
candidate thresholds at midpoints between observed scores, against
`bench/routing/calibration/axes_v1.toml` — a bank authored to fail somewhere
(74 cases, 32 `expect = "abstain"`). Two guards keep it honest: a margin floor
clamped to ≥ 0, and `FitReport::underpowered()` on any axis with fewer than
five cases per class. **The command writes no constant** — it names the
constant and the file and stops.

**Synthesis and the grounding gate.** The knowledge-turn path is organized by
`role.rs` (the resolver returns a `role::Tier`, load-bearing rather than
declarative). `runtime/grounding/` is the production gate: per-claim fail-open
accounting, a concurrent bounded claim fan-out, an audit pass with a plan and
an outcome, a citation stage whose support decider is the gate's own judge,
and a value-presence veto. Every fail-open exit names WHY at one site. Judges
run against ONE register, enforced by `cargo xtask judge-funnel-gate`, and a
register change is priced in both directions or it is not judged.

**Retrieval** is `runtime/retrieval_pipeline.rs`, a step ledger where every
step accounts for what it did to the pool. `apply_atlas_grounding` is a CALLER
of `corpus_engine::enrichment::atlas::ground`, the same walk `corpus-mcp`
drives, so the two cannot diverge. The evidence budget is spent ACROSS the
ideas the walk reached, not down the ranked list.

**An answer over missing knowledge says so, and CODE guarantees it.**
`UnavailabilityReason` is a closed enum, `corpus_unavailability()` is the one
readiness decider, `append_unavailability_marker` is appended by code rather
than asked of the model, and a turn that lost nothing renders BYTE FOR BYTE as
before. Per-turn stack attribution is derived from observed execution, never
from flag values.

### Inference

`sovereign-inference/src/embedded/` wraps `llama-cpp` with lazy-loaded slots
(Quick / Main / Code / Embed). Hybrid and remote providers wrap
OpenAI-compatible servers. Full detail
[`docs/inference.md`](./docs/inference.md).

**Which engine serves this node is config, not code.**
`engine_factory::build_engine(&SetupConfig)` is the ONE place `[engine] kind`
becomes an `Arc<dyn InferenceProvider>`. The vocabulary is one layer down in
`sovereign-contracts`, so an out-of-tree engine can name the selection without
naming any implementation. `llama` and `remote` are typed variants; anything
else is `EngineKind::Custom(name)` resolved through `register_engine`, and an
unknown id refuses listing what IS registered rather than falling back.
`BuiltEngine.llama` is `None` for every non-llama engine, and the VRAM
preflight is llama's own question, skipped for engines holding no weights.
The contracts are executable: `engine_conformance::{check_sync, check_serving}`.

**Residency is a policy.** `embedded/idle_slot.rs` is the one idleness decider.
It exists because the daemon is a MESH NODE and must stay available to peers
while the app is closed, which makes idle-EXIT impossible and idle-UNLOAD the
only answer to resident RAM. **The unit of residency is the FAMILY, not the
slot**: `fast` shares its `Arc<LlamaModel>` with `fast_short` and, in alias
mode, with `primary`, so dropping a slot frees a KV cache and leaves the
weights — "fast unloaded" in the log is not yet "memory returned".

**`sovereign-compute` is a process boundary, and its value is crash isolation
plus the can't-fit-one-box case, NOT throughput.** For a model that fits one
box, N process replicas LOSE to in-process multi-sequence batching — the
replica-pool path is a demonstrated dead end for parallelism.
`[compute] distributed_primary` is the payoff: the daemon plans and warms, a
`DynamicChildSlot` owns the primary, and the handoff ships the PLAN rather
than the worker list, so plan agreement survives the process boundary.

### Tools

| Tool | Purpose |
|---|---|
| `SearchTool` | Local vector + FTS5, coverage assessment, optional web fallback |
| `WebSearchTool` / `WebFetchTool` | NL → keywords, fetch, synth with citations; single-URL fetch + HTML→text |
| `KnowledgeTool` | Direct corpus query |
| `ClaimSearchTool` / `EpistemicLandscapeTool` | Enriched-corpus retrieval |
| `DocumentTool` | Map-reduce summarize/analyze |
| `ShellTool` / `FileTool` / `EmailTool` / `CalendarTool` / `ComputeTool` | Standard tools (sandbox + approval) |
| `McpClient` + `McpToolAdapter` | stdio JSON-RPC + HTTP+SSE; wrap remote MCP servers as native tools |

**The declared half of a tool is data.** Identity, behavioural properties
(`effect` / `idempotency` / `latency` / `scope`), parameter schema, examples
and required permissions live in `sovereign-contracts/tool-manifests/*.toml`;
56 impls read it, so a tool's declared facts have one decider. A manifest
declaring `delegate` plus `defaults` needs no Rust at all. Five tools keep a
coded descriptor because theirs is derived at runtime.

External MCP servers are configured in `[[mcp_servers]]` and loaded by the one
shared loader `sovereign_tools::mcp::load_from_setup_config`, which every chat
surface calls. `McpToolAdapter` infers effect and idempotency from a tool's
name, so a browser `click` picks up the approval gate and replay ledger while
a `snapshot` read does not.

**Code intelligence** is served over MCP by `svrn project serve` or the
daemon. Tools under `sovereign-tools/src/code/`: the code index (`symbols`,
`code_search`, `recent_changes`, `working_set`, `brief`), the session brief
(`briefing`), the tree-sitter fact base (`facts`), the SCIP call graph
(`callers`, `callees`, `blast_radius`), watchers, notes, ATOS lifecycle,
drift, capability docs, project context, session reflection, and work-atlas
coordination (`declare_scope`, `release_scope`, `work_in_flight`).

The daemon's tool graph and the reindexer share ONE merged `ScipGraph` handle,
so updates are visible to `symbols`/`callers`/`blast` live. Each debounced
save runs an embed-free tree-sitter overlay; the heavy rust-analyzer export is
demoted (spawned, rate-limited, quiescence-gated, `nice +10`) and is
one-writer, staging and renaming under a cross-process flock so a query in
flight always sees a complete graph. **One project owns one workspace**:
registration refuses a root that is an ancestor or descendant of a registered
one, because nested registrations collapse the freshness pipeline.

**Capability docs** derive what the codebase does from the SCIP call graph and
reconcile it against the prose: `code capability-map` clusters entry points
sharing a call spine, `enrich capability-doc` narrates each with every spine
function cited, `enrich capability-reconcile` produces corroborated /
undocumented / drifted findings. The deterministic floor runs in public CI as
`cargo run -p xtask -- docs-gate`.

### State, memory, skills

- **State** — `sovereign-store` provides `SqliteStateStore` (default),
  `PostgresStateStore`, `MemoryStateStore`. Every record carries a Lamport
  `version`; soft-deletable rows have `deleted_at`, so two stores can
  union-merge without schema migration.
- **Conversation memory — three constant-capacity channels.** The rolling
  visible window; the **conversation frame** (`conv_frame.rs`, five named
  sections at a 320-token budget, folded incrementally off a watermark); and
  **retrieval-over-history** (dropped turn-pairs embedded once and memoized,
  hybrid-scored, MMR-selected). Total ~2.8k tokens independent of conversation
  length. The frame replaced a prose blob because a blob has to be rewritten
  to be updated and is not renderable.
- **Long-term memory** — extracted at conversation end, FTS5 retrieval,
  exponential monthly decay. Tiered recall reads persistent T1 embeddings and
  blends a T3 memory-RAPTOR signal with per-scope trees.
- **Skills** — TOML under `sovereign/modes/`. `SkillRegistry` merges routing
  hints, planner templates, prompt overrides, memory rules and OICP
  requirements; skills carry `signature` / `signed_by` and a `TrustLevel`.

### Frontends

<<<<<<< HEAD
| Frontend | Notes |
|---|---|
| `sovereign-cli` (+ siblings) | Dispatcher. `sovereign <verb>` execs into `sovereign-cli-daemon`, `-dev` or `-llm`. Unix execs (same PID); elsewhere spawn-and-wait. Discovery is `current_exe()`'s parent, overridable per sibling |
| `sovereign-server` | Axum REST + WebSocket, multi-tenant with per-tenant isolation on corpora and documents. Binds `127.0.0.1:8080`; a non-loopback bind with `[auth]` disabled is refused at startup. **Two cargo features, both default ON, drop the surfaces whose safety rests on one operator owning the box**: `dev-routes` (privilege — `/v1/solve`, uploads taking a server-side path, the `/mcp*` routes, `ShellTool`) and `net-tools` (egress — the search tool's web fallback, `web_fetch`, `wikipedia_fetch`) |
| `sovereign-desktop` | Tauri 2 + Svelte 5, rail `Ask · Library · Reflect · Workshop · ⚙`. Layout is token-driven: `app.css` owns the scale and three global primitives (`.page-body`, `.page-measure`, `.page-header`). Do NOT re-declare padding or overflow on an element carrying `.page-body` — Svelte scoping wins silently and clips content with no way to scroll to it |
| `sovereign-mobile` | Thin Tauri 2 client — no local inference, Runtime or corpus. Consumes `sovereign-turn-client` and nothing else on the wire. Named ceiling: the client family has no auth seam, so the phone reaches a DAEMON, not an api-key `sovereign-server` |
=======
| Frontend            | Purpose                                                                              |
|---------------------|--------------------------------------------------------------------------------------|
| `sovereign-cli` (+ siblings) | User-facing dispatcher. `sovereign <verb>` execs into one of three siblings — `sovereign-cli-daemon`, `sovereign-cli-dev`, `sovereign-cli-llm` — based on the verb. Since 2026-08-21 one verb is the exception: `code converge` is served in-process from `sovereign-cli-dev`'s `[lib]` target (linked, `default-features = false`) — see `InProcessCodeVerb` below. Same UX as one binary; faster builds. Discovery: each sibling at `current_exe()`'s parent dir; override via `SOVEREIGN_CLI_{DAEMON,DEV,LLM}_BIN`. Unix execs into the sibling (same PID); other platforms spawn-and-wait. |
| `sovereign-server`  | Axum REST + WebSocket on configurable port; multi-tenant via `tenant.rs` with per-tenant isolation on corpora and uploaded documents (`ConversationContext.corpus_ceiling` scopes retrieval incl. the round-0 engine search; `DocumentAsset.owner` gates document list/get/delete/ask — the SaaS-hub hardening, 2026-07); server-side `ApprovalChannel` w/ `/v1/tasks/{id}/approve`. **`POST /v1/admin/shutdown`** (2026-09-11, sv-surface svt-2) is how this process is told to stop: inside the auth layer, so it takes the same bearer key every `/v1` route takes and mints no second credential, and REFUSED with a named 403 when `[auth]` is disabled — the layer is a pass-through then, and this binary defaults to a `0.0.0.0` bind. `axum::serve` gained `with_graceful_shutdown` on it plus a 5 s watchdog, because a held-open conversation WebSocket is in-flight for as long as the phone keeps it and would otherwise make an accepted stop indefinite. Before this the binary had NO stop path at all — no route, no signal handler, no pidfile, no run lock — and the only thing that ever stopped it was an external SIGKILL from the desktop's Mobile-access toggle holding its `Child`. **Mobile-facing surface** (`docs/specs/MOBILE.md`): WS `/v1/conversations/{id}/stream` streams `TurnFrame::Token`→`Complete` token-by-token down the requesting socket (not the shared broadcast — avoids cross-tenant leak, and since 2026-08-25 the two channels no longer share a TYPE, so that leak does not compile: per-turn frames are `sovereign_contracts::types::TurnFrame`, the executor's fan-out is the server-local `ExecutorEvent`); `sovereign_contracts::types::projection` surfaces typed `provenance` + `citations` on REST message responses — it moved out of this binary with the protocol so a daemon can project the same metadata; `GET /v1/corpora` lists `CORPUS_REF`s (Knowledge-only, with `scope`/`mesh_shared` privacy posture derived from `IndexInfo.mesh_sharing`); a `scheduler.rs` `FairScheduler` bounds concurrent turns — a weighted-fair queue + per-origin cap with live `TurnFrame::QueuePosition` over WS and `503 + Retry-After` shed (`busy.rs`) on REST, sharing its `serving_policy::fair_sched::SchedCore` policy core with the mesh peer-admission gate (so both are fair by identical rules); reciprocity weights from the contribution ledger rank a contributor's turns up. Secure by default: binds `127.0.0.1:8080`, and a non-loopback bind with `[auth]` disabled is refused at startup (`config::validate_exposure`; explicit `allow_unauthenticated_remote` opt-out) — permissive CORS is applied only when auth is on (`[server] cors = "auto"`). **Note the gap that guard does NOT close:** auth engages only when `mode == "api_key"` **and** `keys` is non-empty, so `mode = "api_key"` with an empty map serves every `/v1/*` route unauthenticated as tenant `"default"` — silently, and with a loopback bind the exposure guard never fires. **Two cargo features, both default ON, drop the surfaces whose safety rests on "one operator owns this box" (`sovereign/deploy/onprem/`).** `dev-routes` gates *privilege*: `/v1/solve` + `/v1/cycle/bdd` (client-supplied `test_command` reaches `sh -c` **inside** the authed router), `/v1/documents/upload` + `/v1/corpora/upload` (ingest an absolute server-side path), the `/mcp*` routes (registered *after* the auth layer, guarded only by `ip.is_loopback()` — which a same-host reverse proxy satisfies for every remote caller), and `ShellTool`. `net-tools` gates *egress*: the `search` tool's web fallback (DuckDuckGo → Google → DuckDuckGo Lite, fired whenever the top **local** retrieval score is thin), `web_fetch` (any URL the model emits; scheme-only validation), and `wikipedia_fetch`. Those three were registered unconditionally and fired on ordinary chat turns; `Permission::Network` does not gate them, because it is consulted at exactly one call site (the plan executor) and the chat path calls `tool.execute()` directly. Under `--no-default-features` `search` survives, built local-only via `SearchTool::new`. |
| `sovereign-desktop` | Tauri 2 + Svelte 5. The **UX-refactor (P0–P4) reshaped the app around user intent** — rail `Ask · Library · Reflect · Workshop · ⚙`. **Ask** (the branded chat w/ streaming + provenance) is the landing. **Library** (`library/{LibraryView,AddSheet,NotebookDetail}` off the `notebook_list` command) is the knowledge home — a notebook shelf with per-notebook Ask + Explore, plus a "Libraries on the mesh" section (the `mesh_media_offers` command → `GET /v1/mesh/media` and its `?peer=` reach, polled every 10 s; a pick probes the loopback `player_url` with `mesh_media_probe` first and opens it only once the library answers — a silent far end shows a named "did not answer" line instead of an empty browser tab, `player_url` itself never shown; a library its holder is watching shows "in use right now" and carries no `player_url` at all, so the card cannot start a stream); the catalog `KnowledgeStatus` + folder/vault/import ingest fold into Library→Add; the Atlas rail is gone (the atlas surface lives inside a notebook's Explore via `AtlasSurface startingCorpusId` + as a reading deep-link target). **Workshop** (`workshop/WorkshopView`) holds the maker facets Build · Run · Test · Connect tools (MCP) · Open to apps (OpenAI endpoint), with a notebook→Workshop "use→make" bridge. **Settings** shrank to General + Operator (Mesh · Sharing · Mobile) clusters. A follow-on **elegance pass** layered craft on top: a plain-language scope bar in Ask (`AskScopeBar` — "Asking ‹notebook›", gating `CorpusFilterStrip`), per-notebook **conversation memory** (the `notebook_conversations` command → `SqliteStateStore::list_conversations_for_corpus`, a `json_each` filter on `enabled_corpora`; a notebook's Ask resumes its last thread, switched via a **Conversations ▾** dropdown), a card→detail **shared-element morph** (`lib/motion.ts` `crossfade`), and an **Ask↔Explore** Map→Ask bridge ("Ask about this" on an atom → the notebook's Ask, seeded). The per-notebook detail consolidates its chrome into **one header bar** — segmented `Ask | Explore` + a `⋯` overflow for Sources/Settings — with the scope stated by the header (the in-notebook scope bar suppressed via `ChatView hideScope`); the **Home hub was dropped** so the branded Ask flow is the first-run landing. **Layout is token-driven, not per-component.** `app.css` owns a layout scale (`--gutter` / `--gutter-top` / `--gutter-bottom` / `--measure` / `--measure-prose`) plus three global primitives — **`.page-body`** (the scroll container + gutter every surface body needs), **`.page-measure`** (the centred content column), **`.page-header`** (a header band on the same gutter). These are global rather than Svelte-scoped on purpose: the app's surface hosts (`.library-surface`, `.settings-surface`, `.nb-body`, `.app-chrome-content`) are all `height:100%; overflow:hidden` clipping boxes, so **a body that fails to establish its own scroller is clipped with no way to reach the content past the fold**. A July 2026 audit found exactly that — `ConflictsPanel` hid 2,442px of governance decisions behind an `overflow-y:auto` that could never fire (it sat on an auto-height box), and `AddSheet`'s body rendered flush to both window edges because a `padding:0` "embedded" opt-out outlived the host that used to compensate for it. `tests/e2e/specs/library-layout-audit.spec.ts` is the regression gate: it drives every Library route, measures composited geometry, and fails on unreachable content or a body inside the gutter. Do **not** re-declare padding/overflow on an element carrying `.page-body` — Svelte scoping gives the local rule higher specificity and it wins silently. Plus skill manager, `sovereign://` deep-link handler, system tray; reuses the shared `@sovereign/chat-ui` package (`packages/chat-ui`). |
| `sovereign-mobile` (`/sovereign-mobile`) | Thin Tauri 2 client (iOS + Android) — **no local inference/Runtime/corpus**. Reaches a host's `sovereign-server` over the tailnet, authenticates as a tenant (token in keychain), renders streamed chat. Rust core owns transport (HTTP + WS), SQLite cache of the spec's cached projections, and a fail-closed connectivity monitor; re-emits the SAME `message-chunk`/`message-complete` events the shared chat FSM consumes. Conversations are cached for display and referenced as a conversation `CORPUS_REF` once host-indexed (`indexed_in_corpus`); long-context is host-side (phone sends only the new turn + conversation id, never re-uploads history or embeds); local-only sources are privacy-badged (`scope`/`mesh_shared`). **A Cargo workspace member since 2026-09-09** (`4e1f99f55`; the "written but never compiled" note was stale — the crate compiled before that change) — a census nobody can run is inventory, and `--package sovereign-mobile` resolves now. **It consumes `sovereign-turn-client` and nothing else on the wire** (sv-surface R6): the hand-copied `ServerEvent` mirror in `remote/dto.rs`, its `ProvenanceDto` / `SourceDto` / `CitationDto`, and `remote/client.rs`'s inline `Deserialize` envelopes are deleted, and every frame, prompt, notice, answer and request comes from `sovereign-contracts` through the client crate's re-exports (the direct contract dep is gone and `layer-gate --tighten` banked the fan-in cut 27 → 26 at `4a83373f4`). The `TurnFrame` match is exhaustive with no catch-all, so `Prompt`, `Notice` (`ResolveAck`, `TurnSettled`) and `QueuePosition` are handled — three capabilities the mirror could not represent — and two Tauri commands (`answer_prompt`, `cancel_turn`) plus a `SenderRegistry` give the phone a real answer path. Evidence: `src-tauri/tests/turn_wire.rs` drives a real turn over a real socket against a fixture host serializing contract frames, with the citation and provenance persisted to the cache and the post-`Complete` `ResolveAck` + `TurnSettled` bookend; `src-tauri/tests/census.rs` is the sv-one-client twin census and keeps two permanent plants so its detector stays proven. Named ceiling: the client family has no auth seam (bare `reqwest`, bare `connect_async`), so the phone reaches a DAEMON, not an api-key `sovereign-server` — daemon-first wire compatibility, recorded in `ApiClient::turn`. See `docs/specs/MOBILE.md` and `/sovereign-mobile/HANDOFF.md`. |
>>>>>>> origin

Verbs by sibling: `sovereign-cli` holds the light delegators (`notes`,
`status`, `drift`, `session`, `design`, `plan`, `init`, `reflect`, `memory`,
`serve`) plus `code index` and `refresh` behind the `code-intel` feature;
`sovereign-cli-daemon` holds `daemon`, `setup`, `install-service`, `doctor`;
`sovereign-cli-dev` holds `atos`, `tools`, the `code` analysis subcommands and
the `project` lifecycle subcommands; `sovereign-cli-llm` holds everything that
talks to a model or does heavy retrieval. `code converge` is the one verb
LINKED rather than exec'd, from `sovereign-cli-dev`'s `[lib]` target.

### Subsystems with their own docs

| Subsystem | Doc |
|---|---|
| Slots, OICP, harness, cutoffs | [`docs/inference.md`](./docs/inference.md) |
| Inline completion (FIM) / next-edit | [`docs/INLINE_COMPLETION.md`](./docs/INLINE_COMPLETION.md) |
| Glassbox reading surface + Atlas Inspector | [`docs/knowledge-view.md`](./docs/knowledge-view.md) |
| Knowledge bases + tiered retrieval | [`docs/KNOWLEDGE_BASES.md`](./docs/KNOWLEDGE_BASES.md), [`docs/TIERED_RETRIEVAL.md`](./docs/TIERED_RETRIEVAL.md) |
| Retrieval redesign | [`docs/RETRIEVAL_REDESIGN.md`](./docs/RETRIEVAL_REDESIGN.md) |
| Epistemic state / the epistemic index | [`docs/EPISTEMIC_STATE.md`](./docs/EPISTEMIC_STATE.md), [`docs/specs/EPISTEMIC_INDEX.md`](./docs/specs/EPISTEMIC_INDEX.md) |
| Ontology primitives + migration | [`docs/specs/ONTOLOGY_PRIMITIVES.md`](./docs/specs/ONTOLOGY_PRIMITIVES.md), [`docs/specs/ONTOLOGY_MIGRATION.md`](./docs/specs/ONTOLOGY_MIGRATION.md) |
| ATOS | [`docs/ATOS.md`](./docs/ATOS.md), [`docs/ATOS_RUNNER.md`](./docs/ATOS_RUNNER.md) |
| Drift / correctness tooling | [`docs/DRIFT_DETECTION.md`](./docs/DRIFT_DETECTION.md), [`docs/CORRECTNESS_TOOLING.md`](./docs/CORRECTNESS_TOOLING.md) |
| Work-atlas peer coordination | [`docs/WORK_ATLAS.md`](./docs/WORK_ATLAS.md) |
| Desktop quality surface — START HERE to verify the desktop | [`crates/sovereign-desktop/QUALITY_SURFACE.md`](./crates/sovereign-desktop/QUALITY_SURFACE.md) |
| Browser actuation / TDD machine / Solver | [`docs/BROWSER_ACTUATOR.md`](./docs/BROWSER_ACTUATOR.md), [`docs/TDD_MACHINE.md`](./docs/TDD_MACHINE.md), [`docs/SOLVER_DESIGN.md`](./docs/SOLVER_DESIGN.md) |
| Mobile / session continuity / memory model | [`docs/specs/MOBILE.md`](./docs/specs/MOBILE.md), [`docs/specs/SESSION_CONTINUITY.md`](./docs/specs/SESSION_CONTINUITY.md), [`../docs/specs/MEMORY_MODEL.md`](../docs/specs/MEMORY_MODEL.md) |
| Worker pods / cloud peers | [`docs/PINNED_WORKER_AS_INFERENCE_PEER.md`](./docs/PINNED_WORKER_AS_INFERENCE_PEER.md), [`../docs/CLOUD_PEER.md`](../docs/CLOUD_PEER.md) |
| On-call runbook / threat model | [`docs/RUNBOOK.md`](./docs/RUNBOOK.md), [`../docs/THREAT_MODEL.md`](../docs/THREAT_MODEL.md) |
| FAQ / troubleshooting | [`docs/FAQ.md`](./docs/FAQ.md), [`docs/HAVING_TROUBLE.md`](./docs/HAVING_TROUBLE.md), [`docs/TROUBLESHOOTING.md`](./docs/TROUBLESHOOTING.md) |

Notable in-tree invariants: watched folders are read-only on source, and
sensitive folders never leave the machine.

---

## 5. cmnwlth — the coordination daemon

A symmetric daemon: every node runs the same binary, no master. It translates
"complete this chat with model X" into a plan that spawns `llama-server` on
one node and `rpc-server` on others, holds the OpenAI-compatible endpoint
open, and keeps the plan healthy as nodes come and go.

### Discovery and membership

<<<<<<< HEAD
Every node persists an Ed25519 keypair; identity is mesh-independent and
survives `leave` and every switch. mDNS advertises `_commonwealth._tcp.local`;
gossip is a 10s epidemic loop over 2–3 random peers with timestamp-LWW
conflict resolution; latency probing is UDP RTT every 30s.

`Mesh` carries **two** credentials and the split is load-bearing:
`mesh_secret` authorizes gossip and never rotates; `invite_key_hash` admits
joiners and rotates freely. A gossip round carries a keyed-BLAKE3 `mesh_proof`
bound to the sender and a 30s window rather than the raw secret; an OFFERED
proof that fails is a hard refusal, never a fall-through. Rotation is refused
while the fleet is mixed, and the confirmation is local observation (the
`GossipAuthArm` that won), never a peer's claim.

**One endpoint key, one member row.** `aliased_endpoint_keys` is the one
implementation; `merge_from_authenticated` refuses to ADMIT a collision while
`gossip::one_row_per_endpoint_key` RESOLVES one at the dial site, because
refusing there strands the machine. Selection fairness is a separate clock
from liveness: offline-decay reads contact, `select_round_peers` reads
`peer_last_attempt`, stamped before the dial so refusals and timeouts advance
it too.

**Encryption, and the honest gap.** A plaintext mesh is the default. A mesh
created with `require_encryption` flips every node to the iroh dial-by-key
transport in REQUIRE mode with no plaintext fallback, binds its listeners
loopback-only, and admits joiners only over an encrypted founder-key-dialed
channel. **NOT covered: the multi-host tensor-split RPC between
`llama-server` and `rpc-server` is raw TCP, outside the transport seam, and is
the sole residual plaintext on an encrypted mesh. Never claim blanket
end-to-end encryption.** Surface-by-surface posture
[`../docs/THREAT_MODEL.md`](../docs/THREAT_MODEL.md).

### The PeerTransport seam

`commonwealth-transport` resolves (peer, traffic class) → ordered base URLs in
exactly one place. `IpTransport` is today's tailnet/LAN overlay;
`IrohTransport` is dial-by-Ed25519-pubkey QUIC bridged to HTTP through
localhost byte-tunnels; `RoutedTransport` composes them, concatenating
candidates ahead of a default so a failed iroh dial degrades to the tailnet
path on the same request. With iroh enabled every class is iroh-first with
per-dial IP fallback, and `[iroh] enabled` absent means AUTO (on iff this node
is in a mesh). Out of seam by design: the join handshake, worker-pod
transport, loopback self-probes, and the raw-TCP tensor traffic above.

Six ALPNs carry the encrypted mesh, and what a STRANGER gets differs per
ALPN — **holding the dial string is not a credential**, so the acceptor routes
on `(ALPN, dialer)` and the key the QUIC handshake proved is the discriminator:

| ALPN | member | stranger |
|---|---|---|
| `cwth/client/0` | the PEER listener (no bearer — federated inference carries none, its key is the credential), serving the client router minus `/internal/*` | the bearer-checking listener |
| `cwth/rpc/0` | the local ggml rpc-server | REFUSED — it authenticates nothing |
| `cwth/media/0` | the declared `[iroh] media_origin` | REFUSED |
| `cwth/app/0` | one of several named HTTP apps, chosen by first path segment | REFUSED |
| `cwth/offer/0` | the declared `[iroh] offer_origin` | REFUSED — the dial string is gossiped, so a downgrade would publish a household's inventory |
| `cwth/guest/0` | — | admitted; the listener reads the bearer |
| `cwth/http/0` | internal router | internal router, DELIBERATELY — a joiner is not a member and `/internal/join` is how it becomes one |
=======
- **Join keys** — `cwth-XXXX-XXXX-XXXX`.
  `membership::generate_join_key` stores BLAKE3 hash, discards
  plaintext. `verify_join_key` compares BLAKE3 hashes. First node calls
  `init_mesh`; subsequent nodes call `accept_join`.
- **Node identity keys** — every node persists an Ed25519 keypair at
  `<data_dir>/node_key` (sibling of `node_id`;
  `commonwealth-transport/src/identity.rs`). The pubkey travels as
  `MemberRecord.node_pubkey` (serde-defaulted — pre-identity builds
  interop), is proven at join time (proof-of-possession signature in
  `JoinRequest`, bad proof → 401), self-stamped by gossip every round
  (the in-place upgrade path), and protected by an anti-downgrade
  rule in `Mesh::merge_from` (a relayed record without the key never
  strips a known one). The seed is byte-compatible with an iroh
  `SecretKey` — it IS the future dial-by-key transport identity.
- **mDNS** — `_commonwealth._tcp.local` advertising `node_id`,
  `mesh_id`, `name`.
- **Gossip** — 10s epidemic loop, 2–3 random peers per round.
  Three-phase digest/delta/response. Conflicts: timestamp LWW.
  Payloads: `MemberState`, `InferencePlan`, `KnowledgePlan`,
  `LedgerEntry`, `MeshConfig`.
- **Latency probing** — UDP RTT every 30s, magic bytes `CWLP`,
  EWMA α=0.3. `LatencyMatrix` shared via gossip.
- **Hardware detection** — `commonwealth-discovery/src/hardware.rs` tries
  `nvidia-smi`, then `rocm-smi`, then Metal.
- **TLS / mesh encryption** — A plaintext mesh (the default) serves
  the internal API (:9742) in the clear; the unused per-session-cert /
  `TrustStore` scaffolding (`discovery/tls.rs`) was removed (2026-06-15)
  rather than left as a security façade. TLS *is* used on the separate
  worker-pod path (`sovereign-pods/worker_daemon.rs`,
  `axum_server::bind_rustls`).
  - **Encrypted mesh (opt-in, founder-set at creation).** A mesh created
    with `require_encryption` flips every node to the iroh dial-by-key
    transport (QUIC/TLS) in REQUIRE mode — no plaintext fallback
    (`RoutedTransport::with_required`, fail-closed) — binds its internal +
    client listeners loopback-only (the iroh acceptor is the sole network
    ingress), and admits joiners only over an encrypted, founder-key-dialed
    channel with a short-lived (24h) TTL invite. The policy lives on the
    gossiped `Mesh` struct (`require_encryption`, monotonic stricter-wins
    in `merge_from`), is inherited at join, and persists. Dial info
    (`relay_url` + `iroh_direct_addrs`) is signed per-node
    (`commonwealth-core::dial_sig`, monotonic `dial_info_version`) so a
    gossip-strip attacker past the join-key gate cannot force a peer
    unreachable or downgrade it. **NOT covered:** the multi-host
    tensor-split RPC between `llama-server`/`rpc-server` (raw TCP, out of
    the transport seam) — the sole residual plaintext on an encrypted
    mesh. Never claim blanket end-to-end encryption. The consolidated
    surface-by-surface posture (every listener, its default bind, auth,
    and the honest gap ledger) lives in `../docs/THREAT_MODEL.md`.
- **Mesh peering** — `peering.rs`; two `PeerTrustLevel`s:
  `ModelAndKnowledgeSharing`, `Full`.

### The PeerTransport seam (commonwealth-transport)

How this node reaches a peer is decided in exactly one place:
`commonwealth-transport`'s `PeerTransport` trait resolves
*(PeerContact, TrafficClass) → ordered base URLs*; call sites keep
their own reqwest clients/timeouts and append route paths. The live
instance hangs off commonwealth-api's `AppState`
(`peer_transport()`; the bootstrap and the iroh watchdog publish
through `peer_transport_reader()`).

- **`IpTransport`** (production): today's tailnet/LAN overlay. Owns
  the Tailscale CGNAT/ULA address ranking
  (`commonwealth_core::peer_addr::rank` — no other production caller)
  and the last-working-address promotion
  that used to live as duplicate caches in gossip and knowledge
  fan-out. Port policy per class: Gossip/ControlPlane/
  KnowledgeSearch/ModelTransfer use the gossiped address verbatim;
  Inference/StatusProbe rewrite to the (assumed-uniform) client
  port. Golden URL-vector tests pin byte-identical output vs the
  pre-seam inline `format!` strings.
- **`IrohTransport`** (cargo feature `commonwealth-transport/iroh`,
  pinned `iroh 1.0` stable since 2026-06-18): dial-by-Ed25519-pubkey
  QUIC, bridged to HTTP via localhost byte-tunnels (client TCP bridge
  + `IrohAcceptor` → existing axum listener). `IrohAcceptor` has two
  forms: `spawn` (all streams → one local listener, Track M) and
  `spawn_routed` (W1 — dispatch by negotiated ALPN to per-class local
  listeners). Spike proof:
  `sovereign-mesh/tests/iroh_transport_e2e.rs` (run with
  `--features iroh-experimental`) drives a real gossip round dialed
  by pubkey. `IrohTransport` resolves its dial target from the
  gossiped `PeerContact` (relay + direct addrs, W2) and picks the ALPN
  by `TrafficClass`; whether it carries a class is the W3 config flip.
- **`RoutedTransport`** (`commonwealth-transport/src/routed.rs`, W3):
  routes each `TrafficClass` to a chosen transport, concatenating its
  candidates ahead of a default (`IpTransport`) — callers try in order,
  so a failed/absent iroh dial degrades to the tailnet path on the same
  request, automatically. `note_success` routes feedback to the
  producing transport by label prefix. Empty `per_class` == its default.
- **Track W1 (server half) + W2 (dial info in trust ring) + W3
  mechanism are implemented** (2026-06-18): when `[iroh] enabled`,
  `EmbeddedDaemon::start_daemon` binds one iroh endpoint from the
  daemon's gossiped `node_key` and accepts across the ALPNs —
  `cwth/http/0` → internal router, `cwth/client/0` → client router,
  and (2026-08-27) `cwth/guest/0` → a SECOND bind of the client router
  whose auth layer does not trust loopback — so a peer/phone
  reaches this daemon by key with no VPN
  (`sovereign-mesh/src/iroh_access.rs`, `MeshIrohAccess`; additive,
  fail-soft, held in `DaemonState::Running`). W2: `MemberRecord` carries
  `relay_url` + `iroh_direct_addrs` (serde-defaulted, MUTABLE
  reachability — normal LWW, unlike `node_pubkey`'s anti-downgrade);
  the daemon self-stamps its live dial info each gossip round via a
  pull-provider on `AppState`; `IrohTransport` dials peers purely from
  the gossiped contact (**membership = dialability**).
- **The no-VPN mesh (2026-07, merged via the `Saas` PR #13; unit +
  e2e + soak-axis verified).** When iroh is enabled, `RoutedTransport`
  routes **every** `TrafficClass` iroh-first with automatic per-dial
  IP fallback — `[iroh.transport] <class> = "ip"` is now an opt-OUT,
  not an opt-in flip. Enablement is `[iroh] enabled: Option<bool>`:
  absent = AUTO (on iff this node is in a mesh, keyed off the
  `client-exposed` marker — a meshless daemon never contacts relays),
  `Some(false)` = kill-switch (also `SOVEREIGN_IROH=off`). Plaintext
  invites carry a `dial=` connect code (distinct from the encrypted
  `iroh=`); `join::perform_join` dials the founder by key first and
  falls back to `?relay=`/mDNS (W2c). A `RelayConfig` (`[iroh]
  relay_urls` + `discovery`) drives `build_relayed_endpoint`:
  self-hosted relays (W4), and `discovery = "none"` builds from
  `presets::Minimal` to sever ALL n0 services (H1 — `relay_urls`
  alone keeps n0's DNS lookup, so it is not a no-third-party
  posture). `proxy_from_env` is always on, so the mesh survives
  UDP-blocked corporate networks over relay-TCP:443 through a
  (Basic-auth) HTTP proxy. Encrypted meshes
  (`require_encryption`) stay the fail-closed variant (all classes
  REQUIRE iroh, loopback-only listeners). `IpTransport` remains the
  permanent fallback; every piece is config-reversible.
- **Track M (mobile) is implemented**: `sovereign-server`'s
  `[iroh] enabled` block accepts dial-by-key clients on ALPN
  `cwth/client/0` (`src/iroh_access.rs`; pairing string at
  `GET /status` → `iroh.dial`), and `sovereign-mobile`'s
  `endpoint_kind='iroh'` host rows tunnel HTTP+WS through a
  localhost bridge (`src-tauri/src/iroh_bridge.rs`) — no VPN on the
  phone. This pulls the iroh feature into sovereign-server's default
  build (runtime-gated off); see
  [`docs/specs/TRANSPORT_MIGRATION.md`](./docs/specs/TRANSPORT_MIGRATION.md)
  for phase status and device-side exit criteria.
- **Out of seam, by design**: the join handshake (pre-membership
  bootstrap), worker-pod `PinnedTransport` (separate trust model),
  loopback self-probes, and the raw-TCP `llama-server`/`rpc-server`
  tensor traffic. The last is the decided W6 posture: multi-host
  inference needs its GPU anchors on a shared IP network (LAN/VPC) —
  which every supported topology already has — rather than a VPN or a
  per-worker iroh sidecar (specced as Option B, gated on a tok/s A/B).
- **Migration order** (when a second transport goes live) is encoded
  by `TrafficClass`, not config: a small `RoutedTransport` mapping
  classes → transports slots into the same `Arc<dyn PeerTransport>`
  — gossip/membership first, blob/model transfer next, inference
  streaming last, raw RPC tensor traffic remaining on IP. The full
  phased plan (mobile first, then per-class mesh flips, relay
  self-hosting, Tailscale-optional end state) is
  [`docs/specs/TRANSPORT_MIGRATION.md`](./docs/specs/TRANSPORT_MIGRATION.md).

### Scheduling + orchestration

**The live decision topology** (rationalized 2026-06-10 — a dead
second-generation scheduler that previously filled this section was
deleted; see `docs/specs/OICP_RATIONALIZATION.md` for the audit):

| Decision | Where | Mechanism |
|---|---|---|
| Joiner decides a turn is offload-*eligible* | `sovereign-mesh/oicp_select.rs::offload_eligible` | SLOT_POLICY §5: `privacy == MeshAllowed && latency_class != Fast`. One predicate, shared by `select_peers_ranked` and `shared_primary_id`; replaced the old privacy-gate + `preferred_speed != Slow` pair (the Speed shadow no longer gates routing) |
| Joiner picks peer-vs-local for an eligible turn | `sovereign-serving-host/src/peer_inference.rs::select_peers_ranked` | OICP claim score × operational adjustments (observations, load, locality, cold-start, throughput, availability); forced-choice sentinels exclude peers not advertising `x:forced_choice` |
| Joiner resolves a *named* target | `sovereign-serving-host/src/peer_inference.rs::locate_named_model` | Name resolution + min-in-flight tiebreak, **not** the scorer. **Hard** (caller-supplied `model_id`) is a constraint: unknown ⇒ error, never substitution. But a peer route that FAILS is not the same as unknown: `LocalAlternative` records whether the peer was the sole holder or merely won the min-in-flight tiebreak over us, and in the latter case a peer failure is served from our own copy of the same id (2026-08-06 — before this, a shed peer 503'd a caller for a model that was loaded locally). Serving the named id here is honouring the name, not substituting for it; sole-holder routes still fail loud. **All three entry points (`complete`, `complete_stream_with_id`, `complete_stream_with_id_and_finish`) resolve through one `select_route` → `RoutePlan` cascade as of 2026-08-07**; per-method code builds only the step's terminus. Before that, `complete()` routed inline via a `select_peer` that took a single peer, so the non-streaming path gave up after one declining peer, skipped peer in-flight booking on ranked routes, and was the reason four successive features each had to be written twice. A named step carries `pinned_model_id`, so the resolved id goes on the wire and a strictly-resolving peer cannot refuse the turn into a silent local substitution. **Soft** (configured `shared_model_id`) is a preference: unknown ⇒ falls THROUGH to `select_peers_ranked` with local as the last rung, recorded on `DecisionPath::NamedFallthrough` (SCHEDULER_QUALITY F8 / §4.3, 2026-07-27) |
| Hub picks a local model for a peer request | `sovereign-daemon/src/routes_inference.rs::route_with_oicp` | OICP claim score over synthesized claims |
| Serving peer picks Fast-vs-Slow slot | `sovereign-mesh/oicp_select.rs::pick_slot_for_oicp` | canonical `slot_policy::latency_to_speed` + hint veto; `pick_slot` backstops `x:forced_choice` sentinels onto Primary |
| Synthesis tier (Fast vs Primary) | `sovereign-core/runtime/evidence.rs::resolve_synthesis_route` | intent + atom-enum + evidence-shape heuristic |
| Distributed placement (model > one node) | `sovereign-inference/embedded/rpc_distribution.rs` | LocalOnly default; StreamSplit ≤500MB; warmed owned-overrides as last resort |
| Collaborative ingest partitioning | `sovereign-grants/knowledge_assignment.rs` | `plan_collaborative_ingestion*`: embed-model-compatible peers, storage-proportional contiguous blocks, zero-storage peers skipped |

**Slot policy is normative** in [`docs/SLOT_POLICY.md`](./docs/SLOT_POLICY.md)
(OICP-first rationalization, 2026-07-08): call sites declare a
`slot_policy::Workload` requirement bundle rather than free-handing
`Speed::` literals; the scheduler resolves those against every slot's
advertised claims cluster-wide, and fast-vs-primary is an emergent
scoring outcome (the local node is the degenerate one-node mesh).
`Speed::Medium` is retired as a construction target (kept only for
serde/metadata); the one canonical `latency↔Speed` map lives in
`sovereign-contracts/slot_policy.rs`.

The composed OICP scoring product lives ONCE in `oicp-types`
(`score_with_adjustments` + `ScoreBreakdown`, Phase B of the
rationalization) and is consumed by sovereign-mesh and
sovereign-inference; leader election lives in
`commonwealth_core::partition::elect_leader`.

**Scheduler quality — measurement, not just plumbing.** Retrieval,
grounding and synthesis each have a bench and a tight iteration loop;
this layer has unit tests on individual factors and e2e suites that
verify *plumbing*, and until 2026-07 nothing measured whether a
routing decision was **good**. The diagnosis, the six findings behind
it and the build order are in
[`docs/specs/SCHEDULER_QUALITY.md`](./docs/specs/SCHEDULER_QUALITY.md);
the root cause is that `score_with_adjustments` returns a product of
six dimensionless multipliers — it *ranks*, it does not *predict* — so
no scoreboard was definable.

**Phase 0 (instrumentation) is landed**, changing no routing decision:

| module | role |
|---|---|
| `sovereign-scheduler/decision_log.rs` | One `RoutingDecision` per decision point (whole candidate set, each `ScoreBreakdown`, each input stamped with its **provenance and age**, peers excluded before scoring and why, the verdict) joined by `decision_id` to one `RoutingOutcome` per completion (served-by / TTFT / total / tokens / shed / failovers). `DecisionSink` is the seam — production, capture-for-tests, null. |
| `sovereign-scheduler/decision_trace.rs` | Replay: `SchedulerTrace::from_jsonl` groups records into `Episode`s by `decision_id` (never adjacency — a live log interleaves requests) and reports a `join_rate` to gate on. |
| `sovereign-serving-host/src/peer_inference.rs` | Emission at `select_peers_ranked` (including gated exits) and join-closing in both stream cascades and `complete`. `observation_snapshot()` exports per-peer observations + gossiped benchmarks + `PeerHealth` — folded into the record stream every 60s so a capture is self-contained. |
| `sovereign-scheduler/yield_backoff.rs` | The one exception to "Phase 0 changes no routing decision" (2026-08-14, order `serve50-availability`). A peer that refuses a hop with `yielded_to_local` is excluded from candidacy — before the manifest fetch — for the `retry_after_secs` it named, capped at 60 s and cleared by any successful turn. Recorded as `ExclusionReason::YieldedToLocal` and `FailoverAttempt.yield_retry_after_secs`. Distinct from `PeerHealthTracker`: a refusal books nothing toward quarantine. |

Capture with `SOVEREIGN_DECISION_LOG=<path>` on the daemon; records
also reach `tracing` under the **`mesh.decision`** target (listed in
`DAEMON_TRACING_FILTER`, without which a custom target is dark).

**Why an exclusion and not a score discount** (the question this design
invites): the SSOT scorer clamps gossiped availability to `[0.2, 1.0]`
(`oicp-types/src/scoring.rs`), so the score path's strongest possible
"no" is a 5× multiplier — a peer better than that on the other terms
still wins and still gets refused. Skipping the manifest fetch is also
what makes a yielding peer cost *nothing* rather than a cheaper
something. See `research/scale-analysis/MESH_SCALE_100_USERS_1000_CORPORA.md`
§9.1.3.

**A node's gossiped `inference_availability` is a composite with one
writer and two inputs** (`sovereign-daemon/src/state.rs`):
`AppState::recompute_local_availability` publishes
`min(activity_level, yield_floor)` — the activity level reported by
sovereign-server's `ActivityReporter` via
`update_local_availability`, and a yield floor derived from the same
two predicates `admit_peer_request` consults, so what a node advertises
and what it enforces cannot drift. The daemon is the second CALLER of
that one writer, recomputing inside the gossip round immediately before
publication (`gossip.rs`) — the yield state is time-derived and has no
transition event of its own to hook. Before 2026-08-14 nothing in the
daemon wrote the field at all, so a node refusing every peer request
gossiped `1.0` for as long as it kept refusing.

**Phase 1 S0 (the Tier-1 simulator) is landed** (2026-07-26), and it
changed the diagnosis:

| module | role |
|---|---|
| `sovereign-scheduler/scheduler_core.rs` | The routing decision as a **pure total function** — `rank(DecisionBuilder, RankInputs) -> RankResult` over a snapshot of what a decider believes, with `now_unix` passed rather than read. `select_peers_ranked` is now gather-then-decide: async I/O above the line, this below it. Also holds the observation feedback (`observe_dispatch` / `observe_success` / `observe_failure`) the provider's `record_*` methods delegate to, so sim and production age their beliefs by one implementation. |
| `sovereign-mesh-test-harness/mesh_sim/` (feature `dst`) | Seeded discrete-event mesh: virtual clock, gossip propagation, manifest-cache ageing, queueing, **model-load time** (`model_load_sec_per_gb`: a cold node advertises `loaded: false` + an estimate and pays it once, attributed to TTFT so the throughput EWMA is not poisoned), nineteen arms (as-implemented / **blind-local-load** / **blind-peer-ramp** / **blind-observations (§4.4)** / fresh-signals / two-choices / both / warm-start / fresh+warm-start / outbound-only-load / **predicted-time (§4.1)** / predicted-time+outbound-only / **tier-floor** / **predicted-time+tier-floor (§4.1.1)** / **predicted-time+tier-floor+two-choices (§4.1.2)** / **predicted-time+tier-floor+within-noise (§4.1.3)** / **response-backpressure (§4.2.1)** / predicted-time+tier-floor+backpressure / a perfect-information oracle). Arm 0 *is* `rank` — not a transcription of it, but note it models the beliefs the dispatch path was *designed* to produce; the three `blind-*` arms model the ones it actually produced before F9's fix, and `blind-observations` is the as-shipped baseline (§4.4). No extra dependencies; **four** separate RNG streams — world, policy, advertised-rate error, and advertised-size error — so switching arms cannot perturb the world the arms are compared in, and both fidelity knobs default to inert so every number recorded before they existed still reproduces. |
| `sovereign-scheduler/predicted_time.rs` | The §4.1 candidate objective, and the only ranking in the tree with **no tunable constant**: `predict()` returns `queue + prefill + decode + rtt` as named addends or an `Unpredictable` reason (never a defaulted rate — a guessed rate is a fabricated fact with a unit attached), and `faster_than_local` filters on it. `LocalOption` keeps *unpredictable* local (⇒ no hop) distinct from *infeasible* local (⇒ any feasible peer wins); collapsing those points them in opposite directions. Reads only what a decider can see, so `PredictInputs::from_candidate` scores it against a production capture. |
| `sovereign-mesh-test-harness/mesh_sim/scoreboard.rs` | `RecordMetrics` and `TierMetrics` are computable from a **production capture** too (the S1 precondition) — `TierMetrics` is §5's capability column, counting downgrades and declined upgrades from decision records alone; `TruthMetrics` needs simulator ground truth and so may never define a calibration gate. |

Run it: `cargo test -p sovereign-mesh --features dst,treesitter
--test main mesh_sim_scoreboard -- --nocapture` (~0.3s; `sovereign-lint.sh`
keeps it compiling).

**Phase 1 S1's instrument is landed** (2026-07-26) — the hardware
capture it points at is not taken:

| module | role |
|---|---|
| `sovereign-scheduler/decision_replay.rs` | Re-runs the **live** scorer and the **live** ranking policy over a captured record and reports whether the record reproduces its own scores and verdict. Split in two on purpose: *scorer agreement* (recorded `CandidateInputs` + `claim_score` + locality → `score_with_adjustments` → does `final_score` come back?) and *policy agreement* (recorded scores → `winners_over_local` → does the `Verdict` come back?). The two run off independent inputs so one bug cannot cascade into the other. Both ratios return `0.0` on an empty denominator, never a vacuous `1.0`. |
| `sovereign-scheduler/scheduler_core.rs` | Gained `winners_over_local` / `beats_local` / `local_sentinel` — the ranking half extracted so replay re-runs the policy rather than a copy of it. Also `RankObjective` on `RankInputs` (`Product` \| `PredictedTime`): the objective is a *parameter* rather than a branch at the call site, so both objectives share one scoring body, one record shape and one `finish_at` — which is what keeps a decision record describing what the decider actually did instead of the product's opinion of a choice it did not make. Production passes `Product`. |
| `tests/scheduler_replay_agreement.rs` | The fixture with a known answer: sim → `TracingDecisionSink::to_path` → JSONL → `SchedulerTrace::from_jsonl_path` → replay. **1.000 / 1.000, bit-exact**, five scenarios × six arms. |

The gap S1 was expected to surface — `claim_affinity` is an argument
the scorer takes and the record does not carry — turned out not to
need a schema field: `observation_mult = effective_affinity(a,obs)/a`
is independent of `a` over `(0, 1]`, and `a` is clamped to `[0, 1]` at
the type level by `CapabilityClaim::effective_affinity`. Settling that
in the simulator is the reason the replay was built before the
capture.

Three **diagnostic** arms landed with it, each pricing a question
before it costs hardware time, and each asserting its own wiring first
(a null result is only informative if the knob is proven connected).
`WarmStart` prices F7: removing the cold-start floor is **+235% mean
latency**, so the floor is the mesh's only brake on offloading.
`FreshWarmStart` then asks whether that damage is F1's — and says
**no**, the penalty is *larger* (+264%) with a perfect load signal, so
the extra offloads lose on their own merits and the floor is
compensating for an **over-eager objective**. That is a direct
argument for §4.1's structural change: a product of dimensionless
multipliers cannot decline a hop that costs more than it buys, and
ranking on predicted time-to-answer can. `OutboundOnlyLoad` says that
if the gossiped in-flight counter misses inbound peer work it costs
+126% to +584%, which earns the two-daemon audit F2's caveat now calls
for.

Findings, in `SCHEDULER_QUALITY.md` §3.1: **F3 reproduced exactly**;
**F1 reproduced but costs the tail, not the median** (the reverse of
the hand-model's reading); **F5's mechanism reproduced but its
two-choices remedy is inert** wherever the fleet has a unique
capability winner, because the eligible set is then a singleton; and a
new **F7 — the cold-start ramp is self-locking**, contradicting
`cold_start_weight`'s own doc comment. **No Phase-2 behavioural change
has landed in production** — the ordering is deliberate: the sim is the
baseline machine, so fixes land as sim arms first.

**§4.1 measured (`Arm::PredictedTime` + `sovereign-scheduler/predicted_time.rs`,
2026-07-26).** The candidate objective — rank on
`queue + prefill + decode + rtt` instead of on a product of
dimensionless multipliers — now exists as an arm, introduces **no
tunable constant**, and is computable from a decision record, so it can
be scored against a production capture with no new instrumentation.
It decomposes the oracle gap that arm 0 and `Oracle` only bracketed:
**the wrong objective costs +126%/+200%/+250%, imperfect information
costs +4.7%/+1.8%/−0.0%** (household / twin-hubs / heterogeneous), which
demotes F1 to the sustained-contention case (`isolation`, +43.8%). The
win survives a ±2× mis-rated fleet (`SimConfig::advertised_rate_error`,
which exists because the sim otherwise grades the predictor against its
own rate card). **It cannot land yet, and not for a latency reason:**
ranking on time alone routes knowledge turns to 4B laptops — 37 of 38
household offloads, and never a hub in `twin-hubs` — so §4.1's tier
floor is a prerequisite, and no §5 metric can see what its absence
costs. Replay also surfaced a missing field: a `RoutingDecision` does
not record *which objective* produced its verdict, so a predicted-time
capture reports scorer agreement 1.000 and policy agreement 0.009.

**The tier floor, and what it did to that claim (`sovereign-scheduler/tier.rs`,
`Arm::TierFloor` + `Arm::PredictedTimeTierFloor`, 2026-07-26 — full
result in `SCHEDULER_QUALITY.md` §4.1.1).** Capability is now a
**filter, not a term**: candidates are partitioned into bands derived at
runtime from the sizes on the manifests a decider currently holds — a
*relative* edge (`BAND_RATIO`, measured against the band's max),
recomputed per decision, never an absolute GB threshold or a table of
model names — and a `Normal`/`Extended` request must be served from
band 0. The floor is read off `effective_latency_class()`, the same map
`latency_to_speed` already uses locally, so this is the policy the local
slot picker has always enforced, finally applied to peers. `TierMetrics`
adds §5's missing column and splits two hazards that were being counted
as one: **downgrade** (served below the origin's own local model — a
real regression, 31% under predicted-time) versus **declined upgrade** (a
stronger node was feasible, 69%). Both are computed from decision
records alone, so the identical function scores a production capture.
Three results change the plan: **(1)** §4.1's headline is not
quality-constant — on `twin-hubs`, the one fleet whose top band is not
saturated, arm0+floor is 31.0s against predicted+floor's 32.6s, so at
constant quality the objective is *~5% worse* than the product, not 200%
better; **(2)** the floor is *free* where the top band has capacity
(twin-hubs −2% versus arm 0 with every quality loss eliminated) and
catastrophic where it is not (household 25.7s → 559.5s) — but
`queue_wait_ms` by dispatch quartile shows a **flat** service time
against a queue climbing 241s → 1020s, so that is a capacity fact about
a one-hub fleet, not a scheduling result, and `heterogeneous-fleet`'s
queue is already unbounded under arm 0 with no floor at all;
**(3)** predicted-time *herds harder* than the product once the floor
makes candidates homogeneous (40/28/10 across three identical hubs
versus 31/27/18), so §4.2 step 2 is a prerequisite rather than a
follow-on. `SimConfig::advertised_size_error` prices the floor's own
self-reported input the way `advertised_rate_error` prices the rate
card.

**What the objective is actually worth, on a second unsaturated fleet
(`scenario::mixed_hubs`, `Arm::PredictedTimeTierFloorTwoChoices`,
2026-07-27 — full result in `SCHEDULER_QUALITY.md` §4.1.2).** Result (1)
above was n=1 in two ways at once: one fleet, one seed. At five seeds
across two fleets the answer is **conditional on whether the top band's
members differ in speed**. `mixed-hubs` is the second unsaturated fleet
and the deliberate *opposite bracket* to `twin-hubs` — the same 35B (so
the same band) on 34/25/11 tok/s machines, where `twin-hubs` band 0 is
three identical hubs. Predicted-time is **+3% (1/5 seeds)** on
`twin-hubs` and **−8% (5/5 seeds)** on `mixed-hubs`. The mechanism is F3
and it is not the obvious one: the product already sends **zero** turns
to the 11 tok/s hub (`throughput_factor` 0.55 is decisive), and its
whole loss is splitting ~50/50 between the 34 and 25 tok/s hubs, which
the clamp at 20 tok/s renders identically 1.0 — deleting the slow hub
leaves predicted-time ahead by 3%, so the win survives deleting the gap
the scorer *can* see. It is not the harness flattering the objective
either: under `advertised_rate_error` the win *widens* (−8/−7/−11/−13%
at ±0/25/50/100%), and the product's one error-correcting path (observed
decode EWMA past five samples) is shown to carry only ~5% of scorings.
Result (3)'s remedy is measured rather than inferred: a **blunt**
two-choices sampler takes `twin-hubs` from +3% to −4% and `mixed-hubs`
from −8% to +3%, so §4.2 step 2's *"among candidates whose predictions
are within noise"* is the load-bearing clause, not a refinement — and
what makes that clause expressible is that predicted times have
**units**, where a dimensionless product has no scale on which two
scores can be called close. Saturation is now gated on
`backlog_depth` (final-quartile queue wait over service time: household
38 turns, heterogeneous 6.6, twin/mixed both under 1.0); the earlier
Q1→Q4 3× ratio is kept only as a screen, because it fires on any fleet
loaded enough to build a queue at all.

**Fresh backpressure measured before it was built — and deferred
(`Arm::ResponseBackpressure`, 2026-07-27 — full result in
`SCHEDULER_QUALITY.md` §4.2.1).** §4.2 step 1 proposed collecting
`fresh-signals`' −9..−22% p95 by piggybacking the serving node's load
onto responses it already sends. The arm is that mechanism with its
*real* reach — fresh only for a peer this decider has actually served a
request through — and it does not pay: **+1.6/−2.6/+0.1% mean** across
household-evening-12 / twin-hubs / mixed-hubs at **4–7% dispatch
coverage**, against fresh-signals' −9 to −11%. The mechanism is not
broken, it is *unreached*: on `isolation` (a background actor
dispatching every ~8s) coverage rises to **46%** and the median true
signal age drops 15.0 → 10.4s, so coverage is a property of traffic
density, not of wiring. The two densities form a scissor — where it
fires the fleet is capacity-bound (fresh-signals itself buys −1.8%
there), and where information binds it does not fire. The structural
reason generalises: a response can only carry news about a peer you
**already chose**, and F1's cost lives in the peers you did not. §4.2's
prediction that freshness matters more to the predicted-time objective
(bounded `load_penalty` vs. a first-order queue term) is real in the
arithmetic and invisible at this coverage: −2.6% vs −2.5% on twin-hubs.
Deferred rather than retired because the **503 body** — the case where
the reading is about a peer you were about to keep hammering — is
untestable in a sim with no admission gate, exactly as F4 is; the
piggyback should ride §4.2 step 3's shed path instead.

**F10 — the scheduler has no speed signal in production, and it changes
how every paragraph above should be read (2026-07-27 — full result in
`SCHEDULER_QUALITY.md` §4.5).** Everything above is Tier-1: measured on
a simulator where each node advertises a `BenchmarkResult`. **No node on
this mesh ever has.** `run_baseline_benchmark`
(`sovereign-inference/src/benchmark.rs`) had zero callers,
`set_local_benchmark` (`peer_inference.rs`) had zero callers, and
`build_local_capabilities` hardcodes `benchmark: None` into every gossip
tick — under comments that used to describe the startup probe and a
`with_benchmark` setter as though both existed. Neither did; both
comments are now corrected in place.

**As of 2026-07-28 both dead producers are deleted**, along with
`InferenceRouter`'s `local_benchmark` field, so the local
`LocalCandidateView.benchmark` is now a literal `None` with no state
behind it. Production is blind by *construction* rather than by
accident, which is what makes the `blind-shipped` arm a measurement of
the shipped system rather than of a state it merely happens to be in.
Leaving the probe in place was the standing invitation the paragraph
below argues against.

So `throughput_factor` has two sources and production supplies neither
(the observed decode EWMA is gated behind a `samples >= 5` the ranked
path never reaches for a peer), leaving it at **neutral 1.0 for every
peer on every fleet**. Read the `mixed-hubs` sentence above with that in
mind: "the product already sends zero turns to the 11 tok/s hub,
`throughput_factor` 0.55 is decisive" is true of the *simulated* mesh
and false of this one, where that hub scores 1.0 like everything else.
F3 is not a weak term; it is a constant.

Two arms price it (`blind-rate-card`, and `blind-shipped` = the mesh as
it runs tonight, now the as-shipped denominator in place of
`blind-observations`). The rate card is worth **0% on five of six
fleets** — including `heterogeneous-fleet` — and **−32% mean on
`mixed-hubs` alone**, because the clamp at a 20 tok/s reference means a
card only carries information about a node *slower* than reference, and
`mixed-hubs` is the suite's only fleet containing one.

**The obvious repair is a measured regression, so it is filed
DO-NOT-BUILD.** Adding a call site to the (now deleted) probe wires the
`Speed::Fast` slot: a ~2.5 GB model's rate stamped in as the baseline,
which `throughput_factor` then extrapolates up to a 21 GB candidate on a
*linear* size law. Decode is bandwidth-bound and the law is false, and
the term's clamp is one-sided, so the error can only push large models
down. New knobs `SimConfig::probe_baseline_size_gb` /
`probe_sublinearity` measure it as `rate ∝ size^-β` (β=1 is the code's
own assumption and reproduces the un-probed rows exactly): at β=0.7 the
"win" grows to −56% while declined capability upgrades double 31→67, and
at β=0.5 real downgrades appear. An honest card costs no quality — but
only if it describes the model being *scored*.

`svrn mesh bench` (below) is the per-model measurement that condition
asks for, and it **deliberately does not write here.** Its number is
aimed at a human deciding whether to add a machine, not at the ranked
dispatch — and `throughput_factor` would extrapolate away from it
through the same one-sided clamp the moment it arrived. Same number,
different consumer. Pointing it at `NodeCapabilities.benchmark` ships
this section's regression with no other code change; a future reader who
"completes the wiring" while citing §4.5 correctly will have aimed it at
the wrong target.

This also settles why §4.1 cannot ship: `PredictInputs::from_candidate`
reads the advertised benchmark and nothing else, so unhardcoding
`RankObjective::Product` today would yield `Unpredictable::NoThroughput`
on every candidate of every request. The hardcoded switch is not the
blocker; the missing rate card is.

**Shared-model fleet churn/failover hardening (Phase 3).** A fleet sharing one
distributed primary stratifies into anchors (hold the RPC layer-split) + a
consumer ring. Anchors advertise `NodeCapabilities.anchor` (`AnchorProfile{
can_anchor, vram_gb, model_resident }`, populated env-derived in
`build_local_capabilities`); `discover_rpc_workers` filters candidates to
`can_anchor` so a casual peer never joins the split, and anchors get the
stricter `worker_eligibility::EligibilityConfig::anchor` profile (settle 300s,
quarantine on first flap). The RPC reload loop (`daemon_cmd::bootstrap`) does
**shrink-fast-prune** — an anchor dropping out of the loaded set reloads
immediately (prune before `GGML_ABORT`) while pure grows keep the 20s debounce.
**Host failover:** every anchor runs the discovery loop but only the elected
host distributes — `partition::should_host(self, host_node_id_pin, eligible_anchors)`
(pin wins while eligible, else `elect_leader`), re-evaluated each tick over
gossiped membership, published to `GET /v1/mesh/status` (`shared_model_host`) so
the mesh soak asserts the `shared_model_single_host` no-split-brain invariant.
Split-brain during convergence is bounded by the eligibility settle + the
quorum/pooled-memory gate (`InsufficientCluster` → "forming") + consumer
local-fallback. NOTE: the demoted-host model-teardown + full failover timing are
multi-box-only to validate (run `scripts/mesh-soak.sh`).

**Discovery never probes a worker over the link that worker's own tensors are
saturating (`daemon::reaffirm_plan`).** Gossip-Online membership — not a probe —
is the liveness signal for a worker discovery has already resolved once, because
the probe rides the congested path while gossip rides a separate one with a
looser budget. So a known **direct-ip** worker is re-affirmed from cache, and a
known **iroh-bridge** worker is re-minted from the transport's local bridge cache
(loopback only); only an unknown or probe-host worker pays the full `/status`
probe. Both known-worker cases trade the same way: a dead rpc-server behind live
gossip surfaces when ggml's RPC connection fails → supervised reload, not at
discovery. The bridge case is load-bearing because a non-direct endpoint gets no
stickiness (`sticky_endpoint` holds only direct-ip), so one starved probe used to
read as "worker absent" → flap → quarantine, compounding to 300s against a peer
that was serving throughout. Underneath, `HttpBridge::retarget` keeps a bridge's
loopback port stable across a peer's gossiped dial-info change (retarget in
place, don't rebuild), because that port IS the worker's endpoint string in
ggml's device list — minting a new one made an unmoved peer read downstream as a
stream of different workers.

**Byte-mass-aware split (`plan_shards_weighted`).** The placement policy apportions
each device a CONTIGUOUS block range whose *bytes* — not block *count* — are
proportional to its VRAM. This is the split the live load runs (`rpc_distribution`
overlays the model's real per-block byte mass from `rpc_warm_cache::tensor_sizes` on a
cache-miss, falling back to the count split only if the header read fails) AND the one
`mesh plan` previews — one function, so preview and reality can't diverge. It exists
because the big open-weight models are MoE, and MoE mass is deeply non-uniform: routed
experts (`blk.N.ffn_*_exps`, `is_routed_expert_tensor`) are **~88–93 % of the bytes**
but COLD (only the router's top-k run per token), and a hybrid SSM+MoE stack alternates
a ~20 MB attention/SSM block with a ~1.3 GB MoE block — a measured **62× per-block
spread**. Count-proportional apportionment (the old `plan_shards`, now the
`block_bytes == []` special-case) hands a small node a heavy contiguous run and OOMs it;
byte-proportional keeps each node ∝ its VRAM (e.g. 24 GB + 16 GB nodes → 18.6 GB + 12.3
GB of a 62×-spread hybrid). The output head is folded onto the host's budget. Ranges
stay contiguous, so single-stream decode keeps its **`D-1` hops per token** and a
layer's experts are never scattered across nodes (cross-node expert-parallelism would
add per-layer hops — wrong for single-stream; cold-expert→CPU offload buys nothing on
unified-memory APUs). See [[project_moe_byte_aware_split]].

**Pre-flight planning — `svrn mesh plan`** (`sovereign-cli-llm::mesh_cmd::cmd_plan`).
An offline dry-run of that split — a GGUF header-table parse, no model load and no GPU,
instant even on a 400 GB split — so you can see whether a model fits a mesh *before*
loading it. It shows the *bytes* each device holds and whether each one *individually*
fits. It also reports the MoE hot/cold
mass breakdown, whether per-block mass is uniform or skewed, and a **node/hop advisor**
— the minimum nodes that hold the model (fewest of the largest devices whose pooled
VRAM covers `model × headroom`) and the resulting hops, flagging when the mesh is spread
across more nodes than the mass needs. It frames this as a tradeoff, not a win button:
fewer nodes cut per-token hop *latency*, but net tok/s depends on the host — on a
memory-bandwidth-bound host (a unified-memory APU) offloading layers frees host
weight-read bandwidth and can raise *throughput* despite the extra hop (the measured
122B ran ~20% faster distributed 36/12 at 17.3–17.9 tok/s than solo at 14.8). So the
advisor reports the hop cost without claiming fewer nodes is always faster. `--from-mesh` plans across the
running mesh — each member advertises `vram_gb` + `can_anchor` on `GET /v1/mesh/status`
(`MemberDto`), the VRAM sourced from `rpc_distribution::local_gpu_total_vram_gb` (the
ggml device total, which unlike sysfs sees the full unified-memory pool on AMD APUs —
~124 GB on Strix Halo vs sysfs's ~0.5 GB dedicated carveout); `--devices 64,32,32` plans
a hypothetical mesh. The headroom factor is operator-set — `[shared_model] headroom` →
(bootstrap bridge) `SOVEREIGN_RPC_HEADROOM` → `rpc_headroom_factor()`, default 1.2,
replacing the hardcoded ×1.2 — and `mesh plan` defaults its `--headroom` to that same
resolution order, so the preview's headroom is the one the load executes with. Exit
codes: `0` fits, `1` won't fit, `2` bad args.
See [`docs/RUN_A_BIGGER_MODEL.md`](../docs/RUN_A_BIGGER_MODEL.md).

**Per-device fit — one decider, both sides (2026-07-28).** Until this date the
live host gated only on *aggregate* pooled memory (`pooled >= model_bytes ×
headroom`), so a cluster that cleared the aggregate gate could still hand one
worker more than it had; `mesh plan` caught that in its own private fold, which
meant the preview and the load could disagree about the thing the preview exists
to predict. Both now call **`rpc_warm_cache::shard_fits(plan, capacities, mass,
headroom) -> Option<Vec<ShardFit>>`**, beside the planner that produced the
split, over a `ModelMass` from `model_mass_from_sizes` (the same GGUF
header-table decomposition, replacing two divergent folds).

Three shapes are load-bearing. It returns **one row per shard, fitting rows
included** — a `Result<(), Overflow>` would force `mesh plan` to keep its own
traversal to print `ok +12.4 GB`, and a second traversal is the drift being
removed. `None` means **"cannot judge"** and is *not* a pass: an unread tensor
table would otherwise clear every device on the strength of zeros. And
capacities arrive in **plan order** (workers first, host last) while rows display
in the operator's `--devices` order — two permutations that look
interchangeable, pinned by a test.

`DistributionPlan` accordingly carries `device_vram_bytes: Vec<u64>` in place of
the summed `pooled_vram_bytes` (the sum is still what the quorum gate checks),
plus a `mass` computed **before** the plan-cache branch so a cached plan is
judged against the same numbers a fresh one is. A refusal is
`LoadPlacement::WorkerUnfit` / `PlannedDistribution::WorkerOverflow` /
`DistributedWarmOutcome::WorkerUnfit` — a **new** variant rather than a reuse of
`InsufficientCluster`, because pooling more memory does not fix an overflow and
saying "the cluster is forming" sends the operator looking for a peer that is
already there. `resolve_placement_inner` must **not** route it to `gate_local`:
falling back to a local load of an 80–90 GB model by a path that looks like
resilience is the 2026-07-27 session-kill. The compute-child path **parks**
(`bootstrap::park`, `retry_at = None`) rather than retrying, because an overflow
is not time-fixable — the existing worker-set-change re-plan is free and is the
only event that could change the answer. The refusal says **lower** the
headroom, not raise it (`need = held × headroom`), and names
`SOVEREIGN_SKIP_PER_DEVICE_FIT=1` for the one real false-positive: on a reload a
worker still holding its previous shard under-reports free memory.

**Measuring what you are running — `svrn mesh bench`**
(`sovereign-cli-llm::mesh_bench`). The producer for the `speed` block `mesh plan`
reports. **It measures the configuration that is loaded and never loads the one
it wants to measure** — there is no slot argument, so there is no slot to get
wrong, which is the mechanism satisfying `SCHEDULER_QUALITY.md` §4.5's "probe the
model being scored". An optional `<model.gguf>` is an *assertion*: fingerprinted
header-only against the resident primary, mismatch → exit 3 naming the config
line.

It fires real streaming completions at `POST /v1/chat/completions` and timestamps
SSE frames as they arrive, so the number includes the actual RPC split and iroh
path; `decode_tok_s = (content_frames − 1) / (t_last − t_first)`, steady state,
TTFT reported separately rather than smeared in. `prefill_tok_s` comes only from
the server's `usage.prompt_tokens` and renders `n/a` otherwise — never
`len()/4`. The probe (prompt, token budget, timing formula, guard set) is fixed
by `mesh_measurements::PROBE_VERSION`; there is deliberately no `--max-tokens`,
because a knob whose adjustment invalidates comparison against every prior record
while looking like harmless tuning is a trap.

**Nine validity guards**, six ported from
`scripts/measure-distributed-decode.sh` (each earned by an observed false result)
plus three new. Ported: which slot served it (below); per-frame timing; placement
re-read after the run; peer liveness before **and** after; a canary first; host
survival (from `/status` uptime going backwards, which unlike `pgrep` cannot
match a wrapper script or a deleted inode). New: `content_frames >= 32`,
inter-trial spread ≤25%, `finish_reason ∈ {length, stop}`.

**The served-slot guard, and why the obvious version of it does nothing.** The
shell script asserted that the SSE `model` field names the primary. On this
server that field is a **verbatim echo of the string the client requested** —
every frame says `commonwealth/primary` because that is what was asked for,
whatever actually answered. Measured 2026-07-28 on the first live run: with the
122B's compute child in `lifecycle: starting`, requests to `commonwealth/primary`
returned ~100 tok/s (impossible for that model, which does ~14.8 local) and the
frame-name check passed cleanly. The script has the same hole and never caught it
because it only ever ran when the primary was up.

`mesh_bench::primary_is_serving` is the check that attributes, run before **and**
after the trials, and it has to understand two hosting modes.
`ComputeRoutedProvider::resident_slots()` forwards the *in-process* engine's
view, and the in-process engine never loaded a child-hosted model — so
`resident` is `false` **forever** for a perfectly healthy child-hosted primary. A
guard reading only that field would refuse every honest run on this
configuration, which is a worse failure than the vacuous check it replaces. So
the predicate is "in-process `resident: true` **or** a `compute_children` entry
with a matching `model_id` and `lifecycle == "serving"`"; `starting` and
`warming` deliberately do not count, because those are precisely the states in
which something else answers. The canary waits on this same predicate rather than
on "I got tokens", since stopping at the first answer hands the timed trials to
whichever slot is currently covering. A run tripping any guard is still **written** — a discarded
failure teaches nobody anything, and dropping it silently makes the tool
retry-until-lucky — but `lookup` never returns it. Exit `0` valid · `1` guard
tripped · `2` bad args · `3` assertion failed · `4` nothing measurable · `5` no
daemon.

The key it files under must be the key `mesh plan` constructs, or every record is
unfindable. Both build `PlacementShard`s over **only the devices that hold
blocks** — an idle peer changes nothing about how the model decodes, and bench
has no idle device to report — and both derive the digest's `mode` from shard
topology rather than from the daemon's mode string (which has five values:
`local`, `distributed`, `child-distributed`, `stream-split`, `forming`) so the
two vocabularies cannot drift. The daemon's own word is preserved verbatim in the
record's `placement_human`.

**A record carries the pre-image of its own key** (`MeasurementRecord.witness`,
added 2026-07-30). Both digests in the key are one-way, which is right for
`lookup` — an equality test — but it meant a record could state a number without
being able to say what the number was *for*. Two runs of this fleet four hours
apart filed under different placement digests with identical `placement_human`
labels, and an exhaustive search over every integer split of the model across
both machines could not reconstruct what the earlier one described. So
`PlacementWitness` stores the exact `(mode, total_blocks, shards)` the digest was
computed from, plus a `MachineWitness` per named machine (`vram_gb`, `backend`)
because `host_hw_fingerprint: 7602642063143971880` is not something a reader can
weigh. It is *checkable* rather than asserted: `PlacementWitness::explains`
re-runs `placement_digest` over the stored fields, and `Configuration::faithful`
applies that check at the point of use, so a witness built from different inputs
than its key is treated as absent rather than quoted. Descriptions are
deliberately outside the digest — improving what a peer advertises must not
orphan every record naming it — and are deliberately **capacities and labels,
never rates**, so nothing here can be divided by anything else to resurrect the
§4.5 size-law. Not a schema bump: unlike v1→v2, whose missing field was a *key*
field, old rows still serve exact hits and are kept saying "not recorded".

**A record also carries the conditions it met** (`MeasurementRecord.conditions`,
added 2026-07-29). The witness above explains *what* a run measured; this is the
other half — the co-resident slot set, host RSS before and after, daemon uptime,
and the wall-clock span of the trials. It exists because four runs under one key
came back 7.75 / 8.38 / 8.53 / 11.08 tok/s and nothing recorded could say which
of them met a busy machine. Every field is something that can differ between two
runs of an *identical* configuration, which is exactly the class the key cannot
hold — so `RunConditions` sits beside the key and **never in it**. Keying on them
would give every run a unique unmatched key, `lookup` would never find more than
one run, and the variance the field exists to expose would become structurally
invisible (test `conditions_never_reach_the_key`). Two traps are closed by
construction: an empty slot list renders as the *finding* "nothing else resident"
rather than as silence, and a role whose `model_id` equals the primary's is not
counted — with `[models].fast` absent, `fast_path()` falls back to the primary
GGUF and `/status` reports a `fast` slot holding the measured model itself, which
filtering by role name alone would have recorded as its own co-resident. Old rows
say "conditions not recorded", never implying a quiet box. `link_rtt_ms` stays
`None`: iroh 1.0 exposes no per-peer RTT on `remote_info`, and a timed round trip
would measure the link *plus* the peer's request handling, which must not be filed
under that name. `mesh bench --history` also prints each row's abbreviated `pd2:`
key and warns when two rows share a `placement_human` under different digests —
the misreading that once produced a reported variance that was never real.

This is what makes `near_misses` load-bearing. The key pins the exact split *and*
the exact silicon, so a reader on hardware we have never seen essentially never
gets an exact hit, and `differs by: split, host-hardware` gives them nothing to
judge with. `near_misses` now returns a `Difference { facet, theirs, ours }` per
facet — `beefymac 12 · ruggedfox 36 +head` against `beefymac 24 · ruggedfox 24
+head` — rendered in both the human plan and `--json` (`differences[]`), with
`differs_by` *derived* from it so the two cannot disagree. `theirs`/`ours` are
`None` where that side kept no witness: the difference is real, and declining to
characterise it is the point. `n_ctx`, `link` and `probe_version` live in the key
itself, so even a pre-witness record reports "measured: 32768 · yours: 8192".

**Measurements travel** (2026-07-30). A measurement is worth most to the machine
that did not take it: locally it recalls what a run felt like, on a peer it
answers what a configuration *would* feel like on hardware the reader cannot try.
Records travel as versioned `to_wire` envelopes (a peer on another
`SCHEMA_VERSION` is dropped by `from_wire`, not half-read). `wire_key` —
`{measured_at:010}/{hash}`, derived from the record and lexicographically
chronological — is what makes a record identifiable by content; the rate enters
that hash **quantized to 0.001 tok/s** because `serde_json` is built without
`float_roundtrip`: a record passes through JSON twice and can come back one ULP
off, which would otherwise let the same run compute two keys.

**The transport is the ring rail, not the gossip KV store** (cw-lift 2d,
2026-09-04). `mesh-measurements` is the first namespace to move, and it moved
because it is the one that fits: `MAX_RUNS_PER_KEY` bounds it, so the journal
never has to forget. It is now in `GOSSIP_EXCLUDED_APP_IDS` — not a privacy
judgement, and it still reaches every peer; the entry is the receiving half,
stopping a peer on an older build re-creating the dead KV namespace here.
`sovereign_mesh::measurements_rail` is the only publisher and it appends to
`rings/mesh-measurements/ring_oplog.jsonl`, which ordinary `ring_sync`
anti-entropy carries. A record rides as `to_wire`'s exact bytes inside a rail
payload, as a JSON **string**: a rail payload may not contain a fractional
number and a `MeasurementRecord` is nine `f64`s, and a string has one spelling
whose bytes are never re-serialized — the hazard the rule guards against is
absent rather than checked (`a_measurement_record_cannot_be_a_rail_payload_directly`).

Three constraints make travel safe rather than merely working:

- **Peer records never enter `MeasurementFile`.** `lookup` still answers only
  "what did *this* machine measure", so no peer's number can be served as the
  reader's own — `mesh plan` keeps saying "not measured **here**" and offers the
  peer's beside it. They reach the operator only through `near_misses`, carrying
  `NearMiss.taken_by` (`None` = this machine, `Some(name)` = a peer).
- **Invalid runs do not travel.** `to_wire` refuses them: a failure is glassbox
  material for the operator who caused it and noise, or worse a mis-read
  capability claim, to everyone else. `--history` still shows them locally.
- **Origin comes from the SIGNATURE, not the payload.** A node cannot claim to
  be someone else by writing a name into bytes it controls. The journal line's
  `actor` is the public key that signed it — the one field a writer cannot forge
  for someone else (ARCH §18.1) — and both halves of the attribution, node id
  and display name, are resolved from it through the ring roster.

An exact-key *peer* hit is kept (`NearMiss::is_exact`) rather than filtered as a
non-miss — someone with the same silicon, split, link and context measured the
thing being asked about, and that is the most informative record travel can
deliver. It is still never the headline.

**The roster bridge, and why there is no roster route.** A ring journal admits
an op only if a roster claims the key that signed it, and `svrn ring`'s roster is
written by hand from the CLI — deliberately unreachable from the rail, so a
deployed app cannot admit signers to a ring (ARCH §7.1). A namespace the *daemon*
publishes to needs a roster anyway, and the shape that keeps §7.1 is
`sovereign_mesh::ring_roster::MeshRoster`: derived from the membership this node
already holds, never accepted over the wire. The bridge is one equality —
`MemberRecord.node_pubkey` and the rail's `Op.actor` are both
`hex(verifying_key)` over the SAME `load_or_generate_node_key`, so there is no
mapping table (`a_member_pubkey_and_a_rail_actor_are_the_same_spelling`).

Three decisions the derivation makes, each pinned:

- **A member with `node_pubkey: None` is not in the roster, under any
  placeholder** — a shared default would collide every unidentified node into
  one identity. Its ops are `UnknownSigner` gaps, and the gaps are REPORTED
  (they reach the reader in `unreadable`), never swallowed.
- **Those gaps HEAL.** The roster is a parameter of the read, not a file:
  nothing is dropped when a signer cannot be placed, so the same journal admits
  the same ops the moment that node's gossip round stamps its key — under the
  same actor, because the signing key is stable on disk
  (`an_op_from_an_unidentified_peer_is_a_gap_that_heals_when_its_key_arrives`).
  This is the whole reason `MeshRoster` has no writer.
- **A tombstone retires a member, not their journal.** A departed member's keys
  stay in the roster; dropping them would turn their whole history into gaps on
  the day they left.

The pipes: `svrn mesh bench` runs in the CLI and the daemon owns the journal, so
the door stays at `POST`/`GET /v1/mesh/measurements` (`mesh_http.rs`,
localhost-only; `?include_self=true` is the diagnostic that shows what this node
has put on the ring). The CLI's only caller is `mesh_travel.rs`. Disk is written
*before* the door, so `mesh bench` works with no daemon and a refusal reads as
"not on the ring yet", never as a lost record — which is what makes refusing
honest for a node that is not in a mesh yet and therefore has no roster that
could claim its signer. `bootstrap::reconcile_local_measurements` is the closure
loop for that: once per boot, deferred until `app_state` answers, it appends any
local record the journal lacks, keyed by `wire_key` so it is idempotent by
content and the journal cannot grow one copy per start. It replaced
`republish_local_measurements`, which had to re-upload the whole file on every
restart because the KV buffer was in memory.

**`republish` is also the SNAPSHOT behind this namespace's seal** (cw-lift 4).
`rail_kv_pump` checks every namespace it owns against `SEAL_AFTER_OWN_OPS`, and
this one has to be reached from there rather than from a drain: it is
gossip-excluded, so it never enters the outbox, and its acts go straight onto
the journal from `POST /v1/mesh/measurements` with nothing above them counting.
A KV namespace's snapshot is its live store rows; this one's is
`mesh_measurements::load()` through `republish`, which is idempotent by
`wire_key` and therefore re-appends exactly what the seal retired. It runs AT
the seal and not at the next boot — the window between them would otherwise be
a ring whose measurements had been retired and not yet replaced.

**The rail has ONE roster reader, and this namespace is why** (2026-09-08,
the fix 4a's live run demanded). `RingRail::roster(&journal)` answers every
caller that holds a journal and a namespace — the append and log routes in
`routes_rail.rs`, the sync-side prune in `ring_sync.rs` — from a
`RosterSource` registered for the namespace when one is, then from
`roster.json`, then from the rail's DEFAULT source (ring-room, 2026-09-18: an
app applies to everyone in the mesh, and `svrn ring roster add` writing the
file is the narrowing primitive); `RingJournal::roster_file` is now named as the file half and has two
callers, that door and the CLI writer. `MeshRosterSource` (in `ring_roster.rs`)
is the default and the one registration, `REGISTERED_NAMESPACES`
(the daemon's own rings — `mesh-measurements` and the six KV namespaces — which no file may narrow) — installed beside the rail itself in
`daemon.rs` through `MeshRosterSource::install`, the one place a namespace
and its derivation meet. Until then those three paths read the file — empty
for this namespace — so the daemon refused its own key at the append door and
a peer's seal retired nothing on the ring that most needs retention. The
control half is kept in
`a_peers_seal_prunes_the_daemons_own_namespace_whose_roster_is_derived`.
`svrn ring log mesh-measurements` and `svrn ring seal mesh-measurements` work;
`svrn ring roster …` on it still REFUSES, since a `roster.json` there would be
read by nothing. One predicate, `ring_cmd::refuse_derived_roster`, and it names
`svrn mesh status` / `svrn ring log` instead.

**Not** routed through `NodeCapabilities.benchmark`, which stays `None` — that
field feeds the ranked-dispatch clamp and arms the §4.5 size-law;
`gossip_never_advertises_a_benchmark` fails the build if it is populated.

The strong-peer-topology roadmap (latency-class hierarchy: cascade
routing, draft-on-spoke/verify-on-hub speculation, hub queue
discipline — each reality-checked against this codebase) is
[`docs/specs/MESH_INFERENCE.md`](./docs/specs/MESH_INFERENCE.md).

`commonwealth-inference/orchestrator/` was DELETED 2026-09-03 — 1,904 lines
over six files whose `Orchestrator::new` was called from nothing but two
integration tests. The liveness investigation it was "flagged for" in
OICP_RATIONALIZATION.md returned the answer: not live. It held `ManagedProcess`
(lifecycle states + SIGTERM-then-SIGKILL), `HealthTracker` (5s poll, 20-sample
latency window, `Unresponsive` after 3 failures), a `FaultDetector` and a
`GracefulDeparture` countdown, and none of it ran.

Multi-process supervision on the path that IS live is
`sovereign-compute/src/supervisor.rs` — `SupervisorState`, `graceful_kill`
(SIGTERM, grace, SIGKILL) and health polling. It has NO departure countdown and
NO fault detector, so those two parts of FE-139 are unimplemented rather than
implemented-elsewhere; `quality/conformance-specs.toml` FE-139 carries the
statement and the requirement is write-work again.
- `GracefulDeparture` — countdown state machine
  (`Announced → Rebalancing → Draining → Complete`), driven by
  `Orchestrator::depart_gracefully` / `announce_departure` +
  `complete_departure`. From the announcement the node refuses new shard
  plans, which is the state machine's only externally visible consequence
  and the thing that keeps it from being a log line. `stop_all` is the
  ABRUPT path and says so; the standby transition in `apply_mesh_plan`
  departs instead. Nothing constructed a `GracefulDeparture` before
  2026-09-02 — it was unit-tested, wired to nothing, and `stop_all`'s doc
  comment claimed its job (FE-139).
- `FaultDetector` collapses health changes into `FaultEvent`s. STILL
  UNWIRED: nothing outside its own tests constructs one.

### HTTP API

Two listeners, two trust domains.

**Client API — :9741, binds 127.0.0.1 by default.** Secure by default:
the wildcard bind is reached only when something explicit asks for it —
an explicit `[daemon] client_bind`, or the `client-exposed` marker
`expose_client_api` writes on `mesh create`/`join` (federated inference
needs peer reachability). An ENCRYPTED mesh forces it back to loopback
whatever the config says: the iroh acceptor is the sole ingress. The one
decider is `sovereign_daemon::daemon::resolve_client_bind_posture`.

A non-loopback bind carries a bearer token or serves nobody — the token
chain is env → `[daemon] client_token` → generate-and-persist, and when
even that fails the posture installs NONE, which makes `client_auth`
refuse every remote caller rather than serve unauthenticated. Loopback
callers pass free; the layer has exempt paths for federation/health.
Added with the SaaS hardening, 2026-07; extracted out of `start_daemon`
and given a test 2026-09-02 (UI-22).

A non-loopback caller can now present one of **three** bearers, matched in
that order. `client_token` is the daemon-wide one and unlocks everything.
A **named client token** (`sovereign-daemon/src/client_tokens.rs`,
`tg-3-tokens-have-names`, 2026-09-21) unlocks the same surface but belongs
to ONE machine: minted by `svrn mesh token --new <label>`, kept at
`<data_dir>/client-tokens/<label>.token` (0600 in a 0700 dir, the shape of
the MCP secret store), admitted through the same constant-time compare
after a fingerprint bucket lookup, and revoked by label in the SAME daemon
lifetime — the store drops it from memory before deleting the file, so
there is no restart and no other credential is disturbed. The admit line
carries the label, never the token. `[daemon] client_tokens` is the
posture (`"shared"` by default, `"named-only"` refuses the daemon-wide one
with a sentence) — a closed set whose unknown spelling declines to start,
exactly as `internal_auth` does on the other port.
An **ephemeral guest grant** (`commonwealth-knowledge::guest_grant`,
2026-08-27) is the narrow one: short-lived, revocable, and bound to a
closed `Scope` enum whose `paths()` is the only route allowlist there is.
A guest is not a mesh member — no `mesh_secret`, no gossip, no invite key
— and cannot mint further grants, because no `Scope` variant names
`/internal/*`. `svrn mesh grant` mints one (`--rail <ns>` adds the rail
scope) and prints a `sovereign://guest/…` link, plus with `--qr-svg` the
https form (`<url>#token=…`, the bearer — and, when present, the `at=`
digest marks and the `iroh=` dial string — in the fragment) as an SVG QR
code; `svrn mesh use` accepts the first and repoints
`svrn chat`. The auth layer never matches on a `Scope` variant: it asks
`GuestGrant::permits_path` and inserts the grant, so a future scope is a
variant plus its `paths()` arm and touches neither auth nor the wire.

**The ring rail is the second scope, and the first deployment target**
(`sovereign-daemon/src/routes_rail.rs` + the `commonwealth-rail-core` /
`commonwealth-rail` pair, ring-deploy S1–S6, 2026-08-30). **It became two
crates on 2026-09-04** (cw-lift 1b), carved out of what was then
`commonwealth-knowledge`'s own rail module:
`-rail-core` is the FOLD — the vocabulary, Ed25519 authorship, admission into
one total order, and the per-actor sync digest, with no filesystem, no clock
and no socket — and `-rail` is the JSONL journal that calls it. The split is
what the campaign's second lift needs: a second application composes on the
fold without inheriting a file layout, and the fold's whole in-repo dependency
surface is `oplog`. `commonwealth-knowledge::rail` survived as a re-export for
one commit so no consumer moved in the same diff; order 1c dropped it, and
`sovereign-grants/src/` carries no rail module today. The three
`[[forbid]] from = "commonwealth-rail*"` blocks in `quality/ARCH_LAYERS.toml`
are what hold it — to `corpus-engine*` (the edge order 1a paid for), to
`sovereign-*`, and back to `commonwealth-knowledge`, which layer ordering
cannot see because both sit in `mesh-foundation`. A Commonwealth mesh had no verb for "make this
exist for exactly my trust ring": a VPS makes it public infrastructure, a
Discord bot puts the data at Discord, and the local-first stack syncs data but
gives you nowhere to run anything and no idea who is asking. `Scope::Rails(ns)`
names exactly one namespace, and `/v1/rail/{append,log}` take **no namespace
parameter** — an app cannot reach another app's namespace because it has no way
to *say* one (§7.1). An operator, who holds no grant, names it explicitly.

**The rail carries an opaque payload, and that cut is the design.** It began
as an expense ledger whose journal line *was* an `ExpenseOp` and whose reader
had one thirteen-variant failure enum — and the enum split cleanly in two the
moment anyone looked: eight variants about delivery and authenticity (torn
line, bad signature, unknown signer, rewritten id, sequence hole, sequence
fork, dangling correction, newer-format line), five about money. Not one of
the first eight knows what an expense is, and a tool-lending board needs every
one of them. So the line is now `RailAct` inside `Op<SignedOp>` —
`Record{payload}`, `Correct{corrects, replacement?}` or `Seal`, a per-actor `seq`, an
optional `on_behalf_of` naming whose words the act was when a door signed for
somebody the ring holds no key for (a name, carried and never interpreted — the
rail does not look it up in the `Roster`; absent by default and absent from the
signed bytes when absent, so every op written before it verifies unchanged), and
an Ed25519 signature over a domain-separated message that binds the namespace,
so an op lifted from one ring and replayed into another fails the signature
rather than a downstream check. **`Op.actor` is the signing public key**,
because it is the only field on the line the writer cannot forge for someone
else (§18.1); a `Roster` binds keys to display names, and one person with two
laptops is two keys in one row. The daemon signs through a closure-shaped
`RingSigner`, the same seam `self_dial_signer` uses, so `AppState` holds no
key material. **Verification is the matching seam, and until 1e it was not
one** — `RingSigner` was a trait and verifying was a call to a free function,
so one half of one scheme could be replaced and the other could not, and both
halves working is exactly what made that invisible. `RingVerifier` is now the
question's only door: `Ed25519Verifier` is the shipped implementation, named at
every call site rather than defaulted so which scheme judged an answer is
greppable, `sig::verify_ring_op` is crate-private behind it, and the admission
trace carries `verifier=`. A verifier is not a roster — one that accepts
everything still cannot admit a stranger, which is pinned.

**`Payload` is a type and not a `serde_json::Value`, and the reason is a bug
that would not have surfaced for months.** A signature covers bytes; a `Value`
has no bytes, it has a serializer, and which bytes that serializer emits for
an object depends on `serde_json/preserve_order` — a Cargo feature that is ON
in this workspace and that any crate added later can turn off. Flip it and
every signature in every ring on the mesh stops verifying at once, presenting
not as "a feature changed" but as a journal that has become entirely
`BadSignature`. A typed body never had this problem (serde writes struct
fields in declaration order); making the body opaque introduces it. So a
`Payload` is **canonical by construction** — objects rebuilt with sorted keys,
recursively, including on deserialization, so a line off the journal and a
body off the wire are canonical too. Floats are refused outright for the same
reason one level down: `1e2`, `100.0` and `100` are one value with three
spellings and the choice is the library's, so a payload carries whole numbers
and the refusal names the fix (use cents, grams, milliseconds).

`admit(ops, skipped, roster, namespace, verifier) -> (acts in ONE order, gaps)`
is **a
function of the op SET, not of arrival order** — nineteen laptops gossip in
nineteen orders, and admission that depended on order would have two housemates
reading different answers off the same journal. That is pinned exhaustively
over all 720 orderings of a six-op fixture; asserting it at this layer rather
than over balances made it both stronger and true for every app that will ever
sit on the rail. It buys the property with: dedupe by re-derived `OpId`; a
content-derived total order `(ts_unix, actor, seq, id)` — the `seq` term added
2026-09-09 after a work-plane burst inside one second folded a `Lease` ahead of
the `Submit` that opened its handoff and a unit consequently ran twice; a void set built from every
correction at once (so a correction arriving before its target pre-emptively
voids it) that **never resurrects** — correcting a correction cancels its
replacement and leaves the original void, which is what "compensating entry,
visible" means; and sorted gaps, so two nodes agree on the *report* and not
merely the acts. Voided ops stay in the returned list, marked, so an app can
render what changed.

**Correction lives in the rail on purpose.** "This earlier act was wrong, and
it never comes back" is not an expense rule — a tool-lending board needs it
the first time somebody writes *I returned the drill* and then *no I didn't* —
and it is the rule most easily got wrong, because the void set has to be built
from every correction at once rather than by walking for liveness. One
implementation, and no app author re-derives it.

`gaps` is the half that refuses to fake completeness. A journal that cannot
say "I may be missing something" lets an app state a wrong total with complete
confidence (§18.3), so `/v1/rail/log` returns the admitted acts and the gaps
from one read, each gap carrying `message` — the rail's own sentence, so the
terminal, the app's page and the append door's 422 say the same words about
the same condition (§10.6).

**The money left Rust with the fold, and that is the stated cost.** The penny
remainder (\$10 three ways is 334/333/333), settlement idempotency, and the
five money-shaped refusals now live in `ring_cmd/templates/expenses.js` — the
reference app, which is also what `svrn ring new` scaffolds, so the thing a
housemate starts from is the thing the workspace gates. `expenses.test.mjs`
pins them and runs inside `cargo test`
(`scaffold.rs::the_reference_apps_money_rules_pass_their_own_tests` shells to
`node --test`; a missing `node` **fails** rather than skips, because
could-not-judge is not passed, §18.1). The app keeps the shape the rail gave
up: one `validate` that its own door and its own reducer both call.
`participants` is still an explicit list and the roster is still never read for
a split — the moment it is, adding a housemate silently re-divides every past
expense, and the test named after that moved to JS with the rest.

**The SDK ships the fold, not just the transport.** `window.ring` is `log()`,
`record()`, `correct()`, `fold(log, reducer, initial)` and `live.{send,drain}`
(the ephemeral lane, proxied as the `live` / `live-drain` ops so a page served
by `ring dev` can reach `/v1/rail/live` at all), and the fourth is
why it is an SDK rather than a fetch wrapper: it walks the rail's order and
skips voided acts and replacement-less corrections, so an author writes a
reducer and never touches `log.ops`. Hand somebody a raw log and hope, and the
first thing they write is `ops.filter(...).sort(...)` — and their house
disagrees with itself about who owes what.

The door is narrow now, and honestly so: it refuses a payload with no
canonical form, and it refuses to author under a key the ring's own roster
does not carry (which would otherwise produce ops every node reports as
`UnknownSigner` forever, silently). It has no opinion about whether an amount
is positive, because it cannot have one.

**Replication is its own loop, because riding the gossip push would have cost
~246 GB/day per node** (`sovereign-mesh/src/ring_sync.rs` +
`/internal/ring/sync`, ring-deploy S3). `gossip.rs` Step 4 shipped a full
mesh-store snapshot to every online peer every 10s — 8,640 rounds/day — and a
household's ~3,500 ops/yr ≈ 1.5 MB would have ridden every one of them, taxing
every other namespace on that body forever. **Step 4 is deleted (cw-lift 2e)**
and this loop is the only sender left, so that comparison is now history rather
than a choice between two live paths. The journal gets a **60-second cadence**
and syncs by **digest**: `{actor → contiguous high-water mark}`, ~600 bytes
regardless of history. *Contiguous* is load-bearing — a node holding seq 0 and
2 that advertised `2` would be answered "nothing above 2" and seq 1 would
never arrive, sitting as a permanent `SequenceHole` while both sides believed
they were in sync. An actor absent from the digest asks for everything.

**The run counts from a SEALED FLOOR, and that is what lets the rail delete**
(2026-09-04). Until then the mark was contiguous *from zero*: `digest`
early-returned for any actor whose seq 0 it did not hold, and `admit` walked
`0..=highest`. So a node that retired an old prefix advertised NOTHING for that
actor, every peer read that as "I hold none of theirs" and re-sent the whole
holding every 60s, while `admit` reported one `SequenceHole` per retired op
forever. **Compaction amplified traffic, undid itself, and looked like
breakage** — the rail could not delete anything at any granularity, which is
not a substrate a third party builds on. The floor is `RailAct::Seal`: an act
that retires everything its author wrote before it. It carries **no actor and
no range** — the floor is whoever signed it, at the seal's own `seq` — so
sealing somebody else's history is unwritable rather than refused (§7.1), and a
seal cannot claim past what its author reached. **Authored, never configured**:
a local truncation setting would put the disagreement one layer up, two peers
with different floors and one re-sending forever; a seal is a signed op in the
same total order as every other act, and the seal itself is how the floor
travels. `sync::sealed_floors` is the ONE reading of it (§10.6), and *which ops
it is handed is the safety question*: `admit` hands it only ops that passed the
signature, roster and fork checks, so a forged seal — the exact line a hostile
peer would push at `/internal/ring/sync`, which ingests as-signed — retires
nothing (§18.3); `digest` hands it the whole holding, the same trust the
contiguous mark beside it has always had, because being wrong there costs a
round while being wrong in `admit` states a total over a subset and calls it
complete. **The wire type did NOT change**: a peer's only use for the digest is
*which ops do I send*, and the answer is `(mark, ∞)` whether the run started at
zero or at a floor, so a build that knows about seals writes a byte-identical
digest to one that does not for the same holding — nothing to default, no
version to tag, and one fewer thing an old peer can fail to parse across a
60-second exchange between mixed builds.

Two idempotent calls converge both directions: `{digest, ops: []}` pulls what
we lack and learns the peer's digest; `{digest', ops: what_they_lack}` pushes
against it. The push is **author-blind** — a node republishes everything it
HOLDS — which kills three failure modes at once: the author's node dying
before anyone else came online, a peer restart wiping in-memory buffers, and a
housemate leaving with half the journal. It is also why there is no own-origin
skip to get wrong: `MeshStore`'s `origin` names the last *republisher*, not the
author, and this path has no origin field because the op carries its author in
a signature. `/internal/ring/sync` validates nothing about an incoming op on
purpose — `admit` is the one decider, and it has to be right anyway because
ops also arrive from disk. Watched: two-node partition drills at both levels
(pure journals, and through the route), plus a half-delivered peer whose gap is
named rather than silently totalled.

**A ring is READ by its roster, offered and served** (mp-2, 2026-09-20). A
signature answers "who may write to my journal" and says nothing about who is
*asking*, so until this rung a mesh member on no ring's roster was sent every
ring this node held and could have asked for any of them by name. Both halves
now decide on `sovereign-mesh/src/ring_roster.rs::roster_names`, one test read
through `RingRail::roster`: the sender filters its Online peers PER NAMESPACE
inside `run_one_round`'s loop (per round is the wrong grain — a peer may be on
one ring and not another), and `routes_internal/ring_sync.rs::roster_refusal`
refuses a 403 naming the namespace and the asker. A ring with no `roster.json`
— every app ring by default, and `REGISTERED_NAMESPACES` — is answered by
membership, so every member stays on it; the file-rostered `work` plane
narrows. An unreadable roster offers and serves nothing: under-share, never
over-share. The asker is the verified principal `mp-1` introduced, so an
`Unverified` caller is refused every namespace — but a caller that presented
NOTHING is `Anonymous` and is still served, which on a PLAINTEXT mesh is every
peer. That is an open gap, recorded rather than closed, and it is why the
encrypted posture (loopback-only internal listener, acceptor the sole ingress)
is the one the room runs.

**One BODY has a ceiling; convergence no longer does** (measured 2026-09-04,
cw-lift rung 2a; chunked by rung 2f the same day —
`sovereign-daemon/tests/rail_e2e/ceiling.rs` §"the convergence ceiling, and the budget
that ended it", plus the loop's own tests in
`sovereign-mesh/src/ring_sync.rs`). The receiver caps a request at
`MAX_REQUEST_BODY_BYTES` = 8 MiB (`server.rs:40`), so the per-body figure is in
BYTES and every one names its fixture: **9,599 ops** at 873.9 B/op (order 2's
594-byte ledger body), 13,731 at 609 B/op, 15,164 at a work-atlas
observation's 552 B/op (re-derived by `examples/rail_read_cost.rs`). Until 2f
that was the CONVERGENCE ceiling, and crossing it was silent: `DefaultBodyLimit`
answers **413 before the handler runs**, so the gauge could not fire; the gauge
was computed on the RESPONSE, the direction nothing bounds, so the rail's one
instrument watched the half that works; the sender mapped the status to
`Err("HTTP 413")` and filed a reachable peer at **debug** as
`peers_unreachable`; and the peer that had been refused reported zero ops,
zero gaps and `is_complete() == true` (§18.3). That mattered most in the one
case that cannot self-rescue — a node that has never seen this ring holds no
`rings/<ns>` directory, so `run_one_round` finds no namespaces and returns
before dialling (`:117`, `:124`): **it can only ever be told, over the
direction that has a limit.**

**The fix is a byte BUDGET and a repeated exchange, not a bigger limit.**
`RING_SYNC_OPS_BUDGET_BYTES` (`routes_internal/ring_sync.rs`, one decider
derived from `MAX_REQUEST_BODY_BYTES / 2` and never re-typed) stops
`ops_missing_from_within` at a chunk in BOTH directions — the response was the
unbounded half — and `exchange` repeats the pair until neither side moves,
bounded by `MAX_CHUNKS_PER_EXCHANGE = 16`. **Nothing on the wire changed
shape**: the exchange was already idempotent because `ingest_all` dedupes on
the content-addressed op id, so a partial one is safe. It terminates because a
chunk's first op is always one the receiver provably lacks — a contiguous mark
of `n` means they do not hold `n + 1`, and the ordered selection yields exactly
that op first — so every non-empty chunk moves the receiver's mark. The gauge
now reads the REQUEST, and a 413 comes back as its own `ExchangeStop::Refused`
counted in `RoundOutcome::peers_refused`, because a peer that answered is not
a peer that could not be dialled. Watched: a 10,000-op journal converging onto
a node with no `rings/<ns>/`, red first against an unbudgeted constant.

**The sealed floor shortens the exchange; it never was what moved the
ceiling.** Measured as a pair over one journal: a `Seal` appended while the
retired lines are still on disk sends the identical ops — `ops_missing_from` is
author-blind over what the node HOLDS, and an empty digest is missing all of it
— so it costs the same several chunks. **Deleting the retired lines is the
whole mitigation**: the suffix lands in ONE chunk, and a peer that has never
seen the ring reads a journal `admit` calls COMPLETE, because the seal travels
as its own op and holes are counted from the floor. Mutating that seal to an
ordinary `Record` reports 10,000 `SequenceHole`s instead, so the floor is
load-bearing and not the delete alone.

**Both halves landed together on 2026-09-07** (cw-lift 4a), because the same
measurement says neither is worth anything alone. The write half is not a new
verb: `RailAct::Seal` is `{"op":"seal"}` through the append door an app already
posts to, so it takes the author's next `seq` and travels the one total order
like any act. The delete half is `RingJournal::compact`, and the append route
runs it in the same request when the act was a seal — sealing and pruning are
one act to the caller, and its `retired` block reports what went, because a
seal's own 200 looks identical whether a thousand lines left the disk or the
prune was refused. Four properties are watched, each red under its own injected
defect: the floor is `Admission::floors` and never a second reading of the
seals, so a **forged** seal (refused by admission, believed by a naive re-read)
deletes nothing rather than erasing a member's history everywhere it lands; the
comparison is strict, so the seal that IS the floor survives and the compacted
actor keeps making a digest claim; a journal holding lines this build cannot
parse is not rewritten at all, since `SkippedLine` carries a line number and
never the bytes and the dangerous case is a line from a NEWER version; and the
kept set is re-admitted before the rewrite commits, so a prune that would raise
a gap refuses instead — which is how "delete only what a floor covers" is
encoded rather than remembered (§7). Pruning is not author-only: a seal binds
whoever admits it, so a peer drops what somebody else retired, which is the
half that actually bounds storage. **Who seals turned out not to be a policy
question.** `Seal` carries no actor, so sealing another's history is unwritable
and every alternative to self-sealing is already ruled out by the type; the
worry that a node which goes quiet never seals and grows without bound does not
survive contact, because an actor's history grows only when that actor WRITES —
an offline node contributes nothing further, and a writing node can always
seal. WHEN to seal stays the operator's, for the reason `sync.rs` refuses a
truncation setting: a local cadence puts the disagreement one layer up.

**Amended at cw-lift 4 for the daemon's OWN namespaces: the daemon writes
them, so the daemon seals them.** The operator's-call rule holds for an APP's
ring — sealing forgets history and an app's history is the app's. It does not
hold for
`sovereign-mesh/src/ring_roster.rs::DAEMON_OWN_NAMESPACES`, which the daemon
writes on a cadence nobody chose: nobody is going to run
`svrn ring seal inference` every few weeks, and a journal nobody seals grows
without bound on every node, so leaving the decision unmade IS a decision. It
is safe to automate here because both halves check themselves — `compact`
re-admits its own result and refuses a prune that would raise a gap, and the
SNAPSHOT re-appends the live set under its ORIGINAL `t`, so the fold puts every
row back where its author left it. That is exactly what `sync.rs`'s refused
truncation knob was not: an operator-set line with nothing checking what fell
below it. The threshold is ONE constant,
`rail_kv_pump::SEAL_AFTER_OWN_OPS = 2_000`, for KV, measurements and the work
plane alike (cw-lift 5d added the third arm; `work`'s snapshot is its live
leases and its offer, taken from the fold BEFORE the prune because the journal
is that plane's only copy of them), and
what it counts is this node's own ADMITTED ops at or above its authenticated
floor. The cost of that count is a fold, so a cheap one-sided gate runs first:
own RAW lines on disk (parsed, not verified) can never be fewer than
own-admitted-above-floor, so a cheap count under the bar proves the expensive
one is (§9.5). **The seal's honest cost, and how it was ABSORBED rather than
priced**: a snapshot carries LIVE rows and a tombstone is not one, so a delete
this node published would stop travelling the moment the seal that retired it
lands, and a peer that never received the tombstone would keep its stale value
forever — the KV shape of K7's "no history past the next seal". ea4da7b68
recorded that in the module docs and here; the rung that followed CLOSED it,
because a seal followed by its whole snapshot IS that actor's live set, and a
peer holding both may retire every other row of that actor's. `snapshot` now
ends with `rail_kv::snapshot_mark(floor)` — one act naming the seal it closes,
appended after the last row — so "I hold the whole snapshot" is a fact on the
journal rather than a guess about timing. The rule and its two gates are under
"the seal reconciliation" in the distributed-state section below. The threshold
stays at thousands of ops: it is priced on journal BYTES, and the cost it was
raised for is gone rather than smaller.

**`svrn ring` is the verb** (`sovereign-cli-llm/src/ring_cmd/`, ring-deploy
S4): `ring new` scaffolds an app (page, reducer, and the reducer's tests),
`ring roster add <person> --self` binds a name to the node key it signs with,
`ring dev <ns>` mints a `Scope::Rails` grant and serves the bundle at
`127.0.0.1:4318`, `ring seal <ns>` retires everything this node has written and
deletes it (over HTTP, not by opening the journal: a second process picking its
own next `seq` would race the daemon's writer lock and fork the actor), and
`ring log <ns>` prints the admitted acts and the gaps in
the terminal — gaps rendered as sentences a housemate can act on, never a
serde dump. There is deliberately **no `ring balances`**: a balance is an
expense app's reading of a journal, and a terminal that printed one for the
tenant that happens to be in front of us would be the money rules living in a
second place (§10.6). The app renders them, because it is the only thing that
knows what one is. The dev server holds the grant itself, so the
browser tab never sees a credential and the page reaches one namespace's rail
and nothing else on the daemon; the grant dies with the process.

**`svrn job` is the other verb on the same rail**
(`sovereign-cli-llm/src/job_cmd.rs`, cw-lift 5d): `ring` deploys an app to a
trust ring, `job` hands that ring a unit of *compute*. `job submit --kind
process:v1 -- <argv>` appends ONE act — a `Submit` naming the command, the git
rev it runs at, and which nodes may take it — through `ring_cmd::rail_append`,
the same append client `ring` uses, and then stops. It does not wait, poll or
place: a submitter that also chose a donor would be a second decider for a
lease (§10.6). `job status` reads `GET /v1/rail/log` and folds it with the SAME
`commonwealth_work::WorkProjection::fold` the daemon's donor loop runs, which
is what the package crate is for — the terminal, the daemon and a lifted
third-party peer are three readers of ONE function and cannot disagree about
who holds a lease. It goes over HTTP and never opens the journal for `ring
log`'s reason: the roster the DAEMON loaded is what decides which acts are
readable, so folding the file directly would be confidently and silently wrong
on exactly the ring where membership is the question.

Two details are the design. **The op table is two entries** — `log` and
`append` — where `meshapp dev`'s is twelve, because the rail is two routes and
an app's vocabulary is built out of those rather than by growing the rail.
**The roster is written by the CLI and is unreachable from the rail**: there is
no roster route at all, so a deployed app cannot add a key to the ring,
including its own. `roster add` then reads the roster back *through the running
daemon* and fails if the daemon does not report it — the one way that command
can look like it worked and do nothing is writing to a directory the daemon
does not read (§18.1).

**And since ring-apps ra-1, a roster row can say WHY it is there.** `Roster`
was a name and some keys, so the only answer to "why is Alex in this ring" was
whoever typed the command remembering. Two halves, landed together:

- `Introduce { person, key, reason }` (`commonwealth-rail-core/src/introduce.rs`)
  — an act a member signs under their own key, written by `svrn ring introduce
  <person> --key <hex> --reason <why> --ring <ns>`. **Deliberately not a
  `RailAct` variant**: a variant is a branch inside `admit`, which is the rail
  deciding what an act means. It rides as an ordinary opaque `Payload` on a
  `Record`, and admission carries it exactly the way it carries an expense.
- `Roster.vouches: BTreeMap<key, Vouch { op, by, at }>` — keyed by KEY, not by
  person, because two laptops can join on two evenings on two people's word.
  `#[serde(default, skip_serializing_if)]`: a `roster.json` written before this
  reads as warrant-unknown rather than failing to parse, and a roster with no
  vouches serializes to exactly the bytes it did before.

`trace(roster, admitted_ops, person, key) -> VouchStatus` is the one resolver.
It takes **admitted** ops, so "exists, verifies, signed by a member" is
discharged by `admit` rather than re-checked (§10.6), and the eight non-`Traced`
variants are the ways a well-formed-looking row still resolves to nothing: op
refused by admission, not an introduction, introduces another key, voided by a
correction, a key vouching for itself, an introducer whose own row is dated
*later* than the op they signed, and a row whose stored signer disagrees with
the op. `svrn ring roster show` (`list` is the same command) prints the rail's
own sentence under each key, and `roster add --on <op-id>` refuses a warrant
that does not resolve — the `Vouch` is minted from the signed op, so the
operator names an op id and nothing else.

**`Introduce` is evidence, never admission, and that is the whole cut.** An
introduction arriving from a peer moves no roster row:
`a_peers_introduction_arrives_readable_and_changes_no_roster_row`
(`commonwealth-rail/src/tests.rs`) asserts on the roster FILE BYTES after
ring-sync delivers one, and was watched failing with `ingest_all` taught to
fold it in. `svrn ring roster add` is still the only writer of a roster, the
roster is still not a function of the op set, and the reference app's
no-re-division test is unchanged.

**A daemon older than the CLI drops warrants silently** — serde ignores an
unknown field, so it reads `roster.json`, re-serializes it without `vouches`,
and every row reads warrant-unknown with the reason sitting on disk. `roster
show` compares against the file its own writer maintains and says so, exiting
non-zero, rather than rendering a confident wrong answer (§18.3). `ring dev` is a foreground server, so it is the one
declared capability no journey can drive; `cli-contract.toml` says so rather
than listing it uncovered.

Deliberately deferred with a named trigger: the namespace has **no retention
bound** (`gc_app` cannot serve — it compares `entry.timestamp`, which for a
write-once event is creation time, so a TTL deletes history and a partial
restore resurrects corrected expenses; and since cw-lift 4 it could not serve
anyway, because the store is a projection and a swept row returns on the next
fold unless the namespace declares a window in `commonwealth-state::retention`). Checkpoints land when one exchange
passes half the receiver's body limit — the sync route warns at exactly that
line, reusing the gauge `gossip.rs` already keeps on the mesh-store snapshot.

**Holding the dial string is not a credential** (`AcceptorRoutes::forward_for`,
`sovereign-mesh/src/iroh_access.rs`, 2026-08-27). An iroh endpoint accepts
anyone, and the dial string that reaches it is public — it rides in every
invite's `dial=` and is gossiped as `node_pubkey`. The acceptor forwards by
`TcpStream::connect`ing a loopback listener, so until this landed, routing on
ALPN alone handed every dial-string holder whatever the client listener grants
its own machine: the full client API, no bearer. What the QUIC handshake *does*
prove is the dialer's Ed25519 key, so the acceptor routes on `(ALPN, dialer)`:

| ALPN | member | stranger |
|---|---|---|
| `cwth/client/0` | the PEER listener (no bearer — peer federated inference carries none, and its key is the credential), which serves the client router **minus `/internal/*`**. Closed outright if that listener did not bind | the bearer-checking listener, i.e. what a LAN caller meets; `AUTH_EXEMPT_PATHS` still open. Closed outright if that listener did not bind |
| `cwth/rpc/0` | the local ggml rpc-server | REFUSED — it authenticates nothing, so there is no safe downgrade |
| `cwth/media/0` | the declared `[iroh] media_origin` (Jellyfin's `:8096`, or any HTTP server honouring `Range`); not advertised at all when none is declared | REFUSED — same reasoning as rpc: the origin authenticates nothing, and the dial string rides in every invite |
| `cwth/app/0` | one of SEVERAL named HTTP apps this node publishes, chosen by the FIRST PATH SEGMENT per request (`GET /chores/tasks` → the `chores` origin, forwarded as `GET /tasks`); its own allow-list (`[iroh] app_allow`), separate from media's; advertised only while something is published, and added to and removed from the live endpoint as that changes | REFUSED — an app written in an afternoon authenticates nothing |
| `cwth/offer/0` | the declared `[iroh] offer_origin` — any HTTP server listing what this operator has to sell or lend; its own allow-list (`[iroh] offer_allow`), a THIRD list separate from media's and apps'; not advertised at all when none is declared | REFUSED — the dial string is public and gossiped, so a downgrade here would publish an inventory of a household's possessions to anyone holding an invite |
| `cwth/guest/0` | — | admitted; the listener behind it reads the bearer |
| `cwth/http/0` | internal router, forwarded with the verified identity (`X-Mesh-Member` / `X-Mesh-Node` / `X-Mesh-Pubkey`, any client-supplied `x-mesh-*` stripped) rather than spliced, so an internal route reads WHO from the acceptor and not from a header the caller typed (2026-09-20) | internal router, DELIBERATELY: a joiner is not a member yet and `/internal/join` is how it becomes one. `gossip_authorized` and the join key guard the sensitive routes; the rest are a known open edge, and closing it needs a join-only listener for non-members. Named by its verified key only — the member headers are ABSENT, never a placeholder |
>>>>>>> origin

Federated media and named apps ride that surface: the holder declares an
origin, the viewer asks its own daemon for a loopback bridge URL, and the
acceptor tells the origin WHO is asking by rewriting request heads
(`X-Mesh-Member`/`-Node`/`-Pubkey`, every client-supplied `x-mesh-*` header
dropped first). Responses are a byte copy, which is why `Range` stays
byte-exact. `svrn mesh offers` enumerates the roster, so a neighbour
publishing nothing appears as a ROW carrying that refusal rather than absent.

<<<<<<< HEAD
### Scheduling and orchestration
=======
**Federated media rides that fifth slot end to end** (`TrafficClass::Media`,
`commonwealth/crates/commonwealth-media/src/reach.rs`, the route and daemon
glue in `sovereign-daemon/src/media_reach.rs`, 2026-09-11). The holder declares
`[iroh] media_origin = "127.0.0.1:8096"` — `svrn mesh media offer [<origin>]
[--admit <member>...]` writes it (and `media_allow`; with no origin it probes
127.0.0.1:8096 then 8920; `mesh media admit <member>...` narrows the stored
origin; `mesh media withdraw` removes all of it). Since 2026-09-19 `offer` also
creates the READ-ONLY account members reach the library as, reads its policy
back key by key, declares that account's token in place of the admin-equivalent
`/Auth/Keys` key, and records its id as `[iroh] media_viewer_user`
(`sovereign-cli-llm/src/mesh_media/viewer.rs`). It runs `daemon reload`,
which swaps the live `MediaRoute` (`sovereign-mesh/src/media_route.rs`) with no
restart; a value that does not parse refuses the boot or the reload. The viewer asks its own daemon — `GET /v1/mesh/media?peer=<name-or-id>`,
`svrn mesh media <peer>` — and gets back `http://127.0.0.1:<port>`: the
transport's cached bridge for `(peer, cwth/media/0)`, minted once and retargeted
in place when the peer's dial info moves, so a player can hold the URL. The
viewer-side bridge is `tokio::io::copy` both ways and never parses HTTP; the
holder-side media arm rewrites request heads to carry the caller's identity
(next entry) and copies everything else, which is why `Range` (a seek) passes
through byte-exact. `Media` is the one class with no
`[iroh.transport]` entry: it has exactly one transport (the IP overlay returns
no candidates for it — there is no port to guess and a guess would be the
library over plaintext), so there is nothing to opt it out to. What the read
refuses it names — unknown, ambiguous, offline, no identity, no origin, no
iroh path, non-loopback endpoint — rather than handing out a port that accepts
and never answers; the CLI then does one real `GET /` through the bridge so the person
sees an HTTP status, not a port. The response also says which KIND of number a
play would be: `path.relayed_reading` is true only for `relayed`, decided once
by `PeerPath::is_relayed_reading` — `mixed` (a direct leg and a relay both
live, bytes on the direct leg, which is every path on one LAN) is a direct
number under a relay's name and must not clear the bar. The bar itself is
measured through the SAME URL: `media_bridge_bench pull --url <it>
--duration-secs 600` judges the pre-registered rate / stall / duration and
leaves the path kind to that field. Watched failing:
`iroh_dialer_admission_e2e::a_stranger_holding_the_dial_string_cannot_read_the_library`
and `commonwealth-media reach::tests::an_offline_member_is_refused_by_name_not_handed_a_dead_port`.
These decisions live in the package crate so the inference daemon and the
package-only rails daemon compose ONE implementation of them (ARCH §10.6).
>>>>>>> origin

Eight decision points, each with one home:

| Decision | Where |
|---|---|
| Joiner decides a turn is offload-eligible | `sovereign-mesh/oicp_select.rs::offload_eligible` |
| Joiner picks peer-vs-local | `sovereign-serving-host/src/peer_inference.rs::select_peers_ranked` |
| Joiner resolves a *named* target | `peer_inference.rs::locate_named_model` — name resolution + min-in-flight, **not** the scorer. A HARD name is a constraint: unknown ⇒ error, never substitution |
| Hub picks a local model for a peer request | `sovereign-daemon/src/routes_inference.rs::route_with_oicp` |
| Serving peer picks Fast-vs-Slow slot | `oicp_select.rs::pick_slot_for_oicp` |
| Synthesis tier (Fast vs Primary) | `sovereign-core/runtime/evidence.rs::resolve_synthesis_route` |
| Distributed placement (model > one node) | `sovereign-inference/embedded/rpc_distribution.rs` |
| Collaborative ingest partitioning | `sovereign-grants/knowledge_assignment.rs` |

<<<<<<< HEAD
Slot policy is normative in [`docs/SLOT_POLICY.md`](./docs/SLOT_POLICY.md):
call sites declare a `slot_policy::Workload` requirement bundle rather than
free-handing `Speed::` literals. The composed OICP scoring product lives ONCE
in `oicp-types`.
=======
**What a member SERVES is gossiped beside how it is reached** (`NodeCapabilities::origins`,
`OriginKind`, 2026-09-11). `OriginKind` is defined in `oicp_types::origin` since the same
day (sv-surface svt-3) and re-exported at `commonwealth_core::capabilities::OriginKind`:
defined in commonwealth-core it pinned every wire shape carrying it — `MemberDto`,
`MeshMember` — above the contract layer, and `oicp-types` is the serde-only leaf both
families already depend on (the `TenantId` precedent). The acceptor knows whether it routes `cwth/media/0`
to a local origin (`MeshIrohAccess::media_route_active`); the dial-info provider
carries that as `IrohDialInfo::origins`, and the gossip self-stamp writes it
into this node's own capabilities each round, after the hardware/corpora
snapshot replaces them — so the advertisement is a fact about the LIVE acceptor,
not about config, and a declared origin whose endpoint never bound is not
offered. A round with NO live dial info republishes the triple this node already
held (`OfferView::restamp_onto`, rr-2) instead of the blank the snapshot left:
the offer is the holder's, only the dial addresses are the transport's, and
before that the endpoint rebuild after `svrn mesh media offer` withdrew an
accepted offer by omission for 22 rounds. `[iroh] media_allow` rides beside it the same way (`IrohDialInfo::media_allow`
→ `NodeCapabilities::media_allow`, empty when no media route is live or nothing is
narrowed) and reads back as `MediaOffer::offered_to`. **`NodeCapabilities::media_available`
rides the same stamp** (2026-09-19): what the holder's origin can serve right
now — `0.0` the holder is watching it, `1.0` free, `None` nobody answered, and
a `None` is never read as free. It is written by the holder's presence poll
(`sovereign-daemon/src/media_presence.rs`, every 10 s) through the SAME
`POST /internal/node/activity` the inference half uses, decided by
`commonwealth_media::presence` against the origin's sessions and the viewer
account in `[iroh] media_viewer_user`. The poll asks with the HOUSE credential
(`<data_dir>/secrets/media-house/`, `commonwealth_media::house_dir_under`) —
the install-stage credential `offer` spends and then replaces, kept on the
holder's machine and carried by no dial, because Jellyfin 12 shows a
read-only user only the sessions it may remote-control and so the DECLARED
viewer token reads the holder's own playback as "free" (2026-09-19); the gossip stamp drops it whenever this
node publishes no media origin, so a reading cannot outlive its offer.
`svrn mesh media withdraw` clears origin, allow list and viewer account
together and reloads, and the desktop Library rail's `mesh_media_offers` row
carries no `player_url` at `0.0` — it does not dial an in-use library at all. A closed enum, serde-defaulted and skipped when empty: a peer on an
older build reads as advertising none (absence, never an offer), and new→old
wire bytes are unchanged. Two reads consume it. `GET /v1/mesh/media` with no
`peer` — `svrn mesh media` bare — lists every active member other than self
whose origins carry `media`, with status and live path, dialing nothing
(`EmbeddedDaemon::media_offers`, `offering_members`); offline members are rows
with their status, because a person wants to know the library exists. And
`pick_member` refuses a named member that advertises none
(`MediaReachRefusal::NoOrigin`) instead of minting a bridge the far end will
close. `MemberDto::origins` carries the same fact on `/v1/mesh/status`. Watched
failing: `commonwealth-media reach::tests::a_member_that_advertises_no_media_origin_is_refused_by_name`.
>>>>>>> origin

Scheduler quality is instrumented rather than asserted.
`sovereign-scheduler/decision_log.rs` writes one `RoutingDecision` per
decision point — the whole candidate set, each `ScoreBreakdown`, each input
stamped with provenance and age, every excluded peer with its reason;
`decision_trace.rs` replays by `decision_id`, never adjacency;
`scheduler_core.rs` is the ranking as a pure total function;
`predicted_time.rs` is the only ranking in the tree with no tunable constant;
`decision_replay.rs` re-runs the LIVE scorer and policy over a capture to
check it reproduces its own verdict. The Tier-1 simulator is
`sovereign-mesh-test-harness`'s `mesh_sim` module behind the `dst` feature.
Findings are in `docs/specs/SCHEDULER_QUALITY.md`; **the standing one is F10 —
the scheduler has no speed signal in production**, so `throughput_factor` is
neutral 1.0 for every peer, and `svrn mesh bench` deliberately does NOT write
to `NodeCapabilities.benchmark` (`gossip_never_advertises_a_benchmark` fails
the build if it is populated).

**Byte-mass-aware split (`plan_shards_weighted`).** Each device gets a
CONTIGUOUS block range whose *bytes*, not block *count*, are proportional to
its VRAM. The big open-weight models are MoE and MoE mass is deeply
non-uniform — routed experts are ~88–93% of the bytes but cold, and a hybrid
SSM+MoE stack alternates a ~20 MB block with a ~1.3 GB one, a measured 62×
per-block spread — so count-proportional apportionment hands a small node a
heavy run and OOMs it. One function serves both the live load and
`svrn mesh plan`'s preview, so they cannot diverge. Per-device fit is
`shard_fits(plan, capacities, mass, headroom)`, where **`None` means
cannot-judge and is not a pass**: an unread tensor table would otherwise clear
every device on the strength of zeros.

`svrn mesh bench` measures the configuration that is loaded and never loads
the one it wants to measure — there is no slot argument, so there is no slot
to get wrong. A record carries the pre-image of its own key (`witness`) and
the conditions it met (`conditions`), travels on the ring rail rather than the
gossip KV store, and refuses to travel when invalid. Origin comes from the
SIGNATURE, never the payload.

Known gap: `commonwealth-inference/orchestrator/` was deleted as dead code,
taking `GracefulDeparture` and `FaultDetector` with it. `sovereign-compute`'s
supervisor has no departure countdown and no fault detector, so those two
parts of FE-139 are unimplemented rather than implemented elsewhere.

### The ring rail

An append-only, Ed25519-authored total order per namespace
(`commonwealth-rail-core` is the fold — zero I/O, zero clock;
`commonwealth-rail` is the journal). `Op.actor` is the signing public key, the
only field on the line a writer cannot forge for someone else.

**The rail carries an opaque `Payload`, and that cut is the design** — the
transport does not get to know what an act means. `Payload` is a type and not
a `serde_json::Value` for a specific reason: a signature covers bytes, and
which bytes a `Value` serializes to depends on `serde_json/preserve_order`, a
feature any crate added later can flip — which would make every signature in
every ring stop verifying at once, presenting as a journal gone entirely
`BadSignature`. So a `Payload` is canonical by construction (objects rebuilt
with sorted keys, recursively) and floats are refused outright, because
`1e2` / `100.0` / `100` are one value with three spellings.

`admit(ops, …)` is **a function of the op SET, not of arrival order** —
nineteen laptops gossip in nineteen orders. Dedupe by re-derived `OpId`, a
content-derived total order `(ts_unix, actor, seq, id)`, a void set built from
every correction at once that **never resurrects**, and sorted gaps, so two
nodes agree on the *report* and not merely the acts. `gaps` is the half that
refuses to fake completeness: a journal that cannot say "I may be missing
something" lets an app state a wrong total with full confidence.

**Correction lives in the rail on purpose** — "this earlier act was wrong, and
it never comes back" is not an expense rule, and it is the rule most easily
got wrong. **`Introduce` is evidence, never admission**: an introduction
arriving from a peer moves no roster row, and `svrn ring roster add` is still
the only writer of a roster.

Replication is its own loop at a 60-second cadence, syncing by digest
(`{actor → contiguous high-water mark}`, ~600 bytes regardless of history) —
*contiguous* is load-bearing. The run counts from a SEALED FLOOR, which is
what lets the rail delete: `RailAct::Seal` retires everything its author wrote
before it, carries **no actor and no range** (so sealing another's history is
unwritable rather than refused), and is authored rather than configured. One
body is capped at `MAX_REQUEST_BODY_BYTES`; convergence is not, because
`RING_SYNC_OPS_BUDGET_BYTES` chunks both directions and the exchange repeats
until neither side moves.

Verbs: `svrn ring` (new, roster add, dev, seal, log) and `svrn job` on the
same rail — `ring` deploys an app to a trust ring, `job` hands that ring a
unit of compute. Neither opens the journal directly: the roster the DAEMON
loaded decides which acts are readable.

### HTTP API

**Client API — :9741, binds 127.0.0.1 by default.** The wildcard bind is
reached only when something explicit asks for it. A non-loopback bind carries
a bearer token or serves nobody; when the token chain fails entirely the
posture installs NONE, so `client_auth` refuses every remote caller rather
than serving unauthenticated.

A non-loopback caller presents one of two bearers. `client_token` is
daemon-wide. An **ephemeral guest grant** is the narrow one: short-lived,
revocable, bound to a closed `Scope` enum whose `paths()` is the only route
allowlist there is. A guest is not a mesh member and cannot mint further
grants, because no `Scope` variant names `/internal/*`.

| Path | Notes |
|---|---|
| `POST /v1/chat/completions` | OpenAI-compatible; `LocalOnly` privacy → 400 |
| `POST /v1/responses` | OpenAI Responses-API adapter |
| `GET /v1/models` | Names this daemon can dispatch by name, built from the local OICP manifest + every reachable peer's — the same source `locate_named_model` resolves against, so a listed id resolves and an omitted one does not |
| `POST /v1/embeddings` | What peers call via `embed_http::http_embed_fn` |
| `POST /v1/knowledge/search` | Determines target corpora, fans out, merges, reranks |
| `/v1/apps*`, `/app/{app_id}/{*path}` | Mesh-app install/status + reverse proxy |
| `GET /status` | Node / mesh / inference / knowledge summary, incl. `process.pid` + `run_id` |
| `GET /oicp/v1/capabilities` | Provider manifest + federation info |
| `/api/{version,tags,ps,show,chat,generate,embed,embeddings}` | Ollama-native compatibility shim, pure translation over the OpenAI handlers |
| `POST /internal/ring/sync`, `/v1/rail/*` | The ring rail: anti-entropy, append, log, and the LIVE lane (delivery, not record — nothing reaches a store or a disk) |
| `/internal/guest/grant`, `…/revoke`, `…/list` | Mint / kill / list guest grants. On the Operator bind ONLY |
| `/v1/mesh/*`, `/v1/admin/*`, `/mcp/*` | Loopback-only |

**Which listener serves a route is the guard; "is the caller loopback" is
not.** The acceptor forwards by connecting `127.0.0.1`, so a loopback peer
address proves nothing. `ClientSurface` is the one decider and the client
router binds three times:

| Surface | Reached by | Trusts a loopback peer | Serves `/internal/*` |
|---|---|---|---|
| `Operator` | a real local caller on `:9741` | yes | yes |
| `Peer` | a MEMBER dialling `cwth/client/0` | yes | **no** |
<<<<<<< HEAD
| `Guest` | `cwth/guest/0`, a downgraded stranger, the guest door | no | no |
| `Rail` | a deployed ring app on `127.0.0.1:rail_port` (9743) | no (`UNTRUSTED_LOOPBACK`) | no — serves only `/v1/rail/*` |

The host mounts thirteen further client-router families on that router, each a
door over an object the daemon already holds, each `loopback_only` at the
router **and** `enforce_localhost` per handler: corpus status and atoms
(`reading_http.rs`), atlas and conv-tiered browse (`atlas_http.rs`), meshapp
projections (`meshapp_http.rs`), the enrichment store (`enrich_http.rs`), the
local-corpus registry (`lc_http.rs`), governance, insights, notes, features,
recipe projects, MCP config, turn extras, documents, the corpus catalogue,
recipe authoring and deep research. Parity is audited by
`sovereign-mesh/tests/loopback_parity.rs`.

**Internal API — :9742, plaintext under perimeter trust.** No per-request
auth: gossip, scheduling intent and plans, model transfer, RPC warm, index
shard push/pull/serve, inter-node knowledge search, latency probe. Binds
`0.0.0.0` by default — pin `[daemon] internal_bind`, or create the mesh with
`require_encryption`. **The historical per-session-cert mTLS scaffolding was
removed; never describe `:9742` as mTLS.**

**The loopback guard has three layers and one trap.** Router-level
`from_fn(loopback_only)` middleware, per-handler `ConnectInfo` extraction, and
a pinned listener-shape test. The listener MUST use
`.into_make_service_with_connect_info::<SocketAddr>()` — bare `axum::serve`
leaves `ConnectInfo` absent and the guards fail closed for *every* caller.

### Admission and fairness

Two disjoint layers, so a request meets exactly one and is never double-gated.
`peer_admission_layer` rations traffic that NAMES a node;
`client_fairness_layer` rations traffic that does not, returning early when
`X-Node-Id` is present. Both key the same
`serving_policy::fair_sched::SchedCore<Principal>`, which also backs the chat
server's turn scheduler, so every admission gate is fair by identical rules.
Local requests admit unconditionally; peer requests get 503 + `Retry-After`
when paused, yielding to recent local foreground work, or refused by the
scheduler.

`fair_share_cap(budget, active)` takes **no weight argument**, so the
weight-ordering condemned by SCHEDULER_QUALITY F6 is unexpressible rather than
merely avoided. The client gate's global slot budget is `usize::MAX` so it can
never refuse on depth — the inference slot queue's predicted-wait shed remains
THE shed decider — and it uses `try_grant`, leaving no waiter behind.

**One canonical wire form:** `X-Node-Id` is `NodeId::to_hex()`, exactly 32
lowercase hex chars. The `node-<16hex>` strings on `/status` rows are the
DISPLAY form and must never be echoed back as a header.

### Foreground yield is bounded

`YieldHook::should_yield()` is a LEVEL predicate with no memory of how long
the asker has been parked, so any request cadence shorter than the window pins
it true forever. Every consumer pairs it with `DeferralBudget`, and that
pairing is the invariant. `MAX_FOREGROUND_DEFERRAL` is 300 s and is
deliberately **not** an env flag or config field: a liveness bound someone must
remember to switch on is not a liveness bound. The write side is
`ForegroundSignal` + `ForegroundLease`, and every `Runtime` turn holds a lease
for the turn's whole life.

### Knowledge, ledgers, distributed state

`MeshCorpusManager` / `ShardManager` install, list, remove, shard and
consolidate. `merge_participants` is the ONE merge implementation, and it
finishes the job: a merge that stops at written chunks produces a corpus
`installed_indexes()` skips and gossip advertises nothing for, so
`finalize_canonical` is the last step of the merge itself. When the merge
lands and the finalize does not, the error is its own variant
(`MergedNotFinalized`) because the state is neither neighbour — the chunks are
on disk and the source partitions are gone, so that directory holds the only
copy.

**The contribution ledger has no balance, no exchange rate and no ranking** —
units are incommensurable. `LedgerEventKind` records inference served and
received, knowledge queries, shard transfers, storage and completed job units;
aggregation is pure. It gossips. Its siblings deliberately do not: the local
**Activity ledger** ("what is my daemon doing, even as a mesh of one?") and
**peer preferences** (per-peer affinity multipliers) are both in
`GOSSIP_EXCLUDED_APP_IDS` — your own usage never leaves the machine.

`commonwealth-state::MeshStore` is a SQLite KV that is a **local PROJECTION of
the ring rail, not a replica**. Writes insert a `rail_outbox` row in the same
transaction; the fold picks, per key, the act with the greatest
`(t, actor, id)` among admitted non-voided ops, where `t` is the ORIGINAL
write time, so a snapshot re-append after a seal does not hand every key to
whoever snapshotted last. Acts this build cannot read are COUNTED, never
dropped silently. Retention is part of the fold, because on a projection
nothing else can be: a row a local sweep deletes has no incumbent and returns
on the next round. `MeshStore` is `in_memory()` in production, so the pump's
first act at boot is to rebuild it from the journals or hold nothing at all.
Which namespaces replicate is DECLARED in `DAEMON_OWN_NAMESPACES`; no property
of a namespace string separates `inference` from `house-expenses`, so a rule
would silently admit every member as an author of an app's journal.

### Desktop and deployment

Desktop production-readiness (W1–W6) is in the detail file. The current shape:
the desktop **manages no daemon** (`serving_host::ensure_reachable` brings up
a bundled `sovereign-cli-daemon` sidecar when nothing answers, behind the
`bundled-backend` feature declared by exactly one surface), commissions no
`Runtime`, loads no GGUF, and reads the turn socket once per conversation. The
deletion is structural rather than conventional — those crates left
`sovereign-desktop/src-tauri/Cargo.toml`, so the ability is gone, not merely
unused. W6 is the self-service support surface: seven health checks, a
redacted diagnostic bundle, and a per-answer report, all files on the Desktop
the user reads before sending, never auto-uploaded.

A **mesh app** runs in a `meshapp-<id>` webview reached only through a
permission-gated bridge; the app id comes from the host-set webview LABEL
(unspoofable from JS) and is checked fail-closed. Graph ops live in the
`sovereign-meshapp` library so the desktop host and `svrn meshapp dev` share
one source of truth, and `load_graph` dispatches on what the index holds.
**Isolation caveat:** Tauri v2 does not gate commands per-window
(tauri#9227), so true isolation for UNTRUSTED third-party apps needs a no-IPC
bridge — a deferred milestone.

`commonwealth-rails` (`cw-rails`) is the minimal daemon a shim author installs
beside their media server: join an invite, run, serve three loopback routes.
It deliberately does NOT admit joiners — a mesh is founded by a full daemon,
and that absence is most of why it lifts (319 crates in its closure vs 743).
=======
| `Guest` | `cwth/guest/0`, a downgraded stranger, and the guest door on `[daemon] guest_bind` (open only while a rail grant is live; also serves `/v1/rail/*`, the ring page (one bundle per rail namespace from the `[daemon.guest_pages]` registry at `/ring/<namespace>/`, whose shim names that namespace on every `/v1/rail/*` call — a wall grant names none by design, so the page is what says which app it is, with `[daemon] guest_page_dir` still putting a single app at the bare `/ring/` and the wall's INDEX served there when it is unset) — that registry is also the DECLARATION of which namespaces admit guests, so a `Scope::Wall` grant (`svrn mesh grant --wall`, ONE QR for the wall) reaches every namespace declared there and nothing else on the rail, `--rail <ns>` narrows to one, an entry may declare `guests = "read"`, and a namespace the daemon owns (`ring_roster::is_daemon_owned`) is refused at config load AND at the route, and `POST /v1/guest/ask` — the door running a grounded turn as its own principal in one conversation per bearer, returning `{answer, epistemic_state}` only, `sovereign-daemon/src/guest_door.rs` + `routes_guest_ask.rs`; and `POST /v1/guest/session`, where a phone claims the NAME it is shown under — one QR serves a room, so the grant says what may be reached and a door-issued session says who; the session belongs to the DOOR and is recognised under any live grant it minted, so a person walking between this wall's apps is named once — `[daemon] guest_sessions = "grant"` is the strict setting that binds it to one link instead, `routes_guest_session.rs` + `sovereign-grants/src/guest_session.rs`) | no | no |
| `Rail` | a deployed ring app, on `127.0.0.1:rail_port(client_port)` (9743 by default) | no (`UNTRUSTED_LOOPBACK`) | no — and it serves NOTHING but `/v1/rail/*` |

The `Rail` bind is the only one of the three that is a real TCP listener on a
FIXED port rather than an ephemeral loopback socket the acceptor forwards to —
because the thing that dials it is a separate process (`svrn ring dev`) with
only the config to go on. `commonwealth_core::config::rail_port` is the one
derivation, so the daemon that binds it and the CLI that dials it cannot
disagree on a non-default client port. **`UNTRUSTED_LOOPBACK` is the whole
reason it is a separate bind**: `:9741` admits a loopback caller *before*
reading a bearer, so a ring app pointed there would arrive as an operator with
its grant ignored — namespace scoping would be decorative, and a guard nobody
can watch fail is not a guard (§18.1). Watched failing:
`rail_e2e::on_the_rail_bind_a_loopback_caller_without_a_grant_is_refused`.

No address in `AcceptorRoutes` points at the operator listener at all, so the
acceptor cannot reach that surface however it is called. Watched failing:
`iroh_dialer_admission_e2e::routing_a_member_at_the_operator_listener_is_the_hole_this_closes`
wires the member arm the old way and gets a 200 on `/internal/guest/grant/list`.

**A guest reaches an encrypted mesh over its own ALPN, not the peers'.**
An encrypted mesh binds the client API loopback-only, so the link a guest
holds names an iroh dial string (`dial=`) instead of an address, and `svrn
mesh use` / `svrn chat` tunnel to it. The acceptor routes that traffic on
`cwth/guest/0` to the `ClientSurface::Guest` bind of the client router, which
carries `ClientAuthPolicy::UNTRUSTED_LOOPBACK` — because the acceptor forwards by
connecting `127.0.0.1`, and the default policy admits a loopback peer
before reading a bearer, which would hand every holder of the node's
public dial string the whole client API. Peers keep `cwth/client/0` and a
listener that admits without a bearer: their federated inference carries
no `Authorization` header at all, so routing them together would have
broken one to fix the other. That is the `Peer` bind, not the operator's
own — see the `ClientSurface` table above. Neither the guest nor the peer
listener serves `/internal/*`, MCP, or any mounted host surface. There is no plaintext fallback: a link
carrying `dial=` is tunnelled or it is refused (§18.3).

| Path                          | Notes                                                  |
|-------------------------------|--------------------------------------------------------|
| `POST /v1/chat/completions`   | OpenAI-compatible. Routing differs by daemon shape (embedded vs standalone) — see `commonwealth/docs/routing-field-guide.md`. `LocalOnly` privacy → 400. |
| `POST /v1/responses`          | OpenAI Responses-API adapter (codex 0.130+). Wire-format translator over chat-completions. See [`docs/inference.md`](./docs/inference.md). |
| `GET  /v1/models`             | **Names this daemon can dispatch by name**, one row per name. Built from the local OICP manifest + every reachable peer's — the same source `locate_named_model` resolves against, so a listed id resolves and an omitted one does not. Carries `residency` (`resident`/`cold` — cold is a lazy slot, not an outage) and `advertised_by` (which nodes hold it). Falls back to the gossiped `inference_store` scan ONLY on the orchestrator daemon, which has no manifest; that path can say "the entry's last writer is reachable" and nothing stronger. Before 2026-08-27 the store scan was the ONLY path, and it advertised ids chat completions refused. |
| `POST /v1/embeddings`         | Embedding endpoint (what `embed_http::http_embed_fn` peers call) |
| `POST /v1/knowledge/search`   | Determines target corpora, fans out, merges, reranks   |
| `/v1/apps*`, `/app/{app_id}/{*path}` | Mesh-app install/status + reverse proxy (`commonwealth-app`) |
| `GET  /status`                | Node / mesh / inference / knowledge summary            |
| `GET  /oicp/v1/capabilities`  | Provider manifest + federation info                    |
| `/api/{version,tags,ps,show,chat,generate,embed,embeddings}` | **Ollama-native compatibility shim** (`routes_ollama.rs`). Pure translation over the OpenAI handlers above — lets Ollama-native clients (Open WebUI's Ollama mode, IDE plugins) connect. `chat`/`generate` are non-streaming-backed in v1: the inner handler runs `stream:false` and the complete answer is framed as Ollama NDJSON (one content frame + terminal). No CORS layer + the same auth posture as `/v1/*` (documented in-module); incremental streaming is a tracked follow-up. |
| `POST /internal/ring/sync` | Ring-ledger anti-entropy for one namespace: the caller sends its per-actor contiguous high-water digest (and optionally ops), the responder ingests those and answers with its own digest plus as much of what the caller lacks as fits `RING_SYNC_OPS_BUDGET_BYTES`. Both `ops` arrays are budgeted and the sender repeats the exchange, so one body is not the unit of convergence. Own route on its own 60s cadence; `/internal/app/state`'s 10s full-snapshot push was the alternative and was deleted at cw-lift 2e, leaving this the ONE receiver and its loop the ONE sender of replicated state. |
| `POST /v1/rail/append`, `GET /v1/rail/log` | The ring rail. Appends one signed act to the caller's namespace, and reads back the admitted acts (already in the one order every node applies them) + gaps. The payload is the app's and the rail reads no field of it: whose words an act is comes from the guest session the door authenticated, is signed with the act (`SignedOp::on_behalf_of`), and a name a caller puts on the wire is dropped. `GET /v1/rail/log` finishes that attribution once — a guest act's `person` reads `<name>, guest of <member>` with a structured `guest` beside it — so the wall's page, a scaffolded app and `svrn ring log` all render the same name without composing it. There is no balance here to return. The namespace comes from `Scope::Rails` on the grant, never from the request; an operator (no grant) passes `?namespace=`. Mounted on `Operator` and `Rail`, and on neither `Peer` nor `Guest` — a ring rail is loopback-only in M0. A successful append raises `AppState::ring_write_nudge` (the third raiser, beside the KV pump and the work atlas's broadcaster), so a peer holds the act in about a second instead of at `ring_sync`'s sixty-second tick; the route still talks to no peer itself. |
| `POST /v1/rail/live`, `GET /v1/rail/live`, `POST /internal/ring/live` | The ring rail's LIVE lane — delivery, not record (`routes_rail_live.rs`, `routes_internal/ring_live.rs`). POST fans one payload (≤ `LIVE_PAYLOAD_MAX_BYTES` = 4096, refused with 413 at both ends, never truncated) out to every online peer's `/internal/ring/live` with `pipeline_pause.rs::forward_to_peers`' fan-out, in a `{namespace, payload}` envelope whose namespace is the grant's (`routes_rail::namespace_for`, as append and log); the receiver refuses a namespace no live rail grant on it names and otherwise puts the payload in that namespace's bounded 256-entry in-memory buffer on `AppState` (`rail_live_buffer`, one accessor), and GET drains only the caller's namespace, reporting `dropped` for anything evicted. Nothing reaches a store, a journal or a disk, so `/internal/ring/live` gets NO `REPLICATION_SENDERS` row — that census's subject is replicated state, and it staying green is this lane's positive control while `sovereign-mesh/tests/main/ring_live_non_durable.rs` is the negative one. The lane exists because y-protocols awareness `outdatedTimeout` is 30 s and `ring_sync`'s cadence is 60 s: a cursor carried by the journal would have faded before it arrived. The payload is opaque TEXT and neither route looks inside it. |
 | `GET /internal/ring/checkpoint/{ns}` | One ring's record, frozen: the v1 checkpoint document (`docs/THE_LINK.md` §"The checkpoint, specified") — `ops` are the journal lines verbatim, `digest` is `commonwealth_rail::digest` over them (the completeness claim, recomputable by any verifier), `roster` is the struct the append path admits under, `created_unix` is now. Read through the append path's own pair (`routes_rail.rs::journal_for`'s shape: `rail.roster(&journal)` + ONE `journal.read()`); a namespace the node does not hold is refused before touch (the journal creates on first open) and a read error is a 500, never an empty document. `svrn ring checkpoint <ns> [--out <file>]` is its CLI (`sovereign_cli_shared::rail::rail_checkpoint`, the one client that targets the internal listener), and `svrn ring checkpoint --verify <file> [--roster <file>]` runs the four steps cold over a frozen copy — re-parse, `admit`, digest equality, render — refusing inauthentic gaps (bad signature, unknown signer, tampered id, fork) by name while naming incompleteness gaps (holes) on screen; `ring_cmd/checkpoint_verify.rs`. A READ of record, not replicated state: a peer syncs by digest, so this serves the loopback callers `internal_gate` admits (`routes_internal/ring_checkpoint.rs`). |
 | `/internal/guest/grant`, `…/revoke`, `…/list` | Mint / kill / list ephemeral guest grants. On the `ClientSurface::Operator` bind ONLY: `:9742` has no auth gate, so a mint route there would let any mesh peer forge guest credentials — and the peer/guest binds of the client router 404 it for the same reason. Unreachable by a guest because no `Scope` names it either. |
| `/v1/mesh/*` `/v1/admin/*` `/mcp/*` | **Loopback-only** (router middleware + per-handler `enforce_localhost`) |

**What the host mounts on that router** (`sovereign-mesh`, sv-surface rungs
D2-D8, 2026-09-09/10). The desktop's peripheral commands used to answer from a
private in-process stack, so an ATTACHED app answered from its own objects
while the daemon's went unread — the same question answering differently
depending on which surface asked. Each family below is a door over an object
the daemon already holds, mounted beside `reading_http` with one posture:
router-level `from_fn(loopback_only)` **and** per-handler `enforce_localhost`,
audited by `sovereign-mesh/tests/loopback_parity.rs` (including the PUT leg
above, which no handler can satisfy alone). None of them is served on the peer
or guest bind.

| Prefix | What it serves | File |
|---|---|---|
| `GET /internal/corpus/status`, `GET /internal/corpus/{corpus}/atoms` | the one corpus-status decider, and `understanding_vocab::AtomsFile`'s `Vec<AtomEnvelope>` verbatim — paged because the installed wikipedia atlas is 846 MB (default 200, hard cap 2,000, an over-large limit served clamped and reported, an explicit `null` `next_offset` at the end, a missing atlas a 404 with a reason) | `reading_http.rs` |
| `/internal/atlas/{corpus}/…` | `sovereign_tools::atlas_view` types off a `FileAtlasReader` over the daemon's `index_dir`: corpora, report, members, atoms (a POST — the filter carries a `Vec`), subgraph, atom detail. The section→chunk cache policy moved here with them | `atlas_http.rs` |
| `/internal/atlas/conv/…` | the conversation-tiered browse over `runtime.lane_sources.conv_tiered` (`ConvBrowseReader`, `sovereign-core/src/conv_tiered.rs`): corpora, conversations, detail, entities, aggregate, chunk-entity progress. Absence has three answers — 503 no reader, 501 `NotImplemented`, 404 unknown conversation — never an empty list | `atlas_http.rs` |
| `/internal/meshapp/{corpus}/…` | the thirteen `sovereign-meshapp` explorer projections (graph, node detail, findings, entities, claims, questions, reconciliation, subgraph, stats, timeline, chunk, documents, wrapped), with the page clamps that used to live in the desktop, plus the three SF-LVT parcel reads (`parcels?ids=`, `parcels/search?q=`, `parcel-analytics`) whose folds moved from `commands/meshapp.rs` to `sovereign_meshapp::parcels` on 2026-09-11 — the desktop now gates and calls instead of pulling every atom of a 208k-parcel atlas to filter three; DTOs in `daemon_wire::meshapp`. 404-vs-500 reads a closed `MeshAppError`, not a phrase table | `meshapp_http.rs` |
| `GET /internal/corpus/enriched`, `GET /internal/corpus/{corpus}/starter-questions` | the enrichment-store reads (thin-desktop order, 2026-09-11): the enriched-corpus inventory over the DAEMON's `<data_dir>/enrichment` (`sovereign_enrichment_catalog::list_enriched_corpora_in`) and the starter questions `corpus_engine::enrichment::atlas::analysis::starter_questions` mines from a corpus's atlas — both folds the desktop ran over its OWN data root / every atom pulled over the wire until then. No atlas is a 404 naming the corpus, which the desktop turns into its excerpt-starter branch. DTOs in `daemon_wire::enrich` | `enrich_http.rs` |
| `/internal/corpus/local/…` | the daemon's OWN local-corpus registry (`watched_folder_runtime::manager()`): list, remove, incomplete jobs, cancel, git check, tags, snapshots + rollback, clean, preview, search, ocr-available, and an ingest job answering 202 + `{job_id, progress_route}`. Since 2026-09-11 also a CLUSTER job: `POST …/{c}/cluster` (202, same ack shape) + `GET …/{c}/cluster/progress?after=N` serving the manager's `LocalCorpusProgress` frames verbatim from an in-process log (`ClusterProgress`); the desktop's `lc_cluster` re-emits them, and `preview`/`write-tags` now read the cache that job filled. Same day: `POST …/pre-scan` (`PreScanRequest` path + source_type → `PreScanAnswer`) registers the user-picked path on the daemon's manager (Obsidian arm with the daemon's snapshot root) and scans the config the registry kept — the desktop holds no local-corpus manager read at all now | `lc_http.rs` |
| `GET /internal/corpus/local/{corpus}/ingest/progress` | `IngestProgress` over an `IngestOutcome` file written by ONE writer (`record_ingest_outcome`, from both ingest sites) carrying `IngestStats` verbatim, kept apart from the phase file so an ingest never stamps Complete on the map. `finished` with neither stats nor error is an error, never a zero-count success | `lc_http.rs` |
| `/internal/governance/{corpus}/…` | `GovernanceView` plus the tension verbs (resolve / accept / dismiss / undo), seed, post-build seed and recipe render, over `index_dir/{corpus}/atlas`. A missing atlas is a 404 naming the path — the in-process read answered `Ok` + empty, so an unenriched corpus rendered "no conflicts" | `governance_http.rs` |
| `/v1/insights…` | clip / list / search / delete, `POST /v1/insights/by-id` (`{insights, missing}` — dead ids are named, not silently dropped) and `GET /v1/insights/sinks`, which is the REAL sink registry where the desktop hard-coded an empty vec | `insight_http.rs` |
| `/v1/notes…` | the daemon's `NoteStore`: create, query (a POST, because the filter carries three `Vec`s and `serde_urlencoded` cannot take a sequence), get, delete, set payload, retire | `notes_http.rs` |
| `/v1/features/projects…` | the feature-project store's whole surface, over a `ServingCore.features` slot | `features_http.rs` |
| `/v1/recipe-projects…` | `RecipeProject` composed daemon-side over its stores and data root — list/create, dashboard, TOML write (atomic, under the data root rather than a user path), link-recent-artifact, checkpoint restore, prelude | `recipe_project_http.rs` |
| `/v1/mcp/servers…` | the daemon's own MCP config and mount: list, test, token PUT/DELETE. `connected`/`error` are always `None` with `mount.reason` stating the absence on every response — the runtime recipe drops the `McpServerManager` after boot, so this host has no source, and `connected = count > 0` was the fabrication refused | `mcp_config_http.rs` |
| `GET /v1/skills`, `GET /v1/conversations/{id}/provenance` | the runtime's skills (`trust_level` lowercased HERE — the desktop held the only copy) and `Runtime::get_last_turn_provenance` (a null provenance is a 200, not a 404) | `turn_extras_http.rs` |
| `/v1/documents`, `/v1/documents/legacy` | the document assets and the legacy-document listing + promotion over `ServingCore.state_store` + runtime (D9a; the desktop's nine document reads; ask-document's fall-through turn stays on the driver). Since 2026-09-11 the UPLOAD is a job here too: `POST /v1/documents` (`{path}` → 202 Pending record) + `GET /v1/documents/{id}/progress?after=N` (`DocumentIngestProgress`: the manager's `IngestProgress` frames with `asset_id` stamped, from an in-process log); and `POST /v1/documents/legacy` (`{path}` → `{source, chunks_created}`) is the old paperclip ingest. The desktop's `upload_document_asset` / `ingest_document` are call + poll. The ASK is a job as well: `POST /v1/documents/{id}/ask` (`{question, conversation_id}`; persists the user message, then route + execute + persist on the daemon's manager) + `GET /v1/documents/{id}/ask/{job_id}?after=N` (`AskProgress`: `OperationProgress` frames + a terminal `AskOutcome` — answered with the persisted message, fell_through for off-topic/empty-RAG which the client runs as an ordinary turn, or failed). `ask_document` on the desktop holds no manager. Since 2026-09-11 `manager_for` also hands the manager `runtime.lane().gliner`, so the T2 skeleton entity pass runs on the daemon's resident NER model rather than its LLM fallback — the module's own header asserted "a daemon holds none", which was never true: `sovereign-runtime-recipe` fills `LaneSources::gliner` for every host it commissions | `sovereign-daemon/src/documents_http.rs` | router `loopback_only` + per-handler `enforce_localhost` |
| `POST /internal/corpus/recipes/import`, `GET /internal/corpus/recipes/{corpus}/parameters` | the recipe-authoring writes and reads (thin-desktop order, 2026-09-11): validate a pasted recipe offline (`test_recipe`, sample size 0, staged beside the engine's own recipes dir) and install it through `RecipeRegistry::install_local_recipe` — the ONE decider for "a user published a recipe", which `svrn recipe publish` now calls as well; and the `[parameters]` block for the install form, resolved by `fetch_recipe` so a just-imported recipe answers with no reload. The desktop held a third copy of the install loop over a `CorpusEngine` of its own, resolving THIS process's default recipes dir rather than the daemon's. A validation failure is a 200 with `success: false` and the errors; a body that is not a recipe is a 400. DTOs in `daemon_wire::recipes` | `recipe_http.rs` | router `loopback_only` + per-handler `enforce_localhost` |
| `POST /internal/corpus/recipes/test`, `GET /internal/corpus/recipes/test/{job}/progress`, `POST /internal/corpus/recipes/harness`, `GET /internal/corpus/recipes/harness/{job}/progress` | the recipe-authoring RUNS (sv-surface svt-6, 2026-09-12) — the last thing the desktop needed a `CorpusEngine` for. `…/test` is the dry run: `sample_size == 0` is validation-only (static checks plus, when `offline` is false, one HTTP HEAD on the source URL) and answers `RecipeDryRunReport` INLINE; a sample ACQUIRES, so it is a job — 202 + `IngestJobAck`, 409 by recipe id. Both arms project through one `dry_run_report`, so a sampled run and a validation run cannot disagree about a field. `…/harness` is the deterministic authoring harness over a frozen sample, always a job because the first run captures; the sample lands under the DAEMON's `<data_dir>/harness/<id>`, and rung 6 (`enrich: true`) is `verify_atoms_at(<index_dir>/<id>)` — the corpus this daemon installed, in the index it serves retrieval from. The drive is `sovereign_authoring_harness::run_over_frozen_sample`, the SAME function `svrn recipe test` calls; what differs is rung 6, which is a parameter rather than a flag inside the drive. **The trailing `/progress` is load-bearing**: spelled `…/recipes/test/{job}`, axum prefers the static `test` over `{corpus}` and a recipe named `test` loses its `…/parameters` form — caught by `the_dry_run_progress_route_does_not_shadow_the_parameters_route`, which went red on exactly that. DTOs in `daemon_wire::recipes` (`RecipeDryRun*`, `RecipeHarness*`, `RecipeJobState`, `HarnessRunCardView`); client `TurnClient::{recipe_dry_run, recipe_dry_run_progress, recipe_harness, recipe_harness_progress}` | `recipe_http.rs` | router `loopback_only` + per-handler `enforce_localhost` |
| `GET /v1/admin/hardware`, `GET /v1/admin/setup/catalog?profile=`, `GET /v1/admin/setup/slot?kind=`, `GET /internal/ner/model`, `POST /v1/admin/assets/download` (202), `GET /v1/admin/assets/download/{job}` | the daemon's WEIGHTS (sv-surface svt-7, 2026-09-12). `<data.dir>/models` belongs to the process serving from it, and until this landed a client probed that directory with its own filesystem calls, resolved the catalog with its own copy of `setup_planner`, and wrote into the root with a THIRD GGUF downloader. The four reads answer what this machine can run, what the tier's catalog offers, its single-pick fast/embed slots, and whether the GLiNER export is installed **under the id the daemon is configured for** — `configured_model_id()`, not the `DEFAULT_MODEL_ID` constant a client would repeat. The write is ONE job for both artifact kinds: `{kind: gguf|gliner}` over `setup_planner::download_gguf` into `<data.dir>/models/` or `gliner_ner::download_model` into the GLiNER root, answering `IngestJobAck` + a progress route in the IndexBuild pattern. Three refusals rather than guesses: an unknown `profile` or slot `kind` is a 400 naming the set; a `gguf` request names which of `url`/`file` is missing; and a `file` carrying a path separator or `..` is refused by SHAPE, because a client naming a destination outside the models root is the one thing this route must not honour. `AssetDownloadProgress.path` is populated only on `Complete` — the client writes that string into a model slot, so a path reported mid-download would configure a `.part`. The same four reads are ALSO what `svrn setup --plan --json` prints, in the same `daemon_wire` types, because on a first run there is no daemon to ask. DTOs in `daemon_wire::assets` + `daemon_wire::setup_plan`; client `TurnClient::{admin_hardware, setup_catalog, setup_slot, ner_model, asset_download, asset_download_progress}` | `assets_http.rs` | router `loopback_only` + per-handler `enforce_localhost` |
| `/internal/corpus/catalog`, `/notebooks`, `/diagnose`, `/{corpus}/health`, `/{corpus}/coverage-card`, `/{corpus}/retry-enrichment` | the corpus catalogue and the notebook shelf's five-source fold over `installed_indexes()` (the one decider), the atlas readers and conv-tiered buckets; in-flight state stays on `/internal/corpus/status` (D9a; nine desktop reads in corpus.rs, budget.rs, corpus_install.rs, recipe_testing.rs) | `sovereign-daemon/src/corpus_catalog_http.rs` | router `loopback_only` + per-handler `enforce_localhost` |
| `POST /v1/research` (202), `GET /v1/research/{job}/progress?after=N`, `POST /v1/research/{job}/abort`, `GET /v1/research/capabilities`, `GET /v1/research/runs`, `GET /v1/research/active`, `GET /v1/research/runs/{run}/report` | deep research as a daemon JOB (2026-09-11). Until then `sovereign_core::deep_research::run` was linked into BOTH the desktop's `dr_start` and the CLI verb and served by no route. `POST` launches through `launch::prepare` (the ONE `RunConfig` assembly), one run at a time (a second is a 409 naming the first); the job's frame log (`ResearchFrame`: `started`, every CHANGED `live` run-dir snapshot, one terminal `report_ready`/`failed`) is cursored by `after`, and the answer carries the elapsed/quiet clocks a client's `heartbeat` is made of. The run-dir readers (poller, shelf, report + constitution check) came down from the desktop whole and still read the loop's ICD artifacts as the single state source. The loop's web queries are machine-formed (`port.rs` passes `user_formed: false` at both `egress::verify` sites), so the daemon can host it without weakening the egress boundary. `ResearchLauncher` is the seam the e2e test stubs (`tests/main/research_surface_e2e.rs` pins the job contract; the loop has its own tests in `sovereign-core`) | `sovereign-daemon/src/research_http.rs`; DTOs in `sovereign-contracts/src/daemon_wire/research.rs`; client `TurnClient::research_*` | router `localhost_only` (no daemon handle — `launch::prepare` reads `SetupConfig` itself) |

What could NOT cross is named at the site rather than papered over: a
user-picked path (governance export, `lc_validate_path`, `lc_pre_scan`), an
emitter-only output (`lc_cluster`), a config WRITE (`SetupConfig::save` has zero
call sites in `sovereign-mesh` — every writer is CLI-side, so a
`POST /v1/admin/config` comes first), and workflow-kind validation
(`Workflow::parse` is a studio dep the layer map forbids here, so validation is
`Option` with `validation_unavailable` stated and a `PUT …/toml` on a workflow
project is a 501 rather than an unvalidated write).

**Internal API — :9742, plaintext, MEMBER-gated since 2026-09-21**

The internal routes (gossip, scheduling, model/index transfer, knowledge
fan-out) trusted the network boundary until 2026-09-21. They no longer do:
`sovereign-daemon/src/internal_gate.rs` runs in front of every route but
`/internal/join` and `/internal/gossip` — both of which carry their own
credential check and are how a caller BECOMES something the gate can admit —
and answers 401 unless the caller is a verified `Principal::Member`, carries a
`ProvedMeshMember` marker (a holder of this mesh's secret on a plain-IP hop),
or is a process on this machine. The reach is therefore "any holder of the mesh
secret, plus anything local", not "any peer that can route here"; on a
plaintext mesh nothing can tell one member from another, so one leaked secret
is still every member. `[daemon] internal_auth = "perimeter"` restores the old
posture wholesale and logs one warning at startup saying so; the default is
`"member"` and an unknown spelling refuses to start. Every first-party
non-peer caller (the desktop's one `internal_base_url()` accessor, the CLI's
`/internal/*` reaches, the demo scripts' own nodes) arrives over loopback and
is unaffected; peer callers carry the stamp (`AppState::stamped`, and the
header pair for the two crates that cannot name the minting type).

It also carries a PRINCIPAL. Since 2026-09-20, resolved once at the edge by
`sovereign-daemon/src/internal_principal.rs` and attached as
`AttachedPrincipal`, exactly as `client_auth_layer` does on `:9741`: a hop
this daemon can tie to its own iroh acceptor (loopback peer + the per-process
acceptor mark) resolves to the `Member` its verified Ed25519 key names, and
every other connection has its `x-mesh-*` STRIPPED and resolves
`Principal::Unverified` (or `Anonymous`, when it claimed nothing). Since
2026-09-21 an untied caller may also offer `x-mesh-proof`
(`<sender-hex>.<proof>`, minted by
`commonwealth-transport/src/mesh_proof.rs`): a value this mesh's secret
accepts attaches a `ProvedMeshMember` marker BESIDE the principal and changes
the principal not at all, because any holder of the secret can mint a proof
naming any sender — it proves the GROUP, never which member. A present proof
that fails is `Unverified`. The resolver itself refuses nothing — that is
`internal_gate`'s job, above — and `routes_internal::ring_sync` additionally
refuses an unmarked non-loopback asker BY NAME rather than depending on the
layer, which narrows §"Known gaps" entry 8 from "every peer that can route to
the host" to "every holder of the mesh secret". A MARKED asker is still served
without a roster check: refusing it would stop every file-rostered ring
replicating on a plaintext mesh, and that is the operator's call. Binds `0.0.0.0`
by default — set `[daemon] internal_bind` to pin it to a private interface,
or create the mesh with `require_encryption` to force all traffic onto the
iroh QUIC transport (which binds the internal router loopback-only). The
historical per-session-cert/`TrustStore` mTLS scaffolding was removed
2026-06-15 (see §5 "TLS / mesh encryption"); never describe `:9742` as mTLS.

| Path                                | Purpose                          |
|-------------------------------------|----------------------------------|
| `POST /internal/gossip`             | Gossip exchange                  |
| `POST /internal/scheduling/intent`  | Scheduling decision notification |
| `POST /internal/scheduling/plan`    | New shard plan distribution      |
| `POST /internal/model/transfer`     | Model file transfer (peer-to-peer) |
| `POST /internal/rpc-warm`           | Distributed inference: host asks a worker to seed its RPC tensor-cache shard before a distributed load (auto-warm). `serve_model_file` honors `Range` for shard-only fetch. The host distributes only to ELIGIBLE workers (`sovereign_serving_host::worker_eligibility` — settle + flap-quarantine, surfaced in `svrn mesh status`); a remote crash mid-compute `GGML_ABORT`s the host, so distributed inference requires host supervision. See `docs/RPC_DISTRIBUTED_INFERENCE.md`. |
| `POST /internal/index/transfer`     | Corpus shard upload (push)       |
| `GET  /internal/index/serve`        | Corpus shard download (pull)     |
| `POST /internal/knowledge/search`   | Inter-node shard query (fan-out target) |
| `GET  /internal/latency/probe`      | Latency probe response           |

The table shows the core mesh-protocol routes. `server.rs`'s
`internal_router` registers ~30 more operational routes (corpus
lifecycle `/internal/corpus/*`, model load/unload/inventory, app state
+ registry, budget / quiesce / foreground-state controls, and the
contribution/activity family listed under §5 "Desktop
production-readiness").

The loopback guard is defended in three layers: router-level
`from_fn(loopback_only)` middleware, per-handler `ConnectInfo`
extraction, and a pinned listener-shape test
(`admin_http::tests::loopback_guard_works_under_production_listener_shape`).
The listener must use
`.into_make_service_with_connect_info::<SocketAddr>()` in
`daemon::start_daemon` — bare `axum::serve` leaves `ConnectInfo`
absent and the guards fail closed for *every* caller.

### Knowledge, ledger, peer prefs

- `MeshCorpusManager` / `ShardManager` — install / list / remove /
  shard prepare / install received / consolidate.
- `ShardManager::merge_participants(MergePlan)` — the ONE merge
  implementation (ARCH §10.6). `coordinate_merge` resolves who
  participated from handoff/queue state and then calls it;
  sovereign-mesh's fold-side collector resolves participation
  completely differently and calls the same function. A `MergePlan`
  carrying `expected_partitions: Some(n)` REFUSES
  (`corpus_engine::Error::IncompleteCoverage`) rather than produce a
  subset canonical that would advertise itself as complete on gossip;
  `None` keeps the legacy "merge whatever is present" behaviour, which
  is what `coordinate_merge` passes.
- **The merge finishes the job: `Ok(Some(info))` from
  `merge_participants` means REACHABLE.** `CorpusEngine::merge_partitions`
  → `sharding::merge_shards` writes the merged chunks and stops — the
  output carries `ingestion_in_progress: true, indexes_built: false`, so
  `installed_indexes()` skips it, `usable_indexes()` cannot search it,
  and `hosted_corpora` gossip — built from `installed_indexes()` in
  `sovereign-mesh::capabilities` — advertises nothing. Finishing it is
  `corpus_engine::finalize_canonical` (`corpus-engine/src/sharding.rs`):
  `build_indexes` → `mark_indexes_built` → `mark_ingestion_complete`,
  then the content fingerprint LAST, because a peer pulling against a
  fingerprint trusts the chunk set is stable and the ingestion-complete
  bit is the proxy for stable. One name for that sequence, shared with
  `merge_partitions_into_canonical`, which is where it was lifted from
  (ARCH §10.6). **It is the last step of `merge_participants` itself**,
  after the shard-dir cleanup — cw-lift 5g B8. Before that it was called
  by the FOLD-side caller only
  (`sovereign_grants::auto_recover::merge_from_fold_coverage`), and
  `coordinate_merge` — the queue-mode caller, reached from
  `routes_internal/corpus_queue.rs:210` and `:550` — shared the same merge
  and had the identical gap: a queue-mode merge produced a canonical
  holding every donor's chunks that `installed_indexes()` returned zero
  rows for. "A merge produces a corpus someone can reach" is a
  post-condition, so it lives with the function that promises it rather
  than with two callers each remembering (ARCH #10).
- When the merge lands and the finalize does not, `merge_participants`
  returns `corpus_engine::Error::MergedNotFinalized { corpus,
  canonical_path, chunks, detail }` and logs it at `error!`. Its own
  variant because the state is neither neighbour (ARCH §18.3): the chunks
  ARE on disk and the source partitions are already deleted, so that
  directory holds the only copy — "nothing was produced" invites a caller
  to re-derive from partitions that are gone, and "merged" claims a built
  canonical, which is the defect verbatim. The canonical is deliberately
  left in place. `merge_from_fold_coverage` carries the three fields
  across the crate boundary unchanged as
  `RecoveryOutcome::MergedButNotInstalled`; the two spellings stay
  separate for the same dependency-direction reason `IncompleteCoverage`
  does. Nothing retries it: the next tick short-circuits on
  `AlreadyHasCanonical`.
- `embed_http::http_embed_fn` — POSTs to `/v1/embeddings` so a node
  without a local embed model still ingests via the engine.
- `grounding.rs` — `GroundingConfig` + `search_for_grounding` +
  `format_knowledge_context`.
- **Dimensional contribution ledger**
  (`commonwealth-core::contributions`) — append-only event log
  (`LedgerEventKind` variants `InferenceServed`, `InferenceReceived`,
  `KnowledgeQueryServed`, `ShardTransferred`, `StorageSnapshot`,
  `JobUnitCompleted`) with pure aggregation into per-node
  `NodeContributions`. No `balance`, no exchange rate, no ranking —
  units are incommensurable. Storage in
  `commonwealth-state::ContributionEmitter` (gossip-replicated
  `MeshStore` under `app_id = "contributions"`). Pull-side
  `ShardTransferred` is emitted by the merge leader on behalf of
  the peer that shipped bytes — the schema carries an explicit
  `from_node`, and the aggregator credits `bytes_served` to it.
  **`JobUnitCompleted` is the work plane's dimension** (cw-lift 5h):
  a donor that ran another member's unit to a verdict credits
  `NodeContributions.compute_donated` (a `DonatedCompute` of `units`
  + `wall_seconds`), counted apart from inference because a CI shard
  is not an inference request. It is emitted ONCE by the donor, in
  `sovereign-daemon::work_donor::run_unit`'s `Ok` arm after its own
  signed `Complete` appends — deliberately NOT derived by every node
  that folds that act, because this log converges by "one write site,
  one event" while the `work` journal converges by total order, so a
  per-folder derivation would write one fact once per ring member
  under N distinct LWW keys. `work_donor::credit_for` carries the
  argument and the at-least-once analysis. `wall_seconds` is also the
  half a folding third party could not reconstruct: the journal
  carries lease-held time, not compute. `handoff` + `unit_hash` +
  `donor_actor` (the `ActorKey` admission verified, not the emitter's
  self-reported `node_id`) are the audit trail back to the signed act.
  **Nothing renders this dimension to a person yet**: `mesh_admin.rs`'s
  `NodeContributionsView` is a field-frozen flat mirror carrying only
  the inference/corpora/bytes fields, `svrn mesh balance` is a stub,
  and the daemon's `MeshStore` is in-memory — the rendering surface is
  the next rung's work, not this one's.
- **Local Activity ledger** (`commonwealth-core::activity`) — the
  glassbox counterpart answering "what is *my* daemon doing, even as
  a mesh of one?" A sibling of the contribution ledger, not part of
  it: `ActivityEventKind` variants (`LocalInferenceServed`,
  `EmbeddingsServed`, `LocalKnowledgeServed`, `ChunksIngested`,
  `CorpusEnriched`, `NewsworthyFetched`) record resource work that
  never crosses a peer boundary, and `aggregate_activity` folds them
  into one self-view `ActivitySummary`. Storage in
  `commonwealth-state::ActivityEmitter` under the **gossip-excluded**
  `app_id = "activity-private"` (in `GOSSIP_EXCLUDED_APP_IDS` — your
  own usage never gossips, the deliberate contrast to
  `contributions`). Recorded at daemon boundaries: the local arm of
  `routes_inference::chat_completions`, the `embeddings` handler
  (previously unrecorded — a peer using your embed model was
  invisible), and the corpus-ingest `ProgressCallback`
  (`ChunksIngested` on `Complete`, `CorpusEnriched` on the
  structural-atlas pass). Surfaced via `GET /internal/activity/
  {summary,recent}`. Desktop chat runs the in-process Runtime and
  never hits a daemon HTTP boundary, so its slice is read *derived*
  from the `ResponseProvenance` already persisted on each message via
  `SqliteStateStore::summarize_chat_activity` (no new write path).
  All three feed Settings → **Activity & Sharing** (rebuilt
  `SharingSection.svelte`), which also hosts "the reins": ingest
  throttle, mesh-quiesce, and peer-share ceiling/pause controls.
- **Peer preferences (Ostrom sanctions)** —
  `commonwealth-state::peer_preferences` is the local-only,
  gossip-excluded store of per-peer affinity multipliers (clamped
  to `(0.0, 1.0]`). The manifest endpoint reads `X-Node-Id` and
  multiplies advertised `CapabilityClaim.affinity` per peer; the
  penalized peer's scorer sees lower affinities and naturally
  routes elsewhere. Filtering enforced in two places
  (`peer_preferences.rs` + `store.rs`).
  Read and written over the wire since sv-surface svt-3:
  `GET /internal/peer-preference/list`,
  `POST /internal/peer-preference/{set,clear}`
  (`sovereign-daemon/src/routes_internal/peer_preference.rs`, mounted
  `server.rs:526-536`), reached by `TurnClient::{peer_preferences,
  set_peer_preference, clear_peer_preference}`. The clamp and the
  32-hex-char node-id precondition stay in the daemon — a client
  states neither. The desktop's three Mesh Health commands go through
  that client in BOTH bootstrap modes, which is what took
  `commonwealth-core` and `commonwealth-state` out of
  `sovereign-desktop/src-tauri/Cargo.toml`; before svt-3 the Attach arm
  refused with "set via `commonwealth peer-preference set` instead" and
  the list arm answered an empty list it could not distinguish from
  "none set".
- **Ten wire DTOs moved below the daemon** (svt-3, 2026-09-11).
  `sovereign_contracts::daemon_wire` holds the answers a client parses
  that are pure serde over primitives: `OcrAvailability`, `CancelAck`,
  `IngestJobAck` and `LocalSearchHit` (from `lc_http`), `NoteEntry`
  (`notes_http`), `LegacyDocumentEntry` (`documents_http`),
  `ConversationListEntry` and `CreateConversationResponse` (`turn_http`),
  and `McpServerView` / `McpMountStatus` / `McpServersResponse`
  (`mcp_config_http`). Each was defined inside the `*_http` module that
  serves it, which is the right home for a ROUTE and the wrong one for a
  TYPE: naming `OcrAvailability` — one `bool` — cost `sovereign-desktop`
  a layer edge onto `sovereign-mesh`. `sovereign-mesh` re-exports every
  one at its historical `*_http::Name` path, so the routes, their tests
  and the CLI are unchanged; this is a relocation, not a rename. The one
  consequence that is not: `impl From<Note> for NoteEntry` is illegal
  once both types are foreign to `sovereign-mesh`, so the projection is
  now the free function `notes_http::note_entry` — still one
  implementation, still its two callers. The test for whether a DTO can
  come down is that it closes over nothing but primitives.
  `lc_http::IngestProgress` (over
  `corpus_engine::enrichment::state::EnrichmentState`) and
  `mesh_http::StatusResponse` (over
  `commonwealth_core::capabilities::OriginKind` and
  `commonwealth_media::PeerTransportPath`, reached through
  `daemon::MemberReach`) do not, and stay where they are until the
  vocabulary they close over has a layer-0 home.
- **The mesh view came down, and the two that cannot got a READ** (svt-3,
  2026-09-11, same day, second landing). `OriginKind` moved to
  `oicp_types::origin` (commonwealth-core re-exports it), which unpinned
  the whole mesh view: `daemon_wire::mesh` now holds `MeshStatus`,
  `MeshMember`, `MemberStatus`, `ContributionSummary`, `MeshCorpus`,
  `CorpusStatus`, `JoinConfirmation` (from `types`), `MemberDto`,
  `KnownMeshDto` (`mesh_http`), `SelfReachability`, `ReachabilityStatus`,
  `RecoveryEvent` (`daemon` / `iroh_watchdog`), `RelayCandidate`
  (`mesh_discovery`); `daemon_wire::recipe_projects` holds the five
  `/v1/recipe-projects` answers plus `ArtifactKind` and `CheckpointMeta`
  (from `sovereign-recipe-author`, which re-exports them — the
  dashboard tags every project with the enum, and `snapshot_basename` /
  `label` travelled with it because an inherent impl cannot stay behind
  on a foreign type). Three private ctors became free functions in
  `recipe_project_http` (`list_entry_from_row_and_summary`,
  `validation_nothing_drafted`, `validation_failed`), the same orphan-rule
  consequence as `note_entry`. The two DTOs that still close over runtime
  types — `mesh_http::StatusResponse` (worker-eligibility view,
  cross-family transport path) and `lc_http::IngestProgress` (enrichment
  phase file) — stay, and the client is owed a READ of each, not the
  type: `MeshStatusSummary` and `IngestProgressView<Stats>` are the fields
  the desktop reads, parsed from the same bytes, pinned to the route's
  type field by field in `sovereign-mesh/tests/main/wire_view_drift.rs`
  (rename either side and it is red). `IngestProgressView` is generic over
  the receipt's counts so the desktop reads `IngestStats` typed while the
  contract crate names no capability-layer type. Two pass-throughs the
  desktop never field-read (`governance_http::GovernanceViewPayload`, the
  `corpus_watch_http` answers and `WatchedFolderConfig`) cross as
  `serde_json::Value` — the route's bytes forwarded, no mirror to drift;
  on register an absent `config` OMITS the key so the daemon's
  `#[serde(default)]` supplies its own default (a `null` would 422).
  Two reads the desktop used to do itself moved to the daemon: invite
  preview is `POST /v1/mesh/join/preview` (`JoinPreviewRequest` →
  `JoinConfirmation`, parsed by the SAME `parse_join_argument` the join
  uses, so a preview can no longer refuse a bare key the join accepts —
  `parse_deep_link` lives in `commonwealth-discovery`, which the contract
  layer cannot see, so it is a route and not a relocation), and the node
  id the desktop's corpus engine partitions by comes from `GET /status`
  (`DaemonIdentity.node_id`, `TurnClient::daemon_status`) instead of the
  daemon's `<data_dir>/node_id` file — the old read GENERATED an id when
  the file was absent, a second minter of the daemon's identity; now a
  host that answered the reachability probe but not `/status` refuses the
  boot in its own words, never an invented id.
- **The workflow job answers came down too** (svt-3, 2026-09-11, third
  landing). `daemon_wire::workflows` holds the thirteen `/internal/workflows/*`
  shapes (`WorkflowListEntry`, `WorkflowParamSpec`, `CapabilitiesQuery`,
  `CapabilitiesResponse`, `RunRequest`, `RunResponse`, `JobQuery`,
  `JobStatus`, `JobResponse`, `JobEvent`, `WorkflowJobEvent`,
  `JobItemOutcome`, `WorkflowListResponse`); `sovereign_workflow_host::
  workflow_http` re-exports them and the CLI's `workflow_cmd` is unchanged.
  The desktop named them through the workflow ENGINE crate — the last code
  reason for its `sovereign-workflow-host` edge. Orphan-rule consequence:
  `From<WorkflowProgress> for WorkflowJobEvent` is the free function
  `workflow_http::job_event_from_progress` and `JobStatus::from_event` is
  `status_from_event`, each with its one caller.
- **The mesh MUTATIONS cross too, same rung** (svt-3, 2026-09-11).
  `mesh_create`, `mesh_join`, `mesh_rotate_invite`, `mesh_switch`,
  `mesh_leave`, `mesh_forget`, `mesh_list`, `mesh_get_state`,
  `mesh_is_running`, `mesh_diagnostics` and `mesh_relay_candidates` in
  `sovereign-desktop/src-tauri/src/mesh_commands.rs` each held TWO
  implementations — an Attach arm hand-rolling `reqwest` against
  `http://localhost:{port}/v1/mesh/…` and a Local arm reaching into an
  in-process `EmbeddedDaemon` — and the pairs had already drifted:
  rotate exposed the client API on the wire path and not in-process,
  join accepted three invite forms over the wire and only a
  `sovereign://` deep link in-process, `mesh_list` re-derived its five
  fields from `known_meshes()` plus a direct `persist::active_mesh_id`
  read of a file the daemon owns, and `mesh_forget` worked in NEITHER
  mode (Attach refused naming the CLI; the arm behind that refusal
  asked a daemon this app no longer commissions). One path now,
  through `TurnClient` on `client_base_url()` — and not a choice
  between two live arms: `05586220f` left `AppState::mesh` initialised
  to `None` (`state.rs:318`) and written nowhere, so every Local arm
  deleted here was already answering "Members empty" on every boot.
  `POST /v1/mesh/forget` was WRITTEN for this rung and shares
  `SwitchRequest` with Switch, because both take one reference and
  resolve it through the same `persist::resolve_known` — which is the
  bug `forget_mesh` shipped once already, refusing the id prefix
  `switch_mesh` accepted. Two reads become `Err` where they were
  silence (`mesh_relay_candidates`, `mesh_get_state`), the same
  correction chunk 3 made for the preference list. ONE gap stays and is
  named rather than papered over: `mesh_diagnostics` returns no
  discovered peers — the mDNS table is the daemon's and no route
  carries it (task #37).
- See [`docs/MESH_LOAD_AWARENESS.md`](./docs/MESH_LOAD_AWARENESS.md)
  for peer-admission, contribution ceiling, and foreground-yield.

### Foreground yield is bounded (2026-08-18)

`YieldHook::should_yield()` answers "should I stand aside *right now*".
It is a **level** predicate — `now - foreground_last_active_ts < window`
— with no memory of how long the asker has already been parked, so any
request cadence shorter than `window` holds it true indefinitely. Every
consumer that parks on it therefore pairs it with
`corpus_engine_yield::DeferralBudget`, and that pairing is the
invariant, not a convention:

- `MAX_FOREGROUND_DEFERRAL = 300 s` (five whole default 60 s windows,
  and half the desktop's 600 s `enrich-once` client timeout). It is
  deliberately **not** an env flag or a config field: a liveness bound
  someone must remember to switch on is not a liveness bound.
  `DeferralBudget::with_cap_at_most` clamps to it, so a caller can only
  tighten the bound, never weaken it.
- `corpus-engine`'s two checkpoints (before each embed batch, before the
  enrichment phase) both go through `engine::yield_gate`, which owns the
  wait loop, the 5 s poll interval, and the three events — `yield:
  deferring`, `yield: resuming`, and `yield: deferral cap reached`
  (WARN, carrying `deferred_secs` and `cap_secs`). The override event is
  emitted inside the helper so no checkpoint can adopt the bound and
  forget to announce it. `YieldExit` is an enum rather than a bool so
  "resuming — foreground idle" cannot be printed after an exit that was
  a cap override.
- `AppStateInner::foreground_yield_remaining_secs` is the single
  implementation of the predicate itself. It previously existed three
  times (the hook, `should_yield_to_foreground`,
  `seconds_until_foreground_idle`) and the copies had already diverged
  on the backwards-clock case, so the `/internal/daemon/foreground_state`
  route could report the opposite of what ingest was doing.

**Why this exists.** On 2026-08-18 a `sovereign-server` mobile host
(`[[inference.backends]] type = "remote"` → `127.0.0.1:9741`) ran
`HybridProvider::start_health_loop(30)`, which POSTed a 16-token
`"ping"` completion to the local daemon's `/v1/chat/completions` every
30.0 s. That handler bumps foreground unconditionally, so a 30 s probe
against the 60 s window meant the window never lapsed; three consecutive
real-e2e runs died with `resuming enrichment` appearing zero times and a
misleading "bridge listen_any regression" message. The probe side is
fixed too — `HybridProvider::health_sweep` now probes only backends the
tracker has marked unavailable, since real traffic already maintains a
healthy backend's health — but the bound is what makes the invariant
hold for the case nobody predicted, including an ordinary user who sends
a chat every 30 seconds.

### Test harness

`sovereign-mesh-test-harness` (`commonwealth-test-harness` until the
`domains-2` move, 2026-09-11):

- `SimulatedMesh<S>` — orchestrates many `SimulatedNode<S>`s in-process,
  each with its own state `S` and HTTP listeners on random ports. `S` is a
  type parameter (the OICP/contracts seam): the harness's library names no
  host, and the caller binds it — `AppState` in `sovereign-mesh`'s tests,
  supplied through the `dst` feature's dev edge.
- `SimulatedNodeBuilder` — fluent hardware-profile builder.
- `MockLlamaServer` — Axum responding to `/v1/chat/completions` and
  `/health` with canned responses; request counting via
  `Arc<AtomicU64>`.
- `fixtures.rs` — reusable hardware profiles, models, capability
  profiles.

`tests/integration.rs` covers mesh formation, gossip convergence,
layer assignment, inference E2E through the mock server, fault
recovery, graceful pause/resume, OICP routing, multi-model
portfolio, knowledge fan-out, ledger accuracy. Deterministic timing
— no real 10s gossip waits.

### Distributed state + apps

`commonwealth-state::MeshStore` — SQLite KV (WAL mode):
`StoreEntry { app_id, key, value: Bytes, timestamp, origin: NodeId }`,
LWW conflict resolution, per-`app_id` namespace, `RetentionGc` for TTL at
the window `commonwealth-state::retention` declares.

**It is a PROJECTION of the ring rail since cw-lift 4, not a replica,
and readers are unchanged.** `get` / `scan` answer exactly what they
did; what changed is where a row comes from and where a write goes. Truth is the ring journal — an append-only signed log on disk, one
directory per namespace — and this store is the fold of it:

- **Out.** `set` / `delete` write the row and insert a
  `rail_outbox` row in the SAME transaction (`backend.rs`), drained by
  the pump in `sovereign-mesh` via `MeshStore::outbox_take(limit)` /
  `outbox_ack(&[id])`. A drained row is an
  `Outboxed { id, app_id, op: rail_kv::KvOp }` — the queued write and the
  journal line are ONE type, so a tombstone has one spelling
  (`op.value == None`) between the outbox, the wire and the fold
  (ARCH §10.6). The `rail_outbox.deleted` column is still written and no
  longer read. `merge_entry` deliberately does NOT enqueue — it
  is the receive side, and a row that re-entered the outbox would echo
  around the mesh forever. The SENDER-side privacy guard is inside
  `enqueue_on`, not at the call site: `is_gossip_excluded` is the one
  predicate, so an excluded namespace cannot enter the queue even by a
  caller who forgot (ARCH §7.1).
- **Vocabulary + fold.** `commonwealth-state::rail_kv` — one act,
  `{"k", "v" (base64, absent on a tombstone), "t", "d"}`, plus the snapshot
  MARK `{"snap": <the seal's seq>}` below, and
  `project(&Admission) -> Projection { rows, unreadable, sealed_actors }`.
  An op strictly below its own actor's `Admission::floors` entry is RETIRED
  and does not fold at all — exactly the set `RingJournal::compact` deletes,
  so the answer cannot depend on whether this node's prune has run yet, and a
  key a snapshot declined to re-append cannot come back from the retired line
  still on disk (§10.6). Per key the
  act with the greatest `(t, actor, id)` among admitted NON-VOIDED ops
  wins. `t` is the ORIGINAL write time, not the journal line's
  `ts_unix`: a snapshot re-append after a seal is a new LINE carrying an
  old WRITE, and folding on the line would hand every key to whoever
  snapshotted last. Acts this build cannot read (another app's
  vocabulary on the same rail — `mesh-measurements` — or a peer on an
  unknown shape) are COUNTED in `unreadable`, never dropped silently
  (ARCH §18.3). Tie-break on `(actor, id)` also closes the cross-node
  divergence `merge_entry_equal_timestamp_keeps_incumbent` pinned as a
  known limitation: on the rail there is no arrival order to depend on.
- **In.** `MeshStore::apply_projection(app_id, &Projection, origin_of,
  self_id) -> Applied { merged, deleted, reconciled, expired, withheld,
  unattributed }`. The
  whole `Projection` goes in rather than its rows, because the live sets below
  are the same fold's second answer and pairing fresh rows with a stale claim
  would retire a key on the strength of a different journal (§10.6). Values go
  through
  `merge_entry` (LWW at `t`); a tombstone deletes only what is not newer
  than it, in one statement so a concurrent `set` cannot be taken. An
  actor the roster cannot place is counted `unattributed` and skipped
  rather than attributed to an invented `NodeId`. An excluded `app_id`
  is refused with an `Err` naming it — the RECEIVER-side privacy guard,
  the half `routes_app_internal` used to do inbound before rung 2e deleted
  that route. Pinned end to end by `sovereign-mesh::ring_sync`'s
  `a_peers_private_namespace_is_taken_by_the_rail_and_refused_by_the_projection`:
  a hostile peer's private namespace IS ingested by `/internal/ring/sync`
  (the rail is author-blind, by design) and reaches no store.
- **The seal reconciliation — a tombstone keeps travelling past the seal that
  retired it.** For every actor in `Projection::sealed_actors`, the rows this
  store holds on that actor's behalf are reconciled to the set that actor
  asserts: a row whose origin is that actor and whose key the actor does not
  name is RETIRED (counted `reconciled`, apart from `deleted` — a tombstone is
  an op that says "delete this", a reconciliation is the absence of one from a
  set an actor has vouched is whole). **Two gates decide membership of that
  map and both are necessary**: the actor's `snapshot_mark` for its CURRENT
  floor is held, and no `RailGap::SequenceHole` names it — together, every seq
  from the floor to the mark is on this node, which is every row of the
  snapshot. The cheaper alternatives are both wrong and both are pinned as
  such: `is_complete()` is TRUE on a node holding only the seal (the hole
  audit runs `floor..=highest` and the seal is both), and "some op landed
  above the floor" is true of a half-arrived snapshot and FALSE of an empty
  one — which is the K7 case itself, an actor whose last live key was deleted
  just before it sealed. An actor absent from the map makes no claim, and
  absence is never read as an empty live set (§18.3). **`self_id` is
  skipped**, and that is the direction of truth rather than a special case:
  for a peer the journal is upstream of this store, for THIS node the store
  leads the journal by an outbox drain, so reconciling self would read our own
  lag as a retirement and delete a write still in the outbox. The pins are
  `a_seal_carries_a_delete_the_peer_never_received` and
  `a_snapshot_that_arrives_in_two_chunks_retires_nothing_until_the_mark`
  (`ring_sync`), and `apply_projection_never_reconciles_this_nodes_own_rows`
  (`commonwealth-state`).
- **Retention is part of the fold, because on a projection nothing else can
  be** (`commonwealth-state::retention`, 2026-09-08). A row a local sweep
  deletes has no incumbent, so the next round's `merge_entry` re-inserts it
  from the journal — `RetentionGc`'s thirty days on `contributions` were undone
  within a minute, every minute, on any node with an online peer, and the
  work atlas's 60 s sweep escaped only because it evicts through
  `MeshStore::delete` (a tombstone, an act) rather than through `gc_app`. The
  window is now declared once per namespace in `retention::RETENTION_WINDOW_DAYS`
  — `contributions` at `DEFAULT_WINDOW_DAYS`, which is the window its readers
  aggregate over, so a row past it is provably invisible — and BOTH sides read
  it: the fold refuses a row below the floor (`withheld`) and retires a held
  one (`expired`), and `RetentionGc::for_namespace` takes its TTL from the same
  table rather than from its caller (§10.6). Two cutoffs would spend every
  round undoing each other. **An expiry publishes nothing**: the floor is `now`
  minus a constant and `t` is on every op, so every node derives it identically
  — a tombstone per retired row would add a journal line on every node in the
  mesh for each row retention exists to remove. The store bound reaches the
  JOURNAL through the seal: `snapshot` re-appends the live set FROM THE STORE,
  so a row the floor keeps out is not carried above the new floor and the
  compaction deletes its line. `RetentionGc` is still the bound on a node with
  no online peer, because `run_one_round` returns before projecting anything
  when the peer list is empty. Pins:
  `a_retention_sweep_is_not_undone_by_the_next_projection` and
  `an_authors_own_retention_sweep_is_not_undone_and_puts_nothing_on_the_rail`
  (`ring_sync`), `the_projection_neither_takes_nor_keeps_a_row_past_the_retention_window`,
  `the_sweep_and_the_fold_keep_exactly_the_same_rows` and
  `a_delete_survives_the_fold_and_an_undeclared_sweep_does_not`
  (`commonwealth-state`).
- **A replicating `app_id` is a ring namespace verbatim**, and a
  namespace names a DIRECTORY (`<root>/rings/<ns>/`), so it must satisfy
  `commonwealth-rail`'s `valid_namespace` — `[a-z0-9_-]{1,64}`. The one
  offender in the workspace was renamed rather than the charset widened:
  `corpus_engine::update::newsworthy_watcher::APP_ID_TRACKED` is
  `wikipedia-newsworthy-tracked` (was `…:tracked`). A colon is legal on
  POSIX and APFS and is not on NTFS, and the desktop ships on Windows
  linking `sovereign-mesh` and through it `commonwealth-rail`.

- **The pump, the seal and the fold on the receive side are the daemon's**
  (`sovereign-mesh/src/rail_kv_pump.rs`). `spawn_rail_kv_pump` runs beside
  `spawn_ring_sync_loop` and drains the outbox every
  `RAIL_KV_PUMP_INTERVAL` (2 s): per row, `rail.journal(app_id)` →
  `rail.roster(&journal)` (THE door) → `journal.append(Record)`, then ack.
  Three verdicts, kept apart (§18.2): appended and acked; **deferred** on
  `RailError::NotInRoster` — a solo daemon is a normal daemon, the row STAYS
  queued and travels the moment membership exists, logged once per namespace
  per tick at debug; **refused** on anything else — acked WITH a warn naming
  the sentence, never a silent drop and never an infinite retry.
  `MeshStore` is `in_memory()` in production, so the pump's FIRST act at boot
  is `project_all_on_disk` — the store is rebuilt from the journals or it
  holds nothing at all.
- **On receive, the fold runs once per namespace per ring-sync round**, after
  every peer, in `run_one_round` — NOT inside `exchange`. Half the ops a node
  receives never pass through its own exchange: a peer PUSHES on call 2 of
  ITS exchange and those land through `/internal/ring/sync`, a route in
  another crate. A projection hung off our own pull count would be blind to
  exactly the direction a local write creates.
  `RoundOutcome::namespaces_projected` reports it, and
  `a_round_projects_the_namespace_even_when_it_pulled_nothing` is the pin.
- **A namespace's vocabulary is chosen by NAME, not by parse failure.**
  `rail_kv_pump::projector_for` is one selector returning `Some(Kv)`,
  `Some(Work)` or `None`, against `MEASUREMENTS_APP_ID` and
  `commonwealth_work::WORK_NAMESPACE`. Each fold's own `unreadable` count
  would work as a discriminator and would then report ~28 unreadable acts a
  round for a measurements ring behaving perfectly — worse for `work`, whose
  count would rise with how many leases a donor is renewing. A count that
  fires when nothing is wrong stops being read (§18.3). It is deliberately NOT
  keyed on `DAEMON_OWN_NAMESPACES`: that list answers whose roster the daemon
  derives, and keying the projector on it would change every app ring's
  projection. Both non-KV namespaces are read by `snapshot` through the same
  selector, so the send and receive halves cannot drift onto two answers
  (§10.6).
- **A local write does not wait for the 60-second round.** One
  `tokio::sync::Notify` is held by both halves: the pump raises it after any
  successful append and `spawn_ring_sync_loop` selects on it beside its
  interval sleep. Same wire, same sender — the pump never talks to a peer, it
  asks the one replication path to run. **Nor does a peer's return:** a round
  exchanges only with `Online` members, so `gossip.rs` raises the same
  `Notify` on the offline→online EDGE (never on every reach) and the writes
  made while that peer was gone go now rather than at the next tick — room
  run 2 of 2026-09-20 converged at 85 s against a 60 s bar without it
  (`sovereign-mesh/tests/main/ring_return_syncs.rs`).
- **Which namespaces replicate is DECLARED**, in
  `ring_roster::DAEMON_OWN_NAMESPACES`, and every entry is the constant its
  owning subsystem exports (`INFERENCE_APP_ID`, `CONTRIBUTIONS_APP_ID`,
  `PROCESSED_SHARDS_APP_ID`, `NOTES_APP_ID`, `APP_ID_PUBLIC`,
  `APP_ID_TRACKED`, `MEASUREMENTS_APP_ID`) rather than a literal repeated
  here (§10.6). `MeshRosterSource::install` derives a roster from membership
  for each. It has to be a list and not a rule: an APP's ring keeps a roster
  FILE, and there is no property of a namespace string that separates
  `inference` from `house-expenses` — a fallback like "derive when the roster
  file is empty" would silently admit every mesh member as an author of an
  app's journal (§7.1). A `MeshStore` `app_id` that is NOT on the list
  appends nothing and says so, which is the loud failure.

`all_entries_for_gossip` was the OLD contract — the whole store, at peers, on
a timer — and it is **deleted at cw-lift 2e** together with gossip Step 4,
`broadcast_now`, the `/internal/app/state` route, `recv_app_state`,
`AppStateGossipBody`/`GossipStoreEntry` and `backend::all_rows`. `merge_entry`
survives as the fold's own LWW upsert, called only from `apply_projection`.
There is ONE replication path.

`commonwealth-app` — mesh app platform: `MeshAppManifest`
(gossiped), `AppPermissions` (`mesh_store_read`/`_write`,
`inference_access`, `knowledge_access`), `AppRegistry`,
`AppProcess` lifecycle, `AppPortMap` + `forward()` reverse-proxy.

**Mesh-replicated workspace**. The `alignment` family replicates a
working set of files (default `~/.claude/`) across mesh peers
without a central server. Newest-mtime-wins via `merge_shards`'s
`mutable_merge = "source_doc_id_newest_mtime"` policy. Projector
under exclusive lock; mtime-stable. CLI: `svrn alignment`.
Corpus bytes are local-only (mutually-authenticated peers only —
not gossiped onto the open mesh).

### CLI

```
commonwealth init --name "..."          Create a mesh, get a join key
commonwealth status                     Node + mesh state (GET /status)
commonwealth balance                    Contribution ledger (local store)
commonwealth models                     Models advertised (GET /v1/models)
commonwealth corpus status              Ingestion/shard status (GET /internal/corpus/status)
commonwealth corpus collaborate <id>    Recruit peers for a mid-flight ingestion
commonwealth daemon start               Run the daemon
commonwealth recipe test/validate       Community-recipe harness
commonwealth peer-preference …          Per-peer affinity (local-only store; /internal/peer-preference/* serves it)
```

Every command does real work (2026-07-01): the aspirational
placeholders that printed `(In production, this would …)` and exited 0
were removed, and `status`/`models`/`corpus status` were implemented as
thin views over the HTTP control plane. Mesh lifecycle UX (create /
join / rotate / status across nodes) lives under `svrn mesh`;
daemon lifecycle under `svrn daemon`.

### Deployment

`contrib/`: `install.sh` (curl installer),
`systemd/commonwealth.service`,
`launchd/com.commonwealth.daemon.plist`.

### Desktop production-readiness (W1–W6)

A coordinated stack supporting the friends-and-family launch.
Failure modes addressed: daemon crash drops the whole UI, peer
work pins the GPU while the user is chatting. Components:

- **W1 — child-process daemon supervisor — DELETED 2026-09-11** (sv-surface
  svt-2). The desktop no longer starts, supervises, restarts or stops a
  daemon. `supervisor_setup.rs` (530 lines) and the `supervisor.rs`
  re-export are gone, and with them `AppState.supervisor`, the
  `supervisor-state` / `supervisor-fallback` events, the
  `supervisor_reconnect` / `supervisor_active` commands, and
  `main.rs::stop_daemon_child`. `main.rs` no longer carries
  `Launch::Daemon => exit(sovereign_cli_daemon::daemon_child_main())` — the
  desktop binary re-entering itself as the real daemon — nor the
  `ComputeChild`/`RpcWorker`/`Worker` arms that existed only because that
  in-process daemon re-exec'd `current_exe()` for its own children. All four
  now print `NOT_A_DAEMON` and exit non-zero: a GUI is not a daemon, and a
  window opening for `--daemon-child` looked like a hung service rather than
  a refused one (ARCH principle 6).

  **The deletion is structural, not a convention.** `sovereign-cli-daemon`
  and `sovereign-compute` leave `src-tauri/Cargo.toml`, so those arms cannot
  be rewritten without re-adding a dependency — the ability is gone, not
  merely unused (ARCH principle 12: look where the ability is GRANTED, and
  `cargo tree` is the check that works). `tests/no_daemon_role_census.rs`
  pins it at the source level.

  What the desktop does instead, and where each piece lives:

  * **Getting a daemon at all** — `serving_host::ensure_reachable` (svt-1),
    one call at startup that probes the client port and, in a
    `bundled-backend` build, brings the shipped sidecar up. It retains no
    handle, no restart policy and no shutdown budget.
  * **Noticing one is down** — `attach_watch` polls `/v1/models` through
    `sovereign_turn_client::ServingHost` and emits `attach-daemon-state`.
    Armed for EVERY Attach boot now, not just the subset that had no
    supervisor.
  * **Recovering** — `attach_restart_daemon`
    (`commands/supervisor_ctl.rs`) asks the OS service manager that owns the
    daemon to restart it. That is the one move a client can honestly make
    about a process it does not own.
  * **First post-wizard session** — both wizard completion paths still
    mirror the config and relaunch (`setup_flow::relaunch_after_setup`,
    gated on `setup_flow::daemon_runs_elsewhere`). The reason changed: the
    app looks for a serving host exactly once, at startup, BEFORE the wizard
    has written `config.toml`, so the wizard session cannot acquire one and
    the fresh instance can. `SOVEREIGN_FORCE_LOCAL=1` and the
    `SOVEREIGN_USE_SUPERVISOR=0` kill-switch still mean "this process runs
    the weights" and still skip the relaunch — unchanged for the real-mode
    harnesses. **The relaunch is `AppHandle::request_restart` since
    2026-09-11**, not `Command::new(current_exe()).spawn()` + `exit(0)`: the
    hand-rolled spawn was a thin surface starting a process (the census row
    it deletes), and it was wrong three ways Tauri's is not — inside a macOS
    `.app` it re-launched `Contents/MacOS/<binary>` rather than the bundle,
    it dropped argv, and it skipped `cleanup_before_exit`. The relaunch
    itself is NOT yet unnecessary; that needs the serving-host look to move
    after the wizard's write, which means re-resolving `bootstrap_mode`
    (`main.rs`, `state.rs`).

  * **Mobile access (the opt-in phone-facing host) — svt-2, 2026-09-11.**
    The toggle used to spawn a `sovereign-server` child with
    `kill_on_drop(true)`, hold the `Child` inside a task, and `child.wait()`
    on it: a second daemon, managed by a window, and four of the lifecycle
    census's ten burning-down sites. Both halves moved to owners.
    `mobile_host_setup::ensure_running` reaches the host through
    `ServingHost` — the one sanctioned bring-up — probing `/health` rather
    than the default `/v1/models`, which `sovereign-server` does not serve
    (`ServingHost::ready_at`, new the same day, keeps the default byte-
    identical). `mobile_host_setup::stop` calls the host's own
    `POST /v1/admin/shutdown`, authorized with the `sk-mobile-…` bearer the
    phone already uses. **That route is new**: the binary previously had no
    stop path of any kind — no route, no signal handler, no pidfile, no run
    lock — which is exactly why the app was holding a `Child` to provide
    one. The gap was `sovereign-server`'s and is closed there (ARCH
    principle 12). The desktop now holds nothing between the two calls, and
    the toggle still works both ways. Consequence worth knowing: toggle-off
    stops whatever serves that port, including a host this app did not start
    (`svrn mobile serve`, or a previous run) — the honest reading of a
    toggle that says whether mobile access is on for the NODE.

  `sovereign-compute/src/supervisor.rs` (the shared state machine: heartbeat,
  backoff 1s→5s→30s→2min, crash-loop ceiling counting CONSECUTIVE crashes and
  resetting only on a generation that stayed healthy for `healthy_reset_after`,
  bounded stderr ring, crash-log persistence) **stays** — `sovereign-cli-daemon`
  consumes it for the daemon's OWN compute children. A daemon supervising its
  compute children is right; a window supervising a daemon is not. Only the
  desktop CALLER was deleted.

- **svt-3a — in-process daemon hosting — DELETED 2026-09-11.** The row above
  used to end "still in-process, and NOT this rung's work": the `Local` branch
  commissioned a `sovereign_mesh::EmbeddedDaemon`, claimed the data root's
  `RunLock`, and loaded GGUFs in this process on every Local boot. All of it is
  gone from `state.rs` and the `builders::inference` module (itself deleted at
  svt-7). **There is one story
  now: the desktop is a client of a daemon it does not own, and the only
  question left is which port** (`AppState::client_port`).

  What went, and what each deletion took with it:

  * `sovereign_mesh::assemble` + `EmbeddedDaemon::new` + `DeferredDaemon`, and
    with them the whole `ServingCapability` this file built — the `/mcp` mount
    over a private `ToolRegistry`, `project_http`, `corpus_watch_http`, the
    `WatchedSubsystem` scheduler and `sovereign_workflow_host::
    workflow_http_router`. `AppState.mesh`, `AppState::mesh()`,
    `AppState.run_lock` and `AppState.watched_subsystem` are gone with their
    last readers; `mesh_commands.rs` reaches `/v1/mesh/*` in every boot.
  * `RunLock::acquire` on the data root, and with it the sv-surface B4
    re-probe (`probe_daemon_identity` + its four fixture tests). B4 existed to
    catch a process that concluded `Local` while really being a client. That
    state is now unrepresentable: `AppState::is_attach_mode()` returns `true`,
    full stop, and the doc on it says why rather than leaving a bare constant.
  * The in-process model load. The `builders::inference` module was left with
    one path — `build_daemon_provider` (renamed from `build_attach_provider`;
    there is no attach/local fork left to name it against), an
    OpenAI-compatible client on the daemon's `/v1` — and svt-7 deleted the
    module outright when the slot it filled turned out to have no readers.
  * **`smoketest.rs` (302 lines) and the `Launch::Smoketest` arm.** The
    subprocess re-exec'd this binary to decode one token against the chat GGUF
    so a ggml backend crash (the Gemma-4-on-Metal SIGSEGV) killed the probe and
    not the window. It guarded an in-process load; there is none. `Smoketest`
    joins `Daemon`/`ComputeChild`/`RpcWorker`/`Worker` in printing
    `NOT_A_DAEMON`, and both `[[thin_surfaces.lifecycle_allow]]` rows for the
    file leave `quality/ARCH_LAYERS.toml` — the `lifecycle-gate` burn-down row
    "smoketest.rs starts 2 / reaps 3" is CLOSED, not waived.
    `crash_report::record_native_crash` is orphaned by this and is an owed
    deletion. **What guards the Gemma-4-on-Metal SIGSEGV now: nothing, and the
    crash can no longer take the window down.** It happens in the daemon's
    process, which is the whole W1 rationale
    (`sovereign/docs/specs/DAEMON_RESILIENCE.md:75`); `attach_watch` notices and
    `attach_restart_daemon` recovers. The PRE-EMPTION is what is gone, and the
    shared implementation the daemon would call
    (`sovereign_inference::smoketest`, whose flag its `Launch::parse` already
    accepts — `sovereign-cli-daemon/src/lib.rs:316`) is untouched.
    `sovereign/DEFAULTS_LEDGER.md` carries the row for both model-load guards.
  * The rolling-summary `CompactionWorker` and `EmbedAdvertisement`. Neither is
    a client's to hold: the daemon owns the `sovereign.db` a compaction pass
    rewrites, and advertising an embedding model to peers is what a NODE does.
  * `launch_mode::get` and the `LAUNCH` cell. They existed so the commissioning
    site could name a `Launch` without a second `Launch::parse`; there is no
    commissioning site. `DaemonHost` — where the daemon RUNS — stays.

  **The reach moved to where a config first exists.** `main`'s
  `serving_host::ensure_reachable` runs BEFORE the wizard writes `config.toml`,
  so a first-launch session cannot acquire a host — which is the whole reason
  `setup_flow::relaunch_after_setup` exists. `bootstrap_with_progress` now asks
  again, immediately after `ResolvedModelSlots::load()` proves a config is on
  disk, and REFUSES by name when nothing answers. Two refusals, not one:
  `DaemonHost::InProcess` (`SOVEREIGN_FORCE_LOCAL=1`, or the
  `SOVEREIGN_USE_SUPERVISOR=0` kill-switch) names the flag and says the app can
  no longer run the weights; `SupervisedChild` names the port and
  `svrn daemon start`. **Both env flags now select a capability this build does
  not have** — they are inert at `DaemonHost::from_env` and only the refusal
  text tells the operator so. Retiring them from `quality/env-flags.toml`, and
  flipping `first-launch-setup.journey.spec.ts:86` (which SETS `FORCE_LOCAL`,
  and which `quality/instruments.toml` already records as "the branch real
  users never take") onto the default path `scripts/wizard-verify.sh` covers,
  are owed.

  **The desktop's CPU/arch substitution went too, and that is a fix with a
  gap.** The desktop's boot-time model-compat builder (deleted in 10b549b05)
  swapped a dense chat model in-memory on a
  CPU-only machine whose configured model is a recurrent arch that SIGSEGVs in
  ggml's CPU prefill. The swap never touched `config.toml`, so once the daemon
  became the loader it could not reach the weights at all — all it did was make
  `build_daemon_provider` derive a model id the daemon never loaded, under a
  `model-notice` banner claiming a substitution that had not happened (ARCH
  principle 6). It was already wrong in attach mode before svt-3 made attach
  universal. `sovereign_inference::cpu_compat` is shared, so the DECIDER does
  not move — only its caller, to the daemon's slot build, which has no such
  guard today (`grep choose_cpu_safe_chat_model sovereign/crates/sovereign-cli-daemon`
  → zero hits). Until it lands the guard has no owner:
  `sovereign/DEFAULTS_LEDGER.md` carries the row, for this and the GPU probe
  together.

- **svt-3b — the desktop commissions no `Runtime` — 2026-09-11.** svt-3a took
  the daemon; this takes the turn. `sovereign_runtime_recipe::{baseline_bundles,
  common_parts, commission}` are gone from `state.rs` and
  **`sovereign-runtime-recipe` leaves `src-tauri/Cargo.toml`** — one of the two
  paths by which `sovereign-tools` was reachable, which is the case the
  `[thin_surfaces]` reachability rule was built for.

  The desktop built a full private turn — a `SkillRegistry`, eleven
  `ToolBundle`s, the merged SCIP graph, the mesh knowledge client, the
  landscape-digest provider, a `KnowledgeViewManager` — **in attach mode too**,
  and every turn has crossed the wire since sv-surface R5. The attach-floor
  census named this the blocker: eleven of its twelve needles were consumed by
  the commission and nothing else. **Floor 12 -> 6.** What is left is what the
  desktop's own surfaces read: its `sovereign.db` handle, the corpus engine,
  the local-corpus manager, the tiered-enrichment provider, GLiNER, and the
  daemon-routing inference provider.

  **Four Runtime readers, and NOT ONE needed a new daemon route** (the audit is
  the reason this landed in one pass rather than behind a route queue):

  * `search_web` (`commands/models.rs`) reached `runtime.tools.get("search")`
    and was the only reader wanting a HANDLE. No daemon route runs a named
    tool — `/mcp`'s `tools/call` is allowlisted by
    `sovereign_tools::mcp_surface`, which does not carry `"search"`, over a
    registry holding only code-intel and notes tools; and the turn socket's
    `intent: SimpleAction { tool }` is accepted on the wire and discarded by
    both dispatchers (`sovereign-core/src/runtime/authority_guard.rs:386-394`).
    It did not need one: `submit_information_search`
    (`commands/conversation.rs:509`) has run this exact search with NO Runtime
    since it landed. Lifted to `state::web_search_once` and shared.
    **Behaviour delta, named:** the tool path collapsed every unhandled
    `StepOutput` shape AND a zero-result search into the literal
    `"No results found."`, saved it as an assistant message and returned `Ok`;
    both are `Err` with the backend named now, and nothing is written.
  * `ask_document`'s `Runtime::maybe_collaborate(.., abstained: false)`
    (`commands/document_asset.rs`) was a value-preserving identity function,
    provably: `run_collaboration` returns `NotAttempted` on `!abstained` before
    doing anything (`runtime/collaboration.rs:177-180`) and
    `maybe_collaborate` flattens that back to its input
    (`runtime/system_message.rs:779-784`). The comment above it said so in
    prose.
  * `cancel_stream`'s local pair is served by the daemon's
    `TurnRequest::Cancel` arm (`sovereign-daemon/src/turn_http.rs:1433-1449`),
    which runs the same two operations PLUS `approvals.abandon()` — a strict
    superset, on the process that owns the session store.
  * `redirect_turn`'s session -> conversation fallback has no daemon
    replacement BY DESIGN: `turn_extras_http.rs:22` says "NOT here … the
    surface already learns that pairing from the routing cards it receives",
    and `state.session_conversations` is that mechanism.

  Gone with them: `AppState.{runtime, notes, features, mcp_servers}` (none had
  a reader outside `state.rs`; `lessons`, `recipe_author_commands` and
  `mcp_list_servers` already reach `/v1/notes`, `/v1/features/*` and the
  daemon's MCP config), the knowledge-view state builder (its own attach
  guard already returned `None`, and attach is the only mode), the desktop's
  builtin-skills pass, and the `SplashProgress` recipe adapter — all three
  files deleted in 504c6b6d3.
  `AppState.entity_extractor` keeps its feature and drops a duplicate load: it
  used to arrive as `common.parts.lane.gliner` while this file loaded GLiNER
  separately for the corpus engine, and is now one `LazyGlinerExtractor` beside
  that load.

  **A feature gap is RECORDED, not created.** `AppState.routing_events` has no
  reader. The `interpretation-proposed`, `clarification-request` and
  `turn-narration` Tauri events it emits drive an inline banner, the
  `ClarificationCard` and the mid-turn narration chip, and they have been dark
  on the shipped path since R5 — the sink was only ever installed on an
  in-process `Runtime`. The wire ALREADY DELIVERS all three:
  `commands/chat.rs`'s `render_turn_frames` receives
  `TurnNotice::{InterpretationProposed, ClarificationRequest}` and uses them
  only to record the session pairing (`chat.rs:506-519`), and drops
  `TurnFrame::Narration` (`chat.rs:759-762`). Re-emitting them there is the
  fix; the field and `routing_events.rs` are kept inert so the payload shapes
  stay beside the gap.

  **Two censuses moved with their subject, both watched red.** The attach floor
  took six zeros and its total assertion went 11 -> 5.
  `authority_surface_census` broke at hop 2 ("state.rs composes
  `baseline_bundles`") and its own error text named the rewrite: the desktop can
  STILL install an SEC corpus by ticker, so the invariant holds and its chain
  now crosses a process — desktop installs into `rebrand::svrnmesh_root()/
  indexes`, the daemon reads that root, the daemon composes `baseline_bundles`.
  The shared root is asserted (hop 2a), because a privately-derived path on
  either side would let the desktop install a corpus the answering process
  cannot see, which reads as "no authority declared" and falls through to
  ungrounded streaming.

  **One census row was owed elsewhere and is paid here.**
  `sovereign-mesh/tests/main/daemon_variant_census.rs` listed `state.rs` as a
  live `DaemonServices::Desktop` construction site; deleting the commission
  turned it red, which is the failing input its own header names ("delete the
  last host that builds `Desktop`"). The row is removed and its claim moves to
  `the_desktop_variant_has_no_first_party_host`, which SCANS both crate trees
  and fails if any file outside `sovereign-mesh` reaches the arm. The variant
  stays: sv-surface's K3 keeps in-process hosting as a declared mode, and iOS
  — where fork/exec is forbidden, so a sidecar is impossible — is the standing
  case it is reserved for.

- **svt-6 — the desktop's engine was the daemon's engine — 2026-09-12.**
  `corpus-engine`, `sovereign-tools` and `sovereign-authoring-harness` leave
  `src-tauri/Cargo.toml`, and the **attach construction floor goes 3 -> 0**
  (`tests/attach_construction_census.rs`). What went is one thing wearing three
  names: a full in-process `CorpusEngine` whose builder chain paired `.with_*`
  for `.with_*` against `sovereign-daemon/src/bootstrap.rs`, over
  the same `~/.svrnmesh/{recipes,indexes}` root — the same recipes dir, indexes
  dir, embedding-model name, tiered provider, GLiNER extractor and `sec_edgar`
  acquirer as the daemon builds for itself.

  **Four of its five boot chores were DELETED, not moved, because the daemon
  already did them.** The lazy canonical-fingerprint stamp
  (`bootstrap::spawn_lazy_stamp_fingerprints`), the embed-dimension probe that
  armed clause ST-8's geometry gate (`daemon_cmd/mod.rs`), and — the one true
  delete — `validate_corpus_readiness`, whose ONLY caller in the workspace was
  this line and whose whole effect was a `tracing::warn!` in a client's log
  that no surface read. The `substep` glassbox timer went with its last two
  call sites.

  **The fifth MOVED, and the difference is user-visible.** The vector-index
  readiness sweep self-heals the index's own on-disk
  `IndexMeta.vector_index_built`, and its ONE reader —
  `corpus_catalog_http::catalog` — prefers that field over the state-store
  flag. With no sweep anywhere, a corpus whose LanceDB index finished but whose
  meta predates the field reports FTS-only forever. It is
  `bootstrap::spawn_vector_index_readiness_sweep` on the daemon now, beside the
  lazy stamp: sweep and reader in one process, over one engine. The old comment
  at the desktop site called this "a named gap"; this is the gap closed on the
  side that owns the root.

  **The one surface only the desktop's engine served became two routes** —
  `POST /internal/corpus/recipes/test` and `…/harness`, in the route table
  above — and the harness DRIVE is shared with `svrn recipe test` rather than
  pasted: `sovereign_authoring_harness::run_over_frozen_sample`, with rung 6 as
  a parameter because the CLI verifies atoms it just ingested into a temp index
  and the daemon verifies the corpus it actually installed. `HarnessRunCard`
  is deleted from the desktop; its `run` field named
  `sovereign_authoring_harness::HarnessRun`, the last thing holding that crate.
  `RecipeTestingPanel.svelte` and `HarnessLadderCard.svelte` are UNTOUCHED —
  all three commands were plain awaits with no events, so only the `use` lines
  and the poll moved.

  **The fifteen `local_corpus` DTOs moved DOWN rather than being reached
  through `sovereign-tools`**, into `sovereign_contracts::daemon_wire::
  local_corpus` with a `pub use` at every historical path (the dd8bb42e6
  pattern). That is what CLOSED three generics whose own doc comments said they
  were generic only because "the payload closes over a `sovereign-tools` type
  with no home at this layer": `PreScanAnswerView`, `ClusterProgressView` and
  `IngestProgressView`/`IngestOutcomeView` each name their type now. What
  stayed in `sovereign-tools` is what names the ENGINE or the filesystem —
  `recipe_toml`, `display_meta` (a free function now: it answers
  `corpus_engine::recipe::DisplayMeta`, and an inherent impl must live in the
  crate that defines the type), `file_meta_from_path`, and every manager,
  walker, clusterer and write-back implementation.

  **Eleven `[[exception]]` rows went STALE and were deleted in the same
  commit** — `sovereign-tools`, `arch-layers`, `sovereign-atos`,
  `sovereign-enrichment-catalog`, `sovereign-work-atlas`,
  `corpus-engine-{archaeology,atos,watchers}` and the three
  `commonwealth-{core,rail-core,state}` rows the mesh edge had held. `grep -c
  'from = "sovereign-desktop"' quality/ARCH_LAYERS.toml`: **24 -> 13**.
  `corpus-engine` itself did NOT go stale and that is the reachability rule
  earning its keep — it is still reached through `sovereign-core`,
  `sovereign-gliner` and `sovereign-inference`, the three lines svt-7 cuts.
  The `paddle-ocr` feature forward went too: it existed to make a
  `#[cfg(feature = "paddle-ocr")]` gate meaningful and there is no such gate in
  the desktop, and had not been for some time.

- **svt-7 — the daemon owns its weights and its setup, and the list reaches
  zero — 2026-09-12.** `sovereign-gliner`, `sovereign-inference` and
  `sovereign-core` leave `src-tauri/Cargo.toml`, and
  `grep -c 'from = "sovereign-desktop"' quality/ARCH_LAYERS.toml`: **13 -> 0**.
  The campaign's committed predicate check
  (`quality/campaigns/sv-surface.toml`) EXITS 0 — it exited 1 with 24 rows at
  `ffff8041e`, which is the rung that put the grep in the check so it could.
  `cargo xtask lifecycle-gate`: 0 burning-down.

  **First run had to be measured before it could be designed, and the answer
  was not a route.** The sidecar cannot serve HTTP unconfigured: it exits 1
  with no config off a TTY (`daemon_cmd/mod.rs`), refuses a config with no
  `[models]` (`sovereign-daemon/src/build/inference.rs`), and the app reaches it only
  after the wizard wrote config (`serving_host::ensure_reachable`). Operator
  call: the wizard SPAWNS the sidecar's own `setup` verb, which links nothing.
  `svrn setup --plan --json` prints `{hardware, profile, catalog, fast, embed}`
  and exits, touching no file; `svrn setup --yes --json [--primary <spec>]
  [--data-dir]` is the SAME run with stdout reserved for one
  `SetupProgressLine` per event and the human narration moved to stderr. That
  redirection is one decision in `setup_cmd/emit.rs` rather than a guard at
  each of the 111 `println!` sites — `say!` is the narration, `emit::line` is
  the wire, so a new narration line cannot land on the wrong stream by
  forgetting a guard. `--primary` is the non-interactive form of the picker
  (a catalog file, a `.gguf` URL, or a `.gguf` on disk used in place), and an
  unrecognised spec is refused with the tier's catalog listed rather than
  demoted to the recommendation.

  **`setup_flow.rs` stopped being a second wizard.** It ran its own hardware
  probe, catalog resolve, three `download_gguf` calls and `config.toml` write
  — in a process that owns neither the weights nor the config — and the two
  copies had drifted (the CLI asked the user to pick a primary and this did
  not; only one knew `--repair`). It resolves the pick into `--primary`,
  spawns, maps each line onto the `SetupPhase` frames the UI already renders,
  and keeps the three things that ARE the app's: the DesktopConfig beside
  `config.toml`, the first-run marker, and the relaunch.

  **Four types moved DOWN to `sovereign_contracts::daemon_wire::setup_plan`**,
  each re-exported at its old path: `HardwareProfile` and `ProfileName` (from
  `sovereign-inference/src/hardware.rs`), `SlotConfig` (from
  `sovereign-core/src/models_manifest.rs`) and `PrimaryOption` (from
  `setup_planner.rs`). Naming one used to cost a client the inference stack or
  the runtime hub. `HardwareProfile::detect` became the free function
  `hardware::detect_hardware` at 13 sites — an inherent impl cannot cross a
  crate boundary. The tier's SPELLING had five copies (`setup_planner`'s order
  array and its `resolve_slot` match, the desktop's
  `profile_name_str`/`parse_profile_name`, `setup_flow`'s `profile_str`);
  `ProfileName::{as_str, from_wire}` is the decider now and the serde form is
  the manifest's own section key.

  **`AppState.inference` is gone, and it is the principle-12 shape exactly.**
  It held a `SplitInferenceProvider` over the daemon's `/v1` and had ZERO
  readers — its last two touches were `= None` resets in `config_setup.rs`. A
  count that reaches zero while the ability stays is drawn wrong, so the
  ability went too: the `builders::inference` module is deleted. The one thing
  that call carried and had a reason to live — the boot's refusal when no
  embedding model is configured — is stated in `state.rs` as itself, same
  sentence, same Settings pointer. `fast_exit_skip_destructors` went with the
  C++ it existed to protect: no llama.cpp and no ONNX runtime are linked here
  any more, so there are no static destructors to skip.

  **The census pin got STRONGER, not deleted.**
  `the_attach_provider_construction_is_pinned` held
  `build_daemon_provider(slots)?` to exactly one call site; a floor of one
  became a floor of none, and `the_desktop_names_no_inference_stack` pins it
  at the place the ability is GRANTED — the manifest — rather than at the
  sites that used it (ARCH principle 12: look where the ability is granted).
  Its needles carry their call syntax because this crate's comments cite every
  deleted type by name on purpose.

  **The thirteenth row was a MOVE, and the reason is worth keeping.**
  `corpus-engine-sections` was reached through `sovereign-tools-base` — the
  one runtime-layer crate `[thin_surfaces].may_reach` permits — so what
  tools-base links, every thin client links, by reachability rather than by
  intent. The 2026-08-20 budget amendment that admitted the leaf was right
  about the leaf (`regex` + `tracing`, reached DOWNWARD) and could not see
  that. `rag::section` went back to `sovereign-tools`; `rag::chunk` did NOT,
  because it carries no such edge and the corpus-engine-free studio bundle
  would have lost a pure paragraph chunker for nothing. `standard_registry`
  therefore registers `chunk` and not `section`, and `section` joined
  `sovereign_tools::workflow_corpus_tools()` — the seam that has restored the
  corpus/atlas tools to every host that links the crate since B:P9d. **Banked,
  not funded:** `rag/chunk.rs` and `corpus-engine/src/chunkers/paragraph.rs`
  are two paragraph chunkers both named `chunk_text` with different parameters
  (ARCH principle 8), a behaviour-changing merge for a later order.

  **`windows-vulkan` / `windows-cuda` moved to `sovereign-cli-daemon`.** They
  were desktop features forwarding to a `sovereign-inference` the app no
  longer has, selecting a GPU backend for a process that loads no models. The
  build that needs them is the SIDECAR's:
  `SOVEREIGN_SIDECAR_FEATURES=windows-vulkan scripts/stage-daemon-sidecar.sh`.
  NOT VERIFIED on this host — the Windows legs are commented out in
  `desktop-release.yml` and this is a macOS box; the cross-check path is
  `scripts/windows-crosscheck.sh` plus a native run.

  **One instrument bug, caught by running the check rather than reading it.**
  The prose written into `quality/ARCH_LAYERS.toml` to record this burn-down
  originally quoted the row key verbatim, and the campaign predicate is a bare
  `grep -q` over that file — so the check stayed red with zero rows present, an
  instrument failing for a reason that is not the fact it measures (ARCH
  principle 5). The comment is reworded and says why.

- **W2 — peer-admission middleware**
  (`sovereign-serving-host/admission.rs`, the decider and both middlewares;
  `sovereign-daemon/src/admission.rs` keeps the `AppState` port impls and the guards —
  REVIEW-build-serving-move-admission, 2026-09-15) — applied to client-port
  `/v1/chat/completions` + internal-port
  `/v1/knowledge/search`. Local requests admit unconditionally;
  peer requests are rejected with 503 + `Retry-After` when paused,
  yielding to a recent local foreground request, or refused by the
  fair scheduler. A peer corpus read (`/internal/knowledge/search`,
  `PeerWork::KnowledgeRead`) meets only the pause gate and its own
  `[daemon] max_peer_knowledge_reads` budget (default 4,
  `serving.knowledge_read_sched`), never the yield or the inference
  ceiling (seat A23). The flat ceiling became a **`serving_policy::fair_sched::SchedCore<Principal>`**
  (`AppStateInner.serving.peer_sched`): a runtime-mutable global ceiling
  (`set_slots`, `0` = reject all) **plus a per-principal concurrency cap**
  so one peer can't hog the pool, **reciprocity-scaled** — a
  contributor's effective cap rises toward the ceiling, read from a
  cached per-node weight table (`reciprocity_weights`, refreshed
  ~30 s by a daemon loop from the contribution ledger). This is the
  host-side convergence point for a shared-model fleet (every
  consumer's turn lands here as a peer request, keyed by the published
  `Principal`'s `Member` arm built from `X-Node-Id`; `DAEMON_CORE.md`
  §3.3).
  **One canonical wire form (order commons-fluency fix 7):** the header
  value is `NodeId::to_hex()` — exactly 32 lowercase hex chars, the
  encoding of the 16-byte id; `sovereign_contracts::principal::claimed_node_id`
  accepts nothing else (`node-<16hex>` strings from `/status` rows are the
  DISPLAY form and must never be echoed back as a header). That function is
  the ONE production read of the header anywhere under `sovereign/crates`,
  enforced by a test that greps for it and for the literal header
  (`sovereign-daemon/src/mesh_principal_gate.rs`, order
  `mesh-verified-principal`): every decider reads the `Principal` a resolver
  attached, never the wire. A present-but-malformed value resolves to
  `Principal::Unverified` and is REFUSED by the peer gate — a ceiling keyed on
  an id nobody proved is a ceiling any caller can pick — while the `/status`
  zero-bucket row still names the rejected raw value, when it was last seen,
  and the expected wire form.
  **On the INTERNAL plane the header is not an identity at all.** A request
  that crossed the iroh acceptor carries the Ed25519 key the QUIC handshake
  proved (`internal_principal.rs`); a request that reached `:9742` some other
  way has its `x-mesh-*` stripped and resolves `Unverified`, and any
  `x-node-id` on it is ignored. The four senders still STAMP the header for
  one release, because a receiver on an older build routes on its presence.
  **Per-principal client fairness (order `serve50-identity`, 2026-08-13):**
  the gate above rations traffic that NAMES a node; its sibling
  `client_fairness_layer` rations traffic that does not. The two are
  disjoint by construction — both split on one predicate,
  `claims_peer_identity`, over the resolved `Principal` — so a request meets
  exactly one of them and is never double-gated. It keys the same policy core as
  **`SchedCore<Principal>`** (`AppStateInner.serving.client_sched`), where
  the principal comes from `sovereign-daemon/src/client_principal.rs`: the ONE
  resolver, `AppState::resolve(headers, peer, policy)`, which covers all
  five arms of the published `Principal` — a live guest grant →
  `Guest` (the grant store decides; the key is a fingerprint); another
  readable `X-Node-Id` → `Member`, an unreadable one → `Unverified` (read
  BEFORE the loopback branch, because a mesh peer arrives on the trusting
  listener over loopback, and BEFORE the non-guest bearer, because peers
  present both and the bearer is the same daemon-wide bytes for all of them);
  another presented `Authorization: Bearer` → `RemoteClient` (a `WorkerToken`
  rides this branch, it is a plain bearer); `X-Principal` from a
  **loopback** caller on a listener that trusts a loopback peer address →
  `LocalOwner`; else `Anonymous`. It is the one resolution at the edge:
  `client_auth_layer`'s credential/grant decision, the host's
  `AdmissionHost::resolve` port (which this layer calls) and
  `peer_admission_layer`'s `Member` construction all come through it. It
  is deliberately NOT `sovereign-contracts`'
  `PrincipalResolver` (`traits.rs:106`), which keys on a conversation
  id that stateless `/v1/chat/completions` does not carry. The share
  rule is `serving_policy::fair_sched::fair_share_cap(budget,
  active)` — `u32::MAX` when one principal is alone on the host (so
  single-caller load is untouched), else `max(1, budget / active)`.
  It takes no weight argument, so the weight-ordering condemned by
  `SCHEDULER_QUALITY.md` F6 is unexpressible rather than merely
  avoided. Two properties are load-bearing and easy to undo by
  accident: the scheduler's global slot budget is `usize::MAX` so this
  gate can NEVER refuse on depth (the inference slot queue's
  predicted-wait shed, `model_slot.rs`, remains THE shed decider), and
  it uses `try_grant`, which leaves no waiter behind, so a refused
  caller cannot park. Measured red→green on the §9.3 harness: the
  polite cohort went from 0.21× to 0.63× fair share
  (`research/scale-analysis/MESH_SCALE_100_USERS_1000_CORPORA.md`
  §9.5). Applied to `POST /v1/chat/completions` only — `/v1/responses`,
  `/v1/completions` and the Ollama shim are ungated. Knobs:
  `SOVEREIGN_CLIENT_FAIRNESS` (kill switch, default on),
  `SOVEREIGN_CLIENT_FAIR_CONCURRENCY` (budget, default 16). Decisions
  log under the `admission` tracing target, which is in the daemon
  filter allowlist.
  **Notes-rail convergence liveness (order commons-fluency fix 9):**
  `/status` also carries a `convergence` section — the answer to "is
  the publish path alive?" (§9.5). The daemon boot creates ONE shared
  `ConvergenceRecord` (named on the daemon's
  `DaemonServices::Headless` rails and installed into
  `AppStateInner.fabric.convergence` at AppState construction in
  `sovereign_daemon::start_daemon`), the notes publish sink stamps
  `last_outbound_publish_at` on every successful `set()`, and the
  notes ingest poller stamps `last_inbound_ingest_at` on every
  applied peer batch — so `/status` reads the writers' own stamps,
  never a parallel copy. Each arm's age is reported as a BRACKET
  (`0-30s`, `30s-2m`, `2-5m`, `5-30m`, `>30m`, or `never` — points
  would overstate the precision of a cadence-bounded measurement,
  operator steer note 83214914), and an arm silent past the 300s
  alarm threshold (30 ticks of the 10s cadence) reads `stale` rather
  than pretending. `never` is NOT stale: a path that never fired has
  no silence duration to alarm on — the regression the alarm exists
  to catch is a path that WAS alive and went quiet (the 41-minute
  silence of defect 9).
  **That last clause only became true on 2026-08-06** (M5 piece 3,
  `MESH_N4_TOPOLOGY.md` §M5): the gate keys entirely on the presence
  of `X-Node-Id`, and mesh inference did not stamp it, so every
  forwarded turn was admitted as the receiving node's OWN local
  traffic — pause, foreground-yield and ceiling all dark. Measured
  before the fix: four concurrent peer requests served with
  `peer_inflight_current` never leaving 0. `provider_for_peer` now
  stamps it, and `InferenceRouter::book_peer_failure` exempts
  the resulting sheds from `PeerHealthTracker` — a `503` from this
  gate is a healthy peer declining, and booking it as a fault would
  quarantine that peer for 60 s after three of them.
  `PrincipalInflightGuard` is RAII (`release`s the principal's slot on drop,
  accurate under panic unwind). The **same `SchedCore` policy** backs
  the chat server's turn scheduler (`sovereign-server/scheduler.rs`),
  so both admission gates are fair by identical rules.
- **W3 — tray status chip + pause submenu**
  (`sovereign-desktop/src-tauri/src/tray.rs`).
- **W4 — first-mesh-join consent** —
  `DesktopConfig.first_mesh_consent`; ConsentGate renders when
  unset.
- **W6 — self-service support surface.** Built for onboarding
  non-developers who must be debugged remotely from artifacts they
  can produce unaided. Three layers, in the order a person hits them:
  - **Fix it yourself** — `health.rs` runs seven checks (`engine`,
    `model`, `mesh`, `mesh_peers`, `knowledge`, `disk`, `stability`)
    over a `HealthFacts` struct gathered by `commands/diagnostics.rs`.
    `evaluate` is pure; every non-OK check carries a terminal-free
    `fix_hint`; an unreachable probe renders `Unknown`, never a
    fabricated verdict. Rendered by `HealthPanel.svelte` at the top of
    Settings → Diagnostics, and reachable from the reconnect banner's
    **Check my setup** (via the `settingsNav` store, which App.svelte
    refuses mid-setup).
  - **Report the machine** — `prepare_diagnostic_report` writes
    `~/Desktop/svrnmesh-<reason>-<ts>.md` for any `ReportReason`, not
    only a crash. `ReportReason::parse` degrades unknown → `Other`: a
    user trying to report a problem is never blocked by an enum.
  - **Report one answer** — `turn_report.rs` + `prepare_answer_report`,
    for the complaint machine state cannot explain. The snapshot comes
    from the **frontend**, assembled from the assistant message's
    persisted metadata (route, sources, backend/peer, gate action),
    because `TurnProvenance` holds only the newest turn of a
    conversation, in memory, on one register. Each report carries a
    speakable `reference_code` derived from `message_id` via a **pinned**
    FNV-1a — a wire format, not an implementation detail: change it and
    a user's screenshot stops matching their own report file. Passage
    *text* is opt-in per report, defaulted off, and `render_turn_section`
    enforces the gate itself rather than trusting the caller.

  Every report is a file on the Desktop the user reads before sending.
  No auto-upload anywhere, and the report **states its own contents** —
  the disclosure text is derived from what is actually in the file, so
  a state-only report, an answer report, and an answer report with
  source text each describe themselves honestly. User-facing doc:
  [`docs/HAVING_TROUBLE.md`](./docs/HAVING_TROUBLE.md) (no terminal);
  the maintainer-facing `docs/TROUBLESHOOTING.md` points at it.

**MeshApp bridge (first-party sandboxed apps).** A mesh app runs in a
`meshapp-<id>` webview reached only through a permission-gated bridge.
`src/meshapp.rs` owns authorization — the app id is derived from the
host-set webview LABEL (unspoofable from JS) and checked fail-closed by
`authorize` against the granted subset in `DesktopConfig.meshapp_installs`.
`src/commands/meshapp.rs` exposes the `meshapp_*` commands: deterministic,
read-only `read_corpus` / `parcel_analytics` (reusing corpus-engine's
`compute_aggregates`, so no model originates a figure on the desktop
surface either); the graph-explorer family `graph` / `node` / `findings` /
`search_entities` / `reconciliation` / `subgraph` (top-degree nodes + induced
edges, for a node-link map) / `corpus_stats` (scale/provenance counts) /
`timeline` (documents bucketed by month, parsed from the `Date:` header every
email chunk carries) / `read_chunk`; host-only install management; and
`meshapp_open` (`WebviewWindowBuilder` + the
`meshapp_shim.js` `window.meshApp` shim over `__TAURI_INTERNALS__` + a
per-window strict CSP set in `on_web_resource_request`). The graph ops'
LOGIC lives in the **`sovereign-meshapp`** library crate (pure path-in /
DTO-out, Tauri-free) so the desktop host and the `svrn meshapp dev`
CLI server share one source of truth; the Tauri commands are thin wrappers
(permission gate + resolve the corpus's on-disk index). The ops are
**backend-agnostic**: `load_graph` dispatches on what the index
carries — a deterministic `investigation/` graph (UAP) or an `atlas/`
enrichment (Enron), projecting both into one DTO contract
(`GraphNodeDto` / `EdgeDto` / `NodeDetailDto`). The atlas adapter maps
Entity atoms → nodes and Relation/Event atoms → cited edges, resolving
each `sec_NNNNN` evidence id to a `chunks.lance` row via `chapters.json`
so `read_chunk` dereferences the source document unchanged
(**that resolution depends on the chunk→section join described below, and
until 2026-08-05 the join was empty on all but 9 local corpora**);
`reconciliation` surfaces the cross-origin merge log as the identity
glassbox. Six first-party apps ship on this surface: SF-LVT
(`public/meshapp/lvt/`, deterministic parcel compute), UAP Blue Book
(`public/meshapp/uap/`, investigation graph), Enron
(`public/meshapp/enron/`, story-first atlas experience), **Wrapped**
(`public/meshapp/wrapped/`, a story-card show over the user's own
`conversations-anthropic` corpus), Federalist
(`public/meshapp/federalist/`, the copyable complete example), and Atlas
Explorer (`public/meshapp/explorer/`, a generic corpus-bound atlas
explorer). Wrapped's op is different in kind: `wrapped_artifact` serves
a **precomputed artifact**, never live inference —
`sovereign-meshapp/src/wrapped.rs` folds every figure deterministically,
runs a **verbatim-citation audit** (`verify_wrapped_artifact`: every
cited chunk id must resolve, every embedded quote must be a verbatim
substring of its chunk — a failing artifact is never served), and caches
`<index>/wrapped/all-time.json` keyed on the corpus fingerprint (opening
the app rebuilds a stale artifact on demand). Cards are typed; absent
data ⇒ absent card; unknown card types are SKIPPED — the forward-compat
seam future enriched cards ship through. The **v3 deck** is Scale,
Rhythm, Recurring, Turn, Obsessions, Night Shift, Cast, Door; the folds
that need enrichment or geometry live in `wrapped/semantic.rs`, the rest
in `wrapped.rs`. Three things about it are load-bearing and expensive to
rediscover. (1) **Themes come from RAPTOR `primary_entities`, not
GLiNER** — measured on `conversations-anthropic`, GLiNER's top of archive
is `People (77) · WORK (53) · Companies (46)` where RAPTOR's is
`San Francisco (37) · Federal Reserve (33) · Taoism (13)`: nouns versus a
life. `ThemeIndex` is source-agnostic (`from_enrichment | from_ner`) so an
un-enriched corpus still gets a deck, at lower quality. Themes rank by
z-scored log-odds against the archive baseline (Monroe et al.), never by
frequency — frequency ranks the baseline and returns the same list every
quarter, which is what made v2 read as topical co-occurrence. (2)
**`ConvDoc::turns` is the PARSED SUBSET of a conversation, not its
shape.** A chunk yields turns only where its text carries a
`### [ts] role` header, and 13,373 of this archive's 16,404 chunks do not
— they are mid-answer continuation fragments that begin mid-sentence.
Anything reasoning about conversation SHAPE must read
`ConvDoc::chunk_ids`; `turns` is for quotes and clocks. Reading shape off
`turns` cost the Turn card 90% of its evidence (1,510 of 15,283 seams,
135 of 425 conversations) until 2026-07-26. The corollary is a licence,
not just a warning: because an unparsed chunk provably holds no turn
boundary, "the last thing you said before the seam" is correct at any
chunk distance, so the quote walk is deliberately unbounded. The same
blind spot ran through TEXT until 2026-07-26: a `parse_turns` block
stops at its chunk's edge, so a turn's words have to be walked forward
across the continuation chunks it spills into (`continuation_lead` +
`build_conv_docs`). Counting header-bearing text alone saw 19.9% of the
archive and reported 704,924 words at a 2.7x assistant:user ratio where
the truth is 3,512,842 at 14.9x. (3) **The archive stamps UTC, and the
deck shows one clock — the reader's.** `semantic::LocalClock` is
inferred once per build (`infer_utc_offset` locates the 4h trough in the
user-turn histogram and places its centre at 03:00 local; this archive
infers UTC-7) and handed to every card that shows an hour: the Rhythm
heatmap shifts whole datetimes, weekday included, and the Night Shift
bands read the same offset. Two cards inferring it separately is two
chances to disagree in front of the reader — which is exactly what
shipped in v3, where the grid peaked at 20:00 UTC while Night Shift
called those same turns 13:00. `WRAPPED_SCHEMA_VERSION` (now 4) is the
lever that forces a cached artifact to rebuild when a fold change like
this must reach existing installs before the corpus next updates. Bundles compose the
**MeshApp SDK**
(`public/meshapp/_sdk/`, dependency-free ES modules served under the CSP): a
corpus-bound `connect()` bridge client, CSP-safe DOM helpers, and the reusable
views (force-graph, timeline, reconciliation reveal, entity-detail, cited-edge,
search, scale-banner, and Wrapped's `storyShow` full-screen card shell +
`heatGrid` hour-of-week view in `story.js`) + `meshapp.css` — Enron's bundle
is ~150 lines of
composition, not ~600 of hand-rolled DOM. Each bundle carries a self-describing
`meshapp.json` manifest (id/name/corpus/grants/entry/trust — the unit a registry
distributes); `scripts/gen-meshapp-catalog.mjs` (pre{dev,build}) aggregates them
into `meshapp/catalog.json`, and `MeshAppsSection` discovers apps from it via
`loadCatalog()` rather than a hard-coded list. So adding an app is a bundle + a
manifest (+ an atlas reader only when the backend differs) — no host code edit.
**Local dev loop:** `svrn meshapp dev <id>` (sovereign-cli-llm) serves a
bundle + its `_sdk/` and injects a `window.meshApp` that proxies the explorer
ops over HTTP to the same `sovereign-meshapp` functions, reading a local corpus
index — so a bundle is iterable against real data without the desktop;
`svrn meshapp new <id> --corpus <c>` scaffolds one. **Corpus as a managed
dependency:** a manifest's `corpus_data` (size + the recipe the bundle ships,
carrying a `[prebuilt]` HF block) makes the corpus first-class — `MeshAppsSection`
shows its presence and, when missing, a one-click **"Get data (N GB) & Open"** that
stages the recipe (`meshapp_stage_corpus_recipe` → `~/.svrnmesh/recipes/`) — refusing a
body with no `[corpus]` table, so a dev-server SPA fallback (200 + index.html) can never
poison the recipes dir — and runs
the existing prebuilt install with a progress bar. **Curated registry:** `svrn
meshapp publish/install/list` (sovereign-cli-llm `meshapp_registry.rs`) distribute an
app as a self-contained `tar.zst` (bundle + a copy of `_sdk/`); install verifies the
artifact's sha256 (refuses tampering) and unpacks under `~/.svrnmesh/meshapps/<id>/`.
TRUST = integrity (sha256) + curation (membership in the reviewed
`sovereign-recipes/meshapp-registry.toml`); `meshapp dev` runs installed apps. The host
enumerates them via `meshapp_installed_apps()`; in-window opening of an installed app
(serving it from the install dir via a `meshapp://` scheme) is the remaining
integration. End-to-end runbooks: `docs/MESHAPP_CONSUMER.md` (replicate a demo) and
`docs/MESHAPP_AUTHORING.md` (recipe → corpus → app → publish). **Isolation caveat:**
Tauri v2 does not gate app
commands per-window (tauri#9227), so `capabilities/meshapp.json` only narrows
the core/plugin surface. Since 2026-09-20 the app-command half is decided in
the host instead: `meshapp::bridge_refusal` (label and command in, refusal out)
runs in the ONE invoke closure in `main.rs` before dispatch, and a `meshapp-*`
window may invoke only the eighteen `MESHAPP_BRIDGE_COMMANDS` — parsed back out
of `meshapp_shim.js` by a set-equality test, so the allowlist is never hand-kept
beside the shim. That bounds the surface to the bridge; the bridge itself is
still reached over IPC, so full isolation for UNTRUSTED third-party apps remains
the deferred no-IPC-bridge milestone (custom protocol / postMessage). The bundles are verified headlessly by
`tests/e2e/specs/meshapp-{lvt,uap,enron}.spec.ts` (Playwright, a11y
locators), each mocking `window.meshApp` + one real-shim→IPC wiring test.

**Accessibility tooling.** `npm run a11y` (`tests/e2e/scripts/a11y-report.mjs`)
is a dev-runnable, NON-BLOCKING axe-core scan of the chat surface + the
mesh-app bundles, writing a readable report to `test-artifacts/a11y/`
(glassbox insight into a11y shortcomings; no CI gate). Two reusable a11y
seams live in `packages/chat-ui` and are shared by desktop + mobile:
`completionAnnouncement` (per-turn screen-reader wording for the polite
completion live region — announce on completion, never per token) and the
`use:dialogFocus` action (modal focus-trap + focus-restore-on-close,
adopted by `MeshJoinDialog`/`MeshSettings`/`NewProjectDialog`/
`DocumentInspector`/`EchoOverlay`). Dynamic a11y behaviours (live regions,
focus restore) are verified by manual screen-reader testing, not axe.

Control routes (loopback-only, on the internal port :9742):
`GET /internal/contribution/status`,
`POST /internal/contribution/ceiling`,
`POST /internal/contribution/pause`,
`POST /internal/contribution/resume`,
`GET /internal/contribution/recent`,
`GET /internal/activity/summary`,
`GET /internal/activity/recent`.

Open polish: tray icon tint, HintCues nudge to Sharing tab, removing
the in-process `EmbeddedDaemon` fallback entirely (the default-flip
itself landed 2026-07-18 — the fallback remains as a surfaced degraded
mode), and graceful SIGTERM-with-grace on daemon shutdown. Daemon-side
resilience roadmap:
[`docs/specs/DAEMON_RESILIENCE.md`](./docs/specs/DAEMON_RESILIENCE.md).

**W7 — live-turn re-attach (streaming survives a conversation switch).**
`chat.machine` owns exactly ONE conversation's `messages` +
`streamingMessageId` and wipes the latter on every `HYDRATE`
(conversation switch), so a turn the user navigated away from was
orphaned: the `conversation_id`-tagged `message-chunk` /
`message-complete` / `message-error` events were dropped by the
`messageId` guard, and the backend persists the assistant row only
AFTER the stream ends (`StreamHandle` contract) — so on return there
was no row, no loading affordance, and the answer never landed. Most
visible on a slow turn whose synthesis is offloaded to a mesh peer
(minutes-long, long enough to switch away). Fix: a runed singleton
registry `stores/liveTurns.svelte.ts`, fed by the global stream
listeners keyed on `conversation_id` regardless of which conversation
is on screen; `ChatView.loadConversation` re-attaches on return
(`REATTACH_STREAM` restores the affordance + partial text for an
in-flight turn; a terminal turn renders its answer/error). Scope: lives
while ChatView is mounted (survives conversation switches, NOT app
restart / Settings-Atlas unmount — that durability belongs to a
persisted streaming placeholder row, deferred). `message-error` now
carries `{conversation_id, message_id}` (`commands/chat.rs`
`MessageErrorPayload`) so a failed backgrounded turn is attributable.
Pinned by `tests/e2e/specs/chat-orphaned-turn.spec.ts` +
`stores/liveTurns.test.ts`.

### Pinned worker pods as inference peers

Ephemeral worker pods (Vast L40S rented via `pipeline pod up`)
join the mesh scheduler's inference pool as one more peer, scored
by the same OICP load balancer. Pods aren't gossiped — owner-
private, TLS-pinned, authenticated by Ed25519 `WorkerToken`. See
[`docs/PINNED_WORKER_AS_INFERENCE_PEER.md`](./docs/PINNED_WORKER_AS_INFERENCE_PEER.md)
and [`docs/EPHEMERAL_WORKER_PODS.md`](./docs/EPHEMERAL_WORKER_PODS.md).
>>>>>>> origin

---

## 6. How the four projects fit together

**Sovereign standalone** — Tauri / CLI / server against `EmbeddedLlamaCpp`,
knowledge via `MeshCorpusManager` (named for the mesh case but works without
one).

**cmnwlth standalone** — daemon on `localhost:9741`; any OpenAI-compatible
client points at it. Ingest uses `embed_http::http_embed_fn`, so a node with
no local embed model still indexes.

**Integrated** — `EmbeddedDaemon` runs cmnwlth in-process; runtime inference
is wrapped in `InferenceRouter`, which OICP-routes synthesis to peers when
scoring favours them. Both sides share `oicp_select`, so the Joiner's selected
model and the Founder's served slot cannot drift. Skills with
`privacy = "local_only"` short-circuit to local.

**Desktop attach mode** — both the desktop and `svrn daemon` want :9741. The
desktop probes `/v1/models` and on success enters Attach: inference through
`RemoteApiProvider`, mesh mutations over HTTP, `save_config` POSTing
`/v1/admin/reload`. **Boot is gated on identity, not on a port** —
`ClientListener` is a watch (`Pending` / `Bound` / `Failed`) and
`/status.process` carries `pid` + `run_id`, so a caller can ask WHO answered.
A fixture daemon that loses the port, keeps running and logs success used to
probe green while the app ingested into the operator's real daemon.

`/v1/admin/reload` rebuilds only what changed: the three model slots swap
atomically via `ProviderFactory`; `client_port`, `internal_port`,
`client_bind`, `client_token` and `data.dir` answer `restart_required: true`.

---

## 7. Build, test, run

Prerequisites: Rust stable, `cmake` (llama.cpp), `protoc` (LanceDB). For
cmnwlth, `llama-server` + `rpc-server` on `PATH`. For desktop, Node.js +
Tauri 2.

The repo is **one unified Cargo workspace** — every crate a member under the
root `Cargo.toml`. `sovereign/`, `commonwealth/` and the corpus-engine
carve-outs are directories of member crates, not separate workspaces.

```sh
cargo build --workspace                    # bundled assets copied via build.rs
cargo check --workspace --all-targets      # what CI's `check` job runs

# The CLI spans 4 binaries — rebuild all of them, since editing one and
# rebuilding only the dispatcher is a silent no-op:
cargo build -p sovereign-cli -p sovereign-cli-daemon \
            -p sovereign-cli-dev -p sovereign-cli-llm
```

For local deployed-daemon iteration use `scripts/dev-release.sh` rather than
plain `--release`: same opt-level, LTO and codegen-units overridden via env,
so a one-line change costs seconds instead of ~7.5 minutes. A custom cargo
profile cannot do this — `llama-cpp-sys-4`'s build script panics under any.

**The gate is the two scripts**, not bare cargo — they resolve the repo's real
feature contract (`corpus-engine/treesitter` + `sovereign-cli/dev-tools`, plus
`sovereign-mesh/mesh-sim` on the lint side) and carry guards bare cargo has no
equivalent of.

```sh
<<<<<<< HEAD
./scripts/sovereign-lint.sh --human [--full]   # scoped to your diff, or the workspace
./scripts/sovereign-test.sh --human
=======
# Sovereign desktop
cd sovereign/crates/sovereign-desktop && npm install && cargo tauri dev

# Sovereign CLI — user-facing surface is `sovereign <verb>`,
# dispatching into one of four binaries. Build all four for the
# full surface, or just the dispatcher for delegator-only edits.
cargo build --release \
  -p sovereign-cli -p sovereign-cli-daemon -p sovereign-cli-dev -p sovereign-cli-llm
target/release/sovereign --help                # via the dispatcher
target/release/sovereign-cli-daemon daemon run # the long-running host

# Sovereign HTTP server
cargo build --release -p sovereign-server
target/release/sovereign-server --config sovereign/sovereign-server.toml
>>>>>>> origin
```

Three scoping levers with different reach: `--package` scopes BUILD and RUN;
`--changed` maps git-changed files to owning crates and falls back loudly to
the full workspace rather than silently under-covering; `--filter` is a
libtest NAME filter that ALSO scopes the build, derived by grepping the
pattern — so a vague pattern degrades to a workspace build. Pass the whole
test name.

**Three guards bare cargo does not have.** A zero-test run is never green
(`pass: 0 fail: 0` exits 4). Unattributable results exit 5, because a
concurrent nextest run overwrote the shared JUnit report. A failed build is a
failure, not a pass.

Executors are `--engine auto|nextest|cargo`. Switching engines changes the
clock, never the coverage: a JUnit adapter emits the same Tier-2 JSONL, the
gate appends a `cargo test --doc` pass because nextest cannot run doctests,
and the JUnit report is deleted before a run so "no report" cannot replay a
stale green.

No tests require GPU, models or network. Sovereign uses
`DeterministicInference` + in-memory SQLite + real FTS5; cmnwlth's harness
runs simulated meshes deterministically.

**`scripts/pre-push.sh` is the primary gate, CI second** — a metered gate that
aborts on a billing failure is nearly indistinguishable from one that passed.
Held to a one-minute budget, it scopes to the diff and runs rustfmt, the
compile, the eight blocking xtask ratchets (docs / arch / boundary / layer /
lock / layout / env / concept) and the desktop node gates concurrently, then
two advisory size ratchets. Install via `scripts/install-git-hooks.sh`, which
points `core.hooksPath` at the version-controlled `.githooks/`. It fails
closed: a push range it cannot diff gates everything.

**A file over its size ceiling is SPLIT, by whoever pushed it over, in the
same piece of work — never re-pinned, never an operator question.** For any
OTHER ratchet failure, `--update-baseline` on your working tree absorbs your
own growth along with everyone else's: re-pin at `origin/main` instead, and
say in the commit body what the lines bought. The baseline diff is the record;
there is no ledger to append to. `--tighten` is always safe.

Concurrent agents serialize on `scripts/with-cargo-lock.sh`: cargo's package
lock makes parallel gate runs BLOCK, and two concurrent nextest runs overwrite
the shared report.

| Port | Service |
|---|---|
| 9741 | cmnwlth/Sovereign client API (OpenAI-compatible) |
| 9742 | cmnwlth/Sovereign internal API (plaintext; network-isolation trust) |
| 9743 | The ring rail's loopback bind (`rail_port(client_port)`) |
| 9743+ | `llama-server` instances |
| 50051+ | `rpc-server` instances for layer shards |
| 8080 | Sovereign HTTP server (configurable) |

---

## 8. Where to look for what

| You want to | Read |
|---|---|
| Understand the agent runtime | `sovereign-core/src/runtime.rs` + `runtime/handlers/` |
| See how plans are executed | `sovereign-core/src/executor.rs` |
| Add a tool | `sovereign-contracts/src/traits.rs`, a file under `sovereign-tools/src/`, a `[[tool]]` block in `sovereign-contracts/tool-manifests/` |
| Run a workflow | CLI `svrn workflow run` → `workflow-host::run_workflow_in_process`; desktop `workflow_commands.rs` → `run_workflow_with_provider` |
| Add a corpus extractor / filter | `corpus-engine/src/extractors/` then register in `engine/ingest.rs`; `src/filters/` + `recipe.rs::FilterConfig` + `filters/loader.rs` |
| Bundle a generated data file | `sovereign-recipes/<corpus>/data/`, append to `corpus-engine/build.rs::BUNDLED_ASSETS`, `include_bytes!` in `filters/assets.rs` |
| Write a recipe | `sovereign-recipes/<id>/recipe.toml`, then `registry.toml` |
| Add an investigation recipe | `enrichment.type = "investigation"` + `[[entity_types]]` + `[[relationship_types]]` + `[[patterns]]` |
| Write a skill / tune models per hardware | `sovereign/modes/<id>/skill.toml`; `sovereign/models.toml` |
| Understand the SCIP call graph | `corpus-engine-scip/` (`scip_graph.rs`, `scip_export.rs`) |
| Classify a symbol / detect trait dispatch | `corpus-engine-scip/src/descriptor.rs` — the ONE decider. Do NOT read `symbols.kind` (88.7% `unknown`) or `refs.ref_kind` (100% `direct`) |
| Find a duplicated concept | IDENTITY `svrn code converge census` / `noun <Name>`; ROLE `converge roles`; SHAPE `converge shape`. Duplicated BEHAVIOUR is `code dry-report`; oversized FILES are `code suggest-seams` |
| Understand index storage on disk | `corpus-index/src/index/mod.rs` |
| Understand the v2 atlas pipeline | [`corpus-engine/ENRICHMENT_V2.md`](../corpus-engine/ENRICHMENT_V2.md) + `enrichment/pipeline/mod.rs` |
| Drive v2 enrichment / build inside the daemon | `sovereign-cli-llm/src/enrich_cmd/`; `enrich_now` (`sovereign-tools/src/local_corpus/atlas_dispatch.rs`) |
| Understand delta updates / scope expansion | `corpus-engine/src/update/delta.rs`, `engine/expand.rs` |
| Understand KnowledgeView | `sovereign-tools/src/knowledge_view/`; injected at `LandscapeDigestProvider::splice_landscape_digests` |
| Understand ATOS lifecycle | `sovereign-atos/src/local/orchestrator.rs` + [`docs/ATOS.md`](./docs/ATOS.md) |
| Run the long-running daemon | `sovereign-cli-daemon/src/daemon_cmd/` + `contrib/launchd` + `contrib/systemd` |
| Serve something the desktop used to compute in-process | the client-router families in `sovereign-daemon/src/*_http.rs` — §5 |
| Prove a deleted twin cannot come back | `scripts/twin-census.py` over `quality/twin-plants.toml` |
| Prove desktop and CLI answer one question alike | `sovereign-desktop/tests/e2e/real/journeys/surface-parity.journey.spec.ts` |
| Trace a `/v1/chat/completions` end-to-end | `commonwealth/docs/routing-field-guide.md` |
| Understand OICP routing | `oicp-types/src/lib.rs` + `sovereign-scheduler/src/oicp_select.rs` + [`docs/inference.md`](./docs/inference.md) |
| Point an outside tool at the daemon | [`../docs/INTEROP.md`](../docs/INTEROP.md); [`../docs/INTEGRATION_SURFACES.md`](../docs/INTEGRATION_SURFACES.md) for which surfaces are contracts |
| Deploy to a shared air-gapped box | `sovereign/deploy/onprem/` — **read `EGRESS.md` before claiming this system makes no outbound connections** |
| Rent a GPU by the minute | [`../docs/CLOUD_PEER.md`](../docs/CLOUD_PEER.md); `scripts/dev-pod.sh`. A `--mesh` flight puts the join link on third-party hardware — end it with `svrn mesh rotate` |
| Know which CLI use cases are promised | `docs/cli-contract.toml` — `[[command]]` the verb surface, `[[journey]]` the sequenced use cases, `[[experience]]` the promises |
| See what the CLI promises and how much can fail | **`svrn contract`** (`map` / `census` / `nightly`). `census` splits the manifest into steps a lane RUNS and steps nothing runs, because a step in a never-run journey is a written intention |
| Judge architecture health at a glance | **`svrn code fieldglass [corpus] --open`** — one deterministic self-contained HTML, evidence only: no scores, no gates. [`../docs/FIELDGLASS.md`](../docs/FIELDGLASS.md) |
| Price or execute a refactor | `svrn code refactor plan` / `gate` / `status`; `code suggest-seams <file> --plan` → `cargo xtask refactor-apply`; `code wire-check`. Process [`../quality/REFACTOR_FACTORY.md`](../quality/REFACTOR_FACTORY.md) |
| Judge the judgment, not just the code | `gym/comaintainer/` + [`../docs/COMAINTAINER.md`](../docs/COMAINTAINER.md); landing seat `scripts/co-review.sh` |
| Is any quality subsystem's posture stale? | **`svrn posture`** — one table: drift / arch / capability / contract-nightly / watchers / env-gate / bench baselines, each row naming its refresh command |
| Is the resident stack BROKEN right now (not drifted)? | **`svrn quality check [--lane <id>]`** — the curated ~30-minute check. Lanes are DATA in `quality/instruments.toml`; each states its verdict as a `kernel_types::Judgement` on its last stdout line. `--distribute` runs the same selection as work on the `work` ring |
| Did my change regress retrieval / routing / synthesis / enrichment? | **`./scripts/sovereign-ci-bench.sh`** (~2-4h) — the FULL nightly, where drift against committed baselines is judged |
| A bench says regressed — real or noise? | [`docs/RUNBOOK.md`](./docs/RUNBOOK.md) §6 — per-lane noise bands, baseline-age semantics, the legitimate re-mint path |
| Pick the next daemon test to write | [`docs/TESTING_SURFACE.md`](./docs/TESTING_SURFACE.md) |

The serial campaign runner is `scripts/ralph.py` (`run`, `supervise`, `watch`,
`pool`): a typed queue parser over `ralph/STATE.md`, a session layer, and
three explicit FSMs, where every terminal state is DONE, an operator stop, or
an escalation. Reference `scripts/RALPH_LOOP.md`.

### 8.1 Where configuration and state live

Four roots, held together by one rule: **path derivations come from the SSOT
accessors** — `sovereign_contracts::rebrand` (`svrnmesh_root`, `data_dir`,
`projects_json`, `work_atlas_toml`, `drift_dir`, `state_db_path`,
`sessions_root`) or their `sovereign_cli_shared::dirs` wrappers — enforced by
a `clippy.toml` `disallowed-methods` ban on hand-rolled `dirs::home_dir`
joins. The `SVRNMESH_DATA_DIR` override applies INSIDE `svrnmesh_root`, so
every accessor above it moves together. Env overrides are declared in
`quality/env-flags.toml`, enforced by `cargo xtask env-gate`.

**Committed contracts (versioned, reviewed):**

| Surface | What it declares | Writer |
|---|---|---|
| `quality/ARCH_LAYERS.toml` | crate layer map, exceptions, package boundaries | humans |
| `quality/env-flags.toml` | the env-knob registry | humans |
| `quality/baselines/` | shrink-only ratchet baselines | **machine only** |
| `quality/CONCEPTS.toml` | the concept register — one canonical owner per noun | humans |
| `quality/TARGET_ARCHITECTURE.md` | the noun-convergence destination; four regions generated | `cargo xtask target-arch`; humans for the prose |
| `quality/source-tree.toml` | the residual "not our source" dirs a gate walk must skip | humans |
| `quality/requirements.toml` | the 625-requirement registry, carrying `spec_hash` | **machine only** |
| `quality/requirements-enforceability.toml` | the ONE hand-authored column: how each can be settled | humans |
| `quality/conformance/<crate>.toml` | which test claims each requirement, from `covers:` tags | **machine only** |
| `quality/instruments.toml` | every instrument and every trigger venue | humans |
| `docs/cli-contract.toml` | CLI verbs, journeys, experiences | humans |
| `models.toml` | model selection per hardware | humans |
| `../sovereign-recipes/registry.toml` | recipe registry | humans |
| `../clippy.toml` | lint budgets + the path-SSOT ban | humans |

**Repo-local `.sovereign/`:** `project.toml` + `project.json`, `sovereign.toml`
(per-repo daemon/watcher posture — watchers deliberately off in this repo),
`notes.db`, `mesh.db`, `features.db`, `SOVEREIGN.md`.

**Per-user root `~/.svrnmesh`:** `config.toml` (`SetupConfig` — THE per-user
config), `work-atlas.toml`, `projects.json`, the indexes / drift / arch /
capabilities / sessions trees, plus models, corpora, recipes and logs,
`daemon.pid` and `worker_owner_key.bin`.

**`[models]` is optional.** Absent — or present naming no primary — plus a
`[node] entry` is `NodeClass::Terminal`: a full mesh member holding no weights
that forwards what it cannot do to a named entry node. The class is DERIVED by
`SetupConfig::node_class()`, never stored, and judged on CONTENT via
`ModelsSection::is_populated()`. A terminal plans zero VRAM slots, registers
no local models, and advertises **no embed model either** — probing would
publish the entry node's model as its own. Two accessors keep that honest:
`local_embed_model_id()` (where this node's text lands) and
`advertised_embed_model_id()` (what it offers peers, `None` on a terminal).
The bind is a mesh IDENTITY, and it decides chat as well as embeddings.

**Platform data dir `~/.local/share/svrnmesh`** holds the mesh identity,
deliberately platform-native so desktop and CLI share it: `node_id` +
`node_key` (mesh-independent, surviving every leave and switch), `active` (the
hex `MeshId` currently live), and `meshes/<mesh-id-hex>/` — one directory per
membership. A node can belong to many meshes and is active in exactly one; a
parked mesh keeps its roster and `mesh_secret`, so switching back is a RESUME,
not a join. **Writing a mesh does not make it active** — `persist::save`
writes the directory and touches nothing else; `save_and_activate` is the
two-step, used only by `create_mesh` and `join_mesh`.

---

## 9. Glossary

- **OICP** — Open Inference Capabilities Protocol. A model publishes one
  `CapabilityClaim` per kind-of-work it does well; schedulers score requests
  against claims. **CapabilityHint** is a validated tag: `general`, `code`,
  open vocabulary via `x:<tag>`.
- **Recipe** — a TOML describing how to ingest one corpus end-to-end.
  **Registry** is the catalog at `sovereign-recipes/registry.toml`.
- **DocumentFilter** — trait between extract and chunk that drops
  `ExtractedDoc`s by predicate. **FilterPipeline / ScopeMeta** is a recipe's
  filter set plus its hash, which lets a corpus expand in place.
- **Field Model (v1)** — five-phase enrichment analysing a corpus
  holistically. **Domain (v1)** encodes a knowledge field's epistemic
  conventions.
- **Atlas (v2)** — typed atom graph + `Pipeline` + registry + `ExemplarBank` +
  `PhaseCache`, which stamps each phase output with the producing model and
  declines to reuse one written by a different model.
- **SCIP** — Source Code Intelligence Protocol. **Exporter resolution** is
  `corpus-engine-scip/src/tool_path.rs`, the ONE decider for "where is this
  tool", because the daemon runs under launchd with a minimal PATH while
  `doctor` runs in the operator's shell.
- **CodeWatcher** — `notify` watcher; re-indexes modified files and marks them
  stale in the call graph (800 ms debounce).
- **Shard** — an index holding a contiguous chunk-ID range, structurally
  identical to a complete index.
- **Slot** — a model-loading position in `EmbeddedLlamaCpp` (Quick / Main /
  Code / Embed).
- **Skill** — a TOML configuring routing triggers, planner templates, prompt
  overrides, memory rules and OICP requirements for a class of work.
- **Mesh** — a closed trust ring of nodes sharing inference and knowledge,
  joined via a `cwth-XXXX-XXXX-XXXX` key. **Peering** is a trust relationship
  between two distinct meshes.
- **EmbedFn / InferenceFn** — the closures `corpus-engine` accepts from its
  caller. **EmbedModelInfo** (`{model_id, dimensions, pooling, normalization}`)
  is the cross-peer interoperability contract.
- **KnowledgeView** — three-map landscape digest (personal memories, 180-day
  conversation history, institutional notes) spliced into the system prompt
  before each turn. Local-scope privacy is structural, not policy.
- **ATOS** — Agent Task Orchestration System. **Charter** is its spec document;
  committing it is approval. **Drift** is "spec changed since approval" —
  warns next turn, does not block.
- **Ring rail** — the append-only, Ed25519-authored total order per namespace
  carrying mesh state, work and measurements. **Work atlas** is cross-mesh
  peer awareness: `work_in_flight` / `declare_scope` / `release_scope`.

---

## 10. Architecture roadmap

Work intentionally deferred, so the next engineer inherits a todo list rather
than a surprise. A big file or a documented gap without an entry is a bug; one
with an entry is sequenced work. When an entry completes its chronicle moves
to [`HISTORY.md`](./HISTORY.md) and the row is dropped.

**The list of oversized files is `quality/baselines/oversized.txt`, and the
gate reads it every run.** There is no companion document: a record of
exceptions to a rule only ever grows, and the answer to an oversized file is
to split it, not to justify it. `corpus-engine`'s files shrink by carve-out
under `corpus-engine/DECOMPOSITION.md` rather than by a local split, which is
the one case where the fix is somebody else's sequencing rather than yours.

Doc posture: this file states what IS and is gated by `cargo xtask docs-gate`,
which resolves every repo path it cites. The narrative reconciliation above
that — drift, capability-reconcile, check-spec — is mesh-side and advisory.
