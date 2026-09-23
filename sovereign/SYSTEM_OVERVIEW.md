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
shipped default-off or dark, with its flip condition and review-by date), and
[`../quality/SIZE_DEBT_LEDGER.md`](../quality/SIZE_DEBT_LEDGER.md) (the
arch-gate acceptance record).

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

| Frontend | Notes |
|---|---|
| `sovereign-cli` (+ siblings) | Dispatcher. `sovereign <verb>` execs into `sovereign-cli-daemon`, `-dev` or `-llm`. Unix execs (same PID); elsewhere spawn-and-wait. Discovery is `current_exe()`'s parent, overridable per sibling |
| `sovereign-server` | Axum REST + WebSocket, multi-tenant with per-tenant isolation on corpora and documents. Binds `127.0.0.1:8080`; a non-loopback bind with `[auth]` disabled is refused at startup. **Two cargo features, both default ON, drop the surfaces whose safety rests on one operator owning the box**: `dev-routes` (privilege — `/v1/solve`, uploads taking a server-side path, the `/mcp*` routes, `ShellTool`) and `net-tools` (egress — the search tool's web fallback, `web_fetch`, `wikipedia_fetch`) |
| `sovereign-desktop` | Tauri 2 + Svelte 5, rail `Ask · Library · Reflect · Workshop · ⚙`. Layout is token-driven: `app.css` owns the scale and three global primitives (`.page-body`, `.page-measure`, `.page-header`). Do NOT re-declare padding or overflow on an element carrying `.page-body` — Svelte scoping wins silently and clips content with no way to scroll to it |
| `sovereign-mobile` | Thin Tauri 2 client — no local inference, Runtime or corpus. Consumes `sovereign-turn-client` and nothing else on the wire. Named ceiling: the client family has no auth seam, so the phone reaches a DAEMON, not an api-key `sovereign-server` |

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

Federated media and named apps ride that surface: the holder declares an
origin, the viewer asks its own daemon for a loopback bridge URL, and the
acceptor tells the origin WHO is asking by rewriting request heads
(`X-Mesh-Member`/`-Node`/`-Pubkey`, every client-supplied `x-mesh-*` header
dropped first). Responses are a byte copy, which is why `Range` stays
byte-exact. `svrn mesh offers` enumerates the roster, so a neighbour
publishing nothing appears as a ROW carrying that refusal rather than absent.

### Scheduling and orchestration

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

Slot policy is normative in [`docs/SLOT_POLICY.md`](./docs/SLOT_POLICY.md):
call sites declare a `slot_policy::Workload` requirement bundle rather than
free-handing `Speed::` literals. The composed OICP scoring product lives ONCE
in `oicp-types`.

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
./scripts/sovereign-lint.sh --human [--full]   # scoped to your diff, or the workspace
./scripts/sovereign-test.sh --human
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

**A ratchet failure is not fixed by `--update-baseline` on your working tree**
— that absorbs your own growth along with everything else. Re-pin at
`origin/main` and ledger the acceptance in
[`../quality/SIZE_DEBT_LEDGER.md`](../quality/SIZE_DEBT_LEDGER.md).
`--tighten` is always safe.

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

**The live deferral tables and the full acceptance ledger are
[`../quality/SIZE_DEBT_LEDGER.md`](../quality/SIZE_DEBT_LEDGER.md).** They
left this file because they are an append-only record keyed to the arch-gate
workflow — the gate tells you to add a row, and the rows accumulate forever —
which is a ledger's job and not a map's. `quality/` is where the gate
baselines already live.

When `cargo xtask arch-gate` reports a NEW oversized file, add a row there and
re-baseline, or split the file. Three standing classes live in that ledger:
**Sovereign deferrals** (per-file split debt), **corpus-engine deferrals**
(files that shrink by carve-out under `corpus-engine/DECOMPOSITION.md` rather
than by a local split), and the dated **size / fan-in acceptance** rows, each
naming what the lines bought.

Doc posture: this file states what IS and is gated by `cargo xtask docs-gate`,
which resolves every repo path it cites. The narrative reconciliation above
that — drift, capability-reconcile, check-spec — is mesh-side and advisory.
