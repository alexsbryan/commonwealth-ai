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
shape came to be) and [`DEFAULTS_LEDGER.md`](./DEFAULTS_LEDGER.md) (everything
shipped default-off or dark, with its flip condition and review-by date). The
arch-gate acceptance record is the baseline diff itself — see
`quality/baselines/`; there is no separate ledger file.

**This file has a ceiling and it is deliberate.** It reached 10.4k lines by
accreting an incident log inside a map — single table cells ran three thousand
words — and at that size it was sampled, never read. Add a fact here only if a
reader needs it to ORIENT. If they need it to ACT, it belongs in the detail
file or the subsystem's own doc.

---

## 1. The programs

```
commonwealth-ai/
├── svrn/             # The assistant and knowledge server: daemon, turn, tools, CLI; its docs and modes
├── serve/            # The model server: inference, the compute child, scheduler, serving host; container images
├── cmnwlth/          # The endpoint router with a roster: the mesh crates and cw-rails; the OICP spec and its conformance tester
├── ingest/           # The recipe pipeline: corpus-engine (LanceDB + Tantivy), recipes, enrichment, the understanding atlas
├── code/             # Code intelligence for agents: SCIP tools, notes, archaeology, watchers, next-edit, the TDD solver
├── bench/            # The evaluator: its crates, and the lanes under bench/lanes/
├── shared/crates/    # The leaves every program may link: oicp-types, kernel-types, sovereign-contracts, corpus-index, …
├── clients/          # Not programs: desktop, mobile, studio, chat-ui, vscode, landing
├── distributions/    # Composition roots — stock, onprem, hosted-ingest — and the on-prem deploy kit
├── quality/          # The layer map, gate baselines, xtask and arch-layers
└── docs/             # This file, ARCH_PRINCIPLES and the other repo-wide docs
```

Also at the top: `scripts/` (the build and test wrappers), `vendor/` (pinned
`llama-cpp-4`), `research/` (cross-program spikes and captured runs), `gym/`
(the next-edit and FIM case banks, the comaintainer seat's charter) and
`ralph/` (the work loop's queue and decisions). Downloaded GGUF weights sit
in a gitignored models directory under sovereign/, the one thing left there.
`quality/xtask` holds the gate binaries plus the workspace-hygiene
generators in `xtask/tests/`, which live there because they read the repo
root and so cannot sit in a liftable crate.

Where a crate lives is a gate, not a convention. `[crate_dirs]` in
`quality/ARCH_LAYERS.toml` puts a `[[package]]` row's crates under its
program's directory, leaves under `shared/`, distributions under
`distributions/`, clients under `clients/` and tooling under `quality/`;
`cargo xtask boundary-gate` fails a member outside its row's directory, and a
member no row claims.

| Program or leaf | Role | Depends on |
|---|---|---|
| `svrn`, `serve`, `cmnwlth`, `ingest`, `code`, `bench` | The six programs (§2) | Each its own crates and the shared leaves: the `[[package]]` closure boundary-gate checks |
| `oicp-types` | OICP wire types + scoring helpers, and the mesh records two programs exchange (node capabilities, the ingest work queue, the contribution/activity ledger, the model catalogue, the inference plan, peer preferences; pb-mesh-exit-core), and `SamplingOverrides`, the sampling pins svrn's turn wire and agent-tools' role profiles share (phase-b-56) | `kernel-types` |
| `kernel-types` | The neutral kernel: identity and provenance (`ContentHash`, `CorpusId`, `NodeId`, `Origin`, `Custody`, `Attribution`), the trust vocabulary (`Verdict`, `Reason`, `Freshness`, `Judgement`), the released turn (`Seal`, `Citation`, `Draft`, `Answer`, `PeerAnswer`, `Refused`), the wire-form decider, the requirement registry. The SECOND layer-0 membrane beside `oicp-types`: oicp is what a node ADVERTISES, this is what content IS. May name nothing above it | `serde`, `getrandom`, `hex`, `blake3` |
| `workspace-hack` | cargo-hakari feature-unification crate, so a `-p` build resolves what `--workspace` resolves | — |

Dependency direction is one-way. Sovereign and cmnwlth meet at the contracts
seam (`sovereign-contracts`, a shared leaf) and over cw-rails' HTTP doors.
`sovereign-mesh`, the in-process embed that was once the only place they met,
is gone (pb-mesh-dissolve, phase-b-92); the stock distribution composes the
programs in one process through their faces (docs/internal/FIVE_PROGRAMS.md §2c).
No program links another: svrn reaches ingest's engine only as the stock and
on-prem distributions hand it in, and cmnwlth links no knowledge crate.

```
   svrn    serve    cmnwlth    ingest    code    bench     each links itself
     │       │         │          │        │        │       and the leaves
     └───────┴─────────┴────┬─────┴────────┴────────┘
                      shared/crates
   distributions/ compose programs in one process, through each one's face
```

Two protocols cross that boundary. **OICP** is declared in
`cmnwlth/docs/oicp-v0.4.md` (v0.4 extends v0.3 additively; the draft
`cmnwlth/docs/oicp-v0.5.md` adds the evidence extension over it, each part
behind its own feature string), types in
`shared/crates/oicp-types/src/lib.rs`, re-exported as `sovereign_core::oicp` and
`commonwealth_core::oicp` — downstream crates use the re-exports.
**`EmbedFn` / `InferenceFn`** are closures `corpus-engine` accepts from any
caller; each project supplies its own.

`serving-policy-core` sits BELOW the boundary rather than crossing it: `fair_sched`
(fair-share caps, `EtaEwma`, reciprocity, the `SchedCore` the daemon's and
the serving host's admission gates share) plus `pipeline_aliases`. It is a
`[[package_leaf]]` (fp-17): its only in-repo dep is `oicp-types`, and
`quality/ARCH_LAYERS.toml` forbids a dep on `sovereign-*` or `commonwealth-*`
in either direction on the `serving-policy` shim, which re-exports both
modules at their historical paths for the serving cluster's own consumers.

---

## 2. Workspace map

One line per crate, under the top-level directory that owns it (§1;
`[crate_dirs]`, boundary-gate). For detail, read the crate's `lib.rs`;
`svrn/docs/` holds the subsystem deep dives.

Since 2026-10-23 the corpus, evidence and deep-research slice is developed
in corpus-engine-core, and its files here are frozen (`quality/frozen-paths.txt`;
AGENTS.md, "Frozen: the slice that moved to corpus-engine-core"). The crates
below still build here because the daemon, desktop and CLI link them.

### svrn — the assistant and knowledge server

```
svrn/crates/
├── corpus-mcp                     # Thin knowledge host — serve, any OpenAI-compatible endpoint (ingest, pull: svrn-ingest)
├── sovereign-cli                  # User-facing dispatcher — execs into sibling binaries
├── sovereign-cli-daemon           # Lifecycle verbs + setup; links no sovereign-inference: setup's probe/plan exec the stock binary (`--setup-probe`), which owns Windows GPU backend selection
├── sovereign-cli-llm              # Model interaction + heavy retrieval (chat/workflow/govern; svrn's sub-verbs of atlas/enrich/corpus; svrn's white-box bench lanes)
├── sovereign-cli-shared           # svrn CLI shared lib (cli-contract, args, flag surface, lane verdict; re-exports sovereign-cli-base at the historical paths)
├── sovereign-core                 # Traits, runtime, planner, executor, router, memory
├── sovereign-daemon               # The node's host crate — assembly, surface shells, edge, adapters
├── sovereign-grants               # GuestGrant, EphemeralGrantStore, `Scope` — per-turn authorization
├── sovereign-meshapp              # Mesh-app explorer ops — pure path-in/DTO-out lib
├── sovereign-runtime-recipe       # THE recipe that commissions a `Runtime` — all four hosts are on it
├── sovereign-service              # Service installation (launchd / systemd / Windows task)
├── sovereign-store                # SQLite + Postgres + in-memory StateStore
├── sovereign-tools                # Built-in tools (search, knowledge, docs, web, MCP)
├── sovereign-tools-base           # Pure leaf workflow tools (shell/web/chunk/file/json/csv/zip/vector/MCP)
└── sovereign-workflow-host        # Daemon-runnable workflow host + the NL workflow-author bundle
```

Beside the crates: `svrn/modes/` (skills), `svrn/docs/`, `svrn/scripts/`,
`svrn/tests/`, `svrn/sovereign-server.toml`. Baked data lives with the crate
that bakes it: the model manifest in `shared/crates/sovereign-contracts/data/`,
the router exemplar and calibration banks in `svrn/crates/sovereign-core/data/`;
`sovereign-service/data/` ships the systemd unit, launchd plist and Windows
task XML `install_service` embeds.

### serve — the model server

```
serve/crates/
├── serving-policy                 # Re-export shim for the serving-policy arithmetic (tier-0)
├── sovereign-compute              # Supervised compute-child boundary — crash isolation, not parallelism
├── sovereign-gliner               # GLiNER (ONNX) NER — the NER served kind's loader (sovereign-compute `ner`, once per process); port in sovereign-contracts `ner`, chunk adapter in corpus-engine
├── sovereign-inference            # llama.cpp slots, remote OpenAI-compat, hybrid, idle residency
├── sovereign-scheduler            # Serving's pure tier — ranker, decision records, replay ("The two tiers", SERVING_BOUNDARY.md)
├── sovereign-serve                # `serve`, the model server binary — the OpenAI wire alone (no mesh, no cw-rails) over the one serving assembly
└── sovereign-serving-host         # Serving's host tier — peer_inference, admission, turn_admission, entry_endpoint
```

Beside the crates: `serve/container/` (the cloud-peer and worker
Containerfiles), `serve/SERVING_BOUNDARY.md`, and `serve/.cargo/` (the HIP
`-fPIC` env llama.cpp's ROCm build takes when cargo runs from there).

### cmnwlth — the endpoint router with a roster

```
cmnwlth/crates/
├── commonwealth-core              # The mesh roster, gossip auth, clock, ledger aggregation; its records re-exported from oicp-types
├── commonwealth-discovery         # Founding + joining: join keys, mDNS, local hardware survey
├── commonwealth-media             # Federated media — who offers a library, who may reach one
├── commonwealth-rail              # The ring rail's JOURNAL — one JSONL log per namespace
├── commonwealth-rails             # `cw-rails` — the minimal daemon a shim author installs
├── commonwealth-state             # MeshStore — KV (pure-Rust in-memory; SQLite file store behind `sqlite`); a local PROJECTION of the ring rail
├── commonwealth-transport         # PeerTransport seam — (peer, traffic class) → endpoints
├── commonwealth-work              # The WORK PLANE on the rail — WorkAct codec, unit seal, lease predicate
├── oicp-conformance               # Standalone OICP v0.4/v0.5 host conformance tester; each v0.5 check watched red against a host fake (src/fake_host.rs)
├── sovereign-cli-mesh             # cmnwlth's verbs — mesh (incl. `mesh pod`), ring, job, publish, run
└── sovereign-pods                 # Compute's remote isolation — leasing a rented machine; `sovereign-pod-worker`, the pod's worker-mode binary
```

The eight `commonwealth-*` crates are the substrate; `commonwealth-rail-core`,
the rail's fold, is a shared leaf. Six left the family in 2026-09 because
their names described a family they were not in: `commonwealth-api` and
`-inference` became `sovereign-api` / `sovereign-serving` and were then
deleted; `-knowledge` became `sovereign-grants`; `-app` became
`sovereign-meshapp-registry` (deleted as dead code, pb-meshapp-apps);
`-test-harness` became `sovereign-mesh-test-harness` (since deleted);
`oicp-conformance` moved to a repo-root sibling, then back here and into
cmnwlth's package with the top-level-programs move. Beside the crates:
`cmnwlth/docs/` (OICP v0.2 to v0.4, and the v0.5 draft), `cmnwlth/apps/` (ring-doc,
ring-runtime), `cmnwlth/deploy/mesh/`, `cmnwlth/BOUNDARY.md`.

### ingest — the recipe pipeline

```
ingest/crates/
├── corpus-engine                  # Knowledge layer (LanceDB + Tantivy)
├── sovereign-authoring-harness    # Recipe-authoring verdict ladder over harness StageOutputs
├── sovereign-enrichment-build     # The enrichment orchestrator, outside the inference stack
├── sovereign-enrichment-catalog   # The enrichment store below every host that reads it
├── sovereign-pipeline             # Pipeline driver (recipes, worklist); pods moved to cli-mesh; `svrn-ingest`, ingest's one CLI (enrich/corpus/atlas/meta-atlas/recipe/pipeline/alignment, `bench atlas`)
├── sovereign-recipe-author        # Recipe-authoring tool bundle + RecipeProject model + project store (svrn reaches them through the contracts RecipeProjectPort)
├── sovereign-recipes (corpus-engine-recipes) # Canonical recipe TOMLs + catalog (the corpus-engine-recipes data crate)
├── understanding-atlas            # Understanding's pure tier — arithmetic over the published language
└── understanding-host             # Understanding's host tier — the ports and the knot
```

**corpus-engine.** Between "raw source on the internet" and "ranked search hits with provenance."
See [`ingest/crates/corpus-engine/README.md`](../ingest/crates/corpus-engine/README.md),
[`ENRICHMENT.md`](../ingest/crates/corpus-engine/ENRICHMENT.md) (the umbrella reconciling
all three enrichment systems — read it before assuming "enrichment" means one
thing), [`ENRICHMENT_V2.md`](../ingest/crates/corpus-engine/ENRICHMENT_V2.md),
[`ATLAS.md`](../ingest/crates/corpus-engine/ATLAS.md),
[`DECOMPOSITION.md`](../ingest/crates/corpus-engine/DECOMPOSITION.md).

- `corpus.rs` — `Corpus`: which corpus, where it lives. The ONE decider of
  on-disk layout; ratchet `cargo xtask layout-gate`.
- `engine/` — `CorpusEngine` façade (`ingest`, `expand`, `reindex`).
- `acquirers/`, `extractors/`, `chunkers/`, `filters/` — pipeline stages.
- `asset_store/` — content-addressed store for binary payloads.
- `recipe.rs`, `registry.rs` — TOML schema + recipe catalog.
- `index/` — LanceDB (IVF-PQ) + Tantivy FTS, `IndexMeta`, `ScopeMeta`.
- `enrichment/` — v1 field engine, v2 atlas, `reconciliation/` (multi-origin
  merge with reversible oplog; signals are identity-grade only).
- `atlas_traversal/` (pure half in `understanding-atlas`) — question →
  `QueryPlan` → `traverse` → brief, no model. Every declared-type question
  runs through ONE executor, `typed::execute`: a `TypedQuery` (target type;
  filters `eq`/`lt`/`gt`/`contains` against a literal, or `eq`/`lt`/`gt` against another
  attribute of the same atom (`{"attribute": ..}`) or, inside a `where`, of the target
  (`{"target": ..}`), either side unread = unjudged; times as signed-year intervals; relation
  constraints over a declared relation type or a `<type>.<attr>` ref, a named
  far end matched by folded name or alias, never a substring; a depth-2
  `where`; `none`/`count`/`argmax`/`argmin`/`tally`, or `sum` of a quantity attribute per
  `group_by` attribute, a ref grouping by the record it holds, an atom with no single readable
  number reported by id and never added as 0) answers with a
  `TypedTable` whose rows carry, per evidence, the installed chunk id that holds its quote
  (`ChunkRef::cited_id`: `index_chunk` when the build placed it, else the section the read
  cited), which the brief prints
  as a cited table with a count line. An unset attribute sets the atom
  aside as unjudged and says so. `svrn enrich atlas-query <corpus> --typed
  '<json>'` runs one as written (`--json`: rows `{name, atom_id,
  attributes, evidence, record}`, `record` the row `atoms_lookup` shows,
  from the one builder `corpus_engine_atlas_reader::records::RecordView`). `typed_prompt::query_grammar` renders a declared
  ontology as the documentation and per-type JSON Schema a model writes one
  against (the query-layer probe's bytes, but for the attribute glosses and the time
  sentence); `AtlasGraph::typed_answer`
  runs the same executor over a loaded graph.
- `update/`, `meta_atlas/`, `freshness.rs`, `pii.rs`,
  `alignment_projector.rs`.

**The wall clock has one decider per dependency island.**
`sovereign_core::time`, `sovereign-time` (the zero-dep leaf),
`corpus_engine_yield::time` and `commonwealth_core::clock` duplicate a
three-line body once each because they cannot import across one another
without a cycle. Everything else asks. Ratchet `cargo xtask clock-gate`,
shrink-only.

**Recipes.** The single source of truth for corpus recipes; corpus-engine vendors the tree
at build time, so there is no second copy. Catalog is `registry.toml` (27
recipes: the `wikipedia*` family, `sep`, `stackexchange*`, `openalex`,
`gutenberg*`, `crs_reports`, `us-code`, `olc-opinions`, `scotus-opinions`,
`federal-register-presidential`, `conversations-anthropic`,
`conversations-chatgpt`, `sec-filings-company`, five `enron-sample*`, three
`uap-blue-book*`). Field reference is `SCHEMA.md`, generated from `recipe.rs`
and gated by the `recipe_schema` test. Outside the catalog: `codebase`,
`arch-principles`, `system-overview`, `chaos-secret-agent`, `chaos-saltgrass`,
`maple-house`, `proxy-company`, `search-gym`, `sf-assessor-roll`.

### code — code intelligence for agents

```
code/crates/
├── code-facts                     # Code-intel package's tree-sitter fact base
├── code-next-edit                 # Code-intel package's next-edit crate
├── corpus-engine-archaeology      # Git archaeology + rough-edges + atom-provenance
├── corpus-engine-notes            # NoteStore + project_docs index
├── corpus-engine-watchers         # Lint/test/project-index watchers + result stores
├── sovereign-agent-bench          # Fifteen-problem agent-coding battery; pi and opencode runners take any OpenAI-compatible endpoint (`--agent-base-url`)
├── sovereign-agent-tools          # Canonical agent-tool primitives (cross-runner contract)
├── sovereign-cli-dev              # Workbench: project lifecycle + code intel + tools; owns the project model (`project init` execs its `project-observe`)
├── sovereign-code                 # Code intelligence served over MCP — the `svrn code` program, lifted out of sovereign-tools
├── sovereign-tdd                  # Unified TDD solver loop (HTTP + MCP transports)
└── sovereign-work-atlas           # Coordination atlas for agents on the mesh
```

`code/inquiries/` holds the principle checks archaeology and `code brief` run.

### bench — the evaluator

```
bench/crates/
├── sovereign-cli-bench            # bench's CLI — bench, eval, quality lane (dials svrn/ingest; links neither)
└── sovereign-eval                 # Pure scorers
```

Fixtures under `bench/lanes/`; orchestrators in `sovereign-cli-bench`'s `bench_cmd/`,
whose turn lanes ask svrn over its turn route (`bench_cmd/subject.rs`) and whose scorers
ask svrn's `__probe` for the grounding gate's own verdicts and judge registers
(`bench_cmd/svrn_judge.rs`); pure scorers in `sovereign-eval/`.

`bench er-score` reports standard clustering metrics conditional on shared mention IDs, and a separate
`recovery_b_cubed` measure using full cluster sizes: omitted gold members lose recall and extra placements
lose precision. Callers scope the evaluation universe; the support-case scorer includes known no-case
placements and reports membership coverage beside the conditional metrics.

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

### shared — the leaves

```
shared/crates/
├── commonwealth-rail-core         # The ring rail's FOLD — Person/Roster/RailAct/SignedOp. Zero I/O
├── corpus-engine-atlas-reader     # Resolved-atlas READ surface (read-only leaf; writes stay in corpus-engine)
├── corpus-engine-scip             # SCIP call graph + per-language exporter dispatch
├── corpus-engine-sections         # Section detectors as a regex-only leaf
├── corpus-engine-yield            # YieldHook cooperative-yield contract (tier-0 leaf)
├── corpus-index                   # Retrieval read-port leaf — CorpusIndex, the IndexSource/CorpusReadPort traits, persisted settings, the engine Error
├── host-kit                       # What each program's binary owns about itself — the single-writer lock on its data root, its paths
├── kernel-types                   # The neutral kernel — identity, provenance, trust, the released turn
├── mesh-join-vocab                # Mesh join-key and deep-link FORMAT vocabulary both ends of a join must spell alike
├── mesh-reach                     # Peer dial vocabulary + PeerTransport; RailsTransport (`rails`), the guest dialer + one iroh HTTP bridge (`guest`)
├── oicp-client                    # OICP pure-HTTP client (OpenAI-compat + manifest routing)
├── oicp-types                     # OICP wire types — no other deps
├── oplog                          # Op/Oplog/Journaled — the append-only JSONL journal (tier-0)
├── quote-align                    # The one quote aligner — norm_v0 + seeded token alignment over `&str` texts, ALIGNER_ID pinned by a golden bank (std + unicode-normalization only); asked by `POST /oicp/v1/align` and, in exact mode, by the post-synthesis quote guard (`sovereign-core::quote_verification`); case-folded, by the span resolver's locator (`native_grounding::span_resolver`) and the citation gate's quote match (`grounding::citation::quote_match`, whose six-word run is read off the aligner's differences). `kernel_types::Seal::locate` stays a plain containment check: it is handed a slice already cut from the member it checks
├── oplog-types                    # The pure envelope (Op/OpId/SkippedLine) split from oplog — zero I/O, the closure rail-core links (2026-09-23, ROOT_CAUSE_FIXES B4)
├── serving-policy-core            # Fair-share scheduling + pipeline aliases ([[package_leaf]] vocabulary leaf)
├── sovereign-cli-base             # Leaf half of the CLI shared set (help, dirs, dispatcher, guest_link, urls, repo, prompts, deprecation, tracing init, models, mcp client; rail client uses the rail-core wire leaf)
├── sovereign-contracts            # The vocabulary — traits, wire types, skills, setup config
├── sovereign-peer-wire            # Wire types both ends of an internal exchange must spell alike
├── sovereign-time                 # Wall-clock helpers — zero-dep leaf for crates off sovereign-core
├── sovereign-turn-client          # THE client half of the turn protocol + reachability (`ServingHost`)
├── sovereign-workflow             # Step·Artifact·Runner — typed dataflow over local-model steps
├── understanding-vocab            # Atlas vocabulary — AtomsFile/AtomEnvelope, Edge, kinds, OntologyPolicies
└── workspace-hack                 # cargo-hakari feature unification — no source, third-party deps only
```

Every program may link these and nothing else outside itself. Each is a
`[[package_leaf]]` with its own pinned budget of internal deps.

### clients

```
clients/
├── desktop/src-tauri (sovereign-desktop) # Tauri 2 + Svelte 5
├── mobile/src-tauri (sovereign-mobile) # Thin Tauri 2 client (iOS + Android) for a svrn host: tailnet, tenant auth, streamed chat
├── studio (sovereign-studio)      # Headless studio CLI (its package's other crates sit with their programs; clients/studio/BOUNDARY.md)
├── chat-ui                        # Shared Svelte chat render surface (desktop + mobile import it as source)
├── vscode                         # First-party VSCode FIM extension
└── landing                        # svrnme.sh: the site, install.sh and the ring guest page
```

Clients are not programs: each is built and prioritised as a client of the
programs' wires. The studio authoring package that `sovereign-studio` headed
is spread across the programs that own its parts; `clients/studio/BOUNDARY.md`
names where each went.

### distributions — the composition roots

```
distributions/crates/
├── sovereign-hosted-ingest        # Ingest's hosting composition for svrn: the one `hosted()` both distributions call (stock hands in recipe authoring, on-prem none); a library listed in BOTH the `stock` and `onprem` `[[distribution]]` rows, so its edges and `src/` answer to each row's faces (phase-b-88)
├── sovereign-onprem               # The on-prem distribution: svrn with serve and ingest hosted, ONE process; no code, no mesh, no recipe authoring, and `Posture::Sealed` withholds web reach, the wikipedia bundle and the `/mcp` route by name, each sealed absence pointing at `/v1/conversations` (`Posture::code_pointer`, `NO_MCP`), and a turn with no web-reaching tool drops the prompts' web offers (`runtime/web_reach.rs`) (`[[distribution]] onprem`; phase-b-86, -87)
└── sovereign-stock                # The stock distribution: svrn with serve, code and ingest hosted (ingest's engine built by `corpus_engine::face` in `sovereign-hosted-ingest` and handed in through `HostedIngest`; svrn links no corpus-engine), ONE process, what `svrn daemon run` execs (`[[distribution]] stock`); its second bin `sovereign-cli-llm-stock` hands cli-llm the same ingest composition and is what the dispatcher execs for the LLM verbs (bare `sovereign-cli-llm` names ingest absent)
```

Beside the crates: `distributions/deploy/onprem/`, the on-prem kit.

### quality

```
quality/
├── arch-layers                    # Schema + evaluator for ARCH_LAYERS.toml — one parser for layer-gate, boundary-gate and arch_report
└── xtask                          # The gates (`cargo xtask <gate>`) and the workspace-hygiene generators
```

`ARCH_LAYERS.toml` is the declared layer map, enforced by `cargo xtask
layer-gate` (Cargo-declared edges) and the code-intel arch report
(SCIP-observed edges); `arch-layers/` is the shared evaluator both use.
Its `[crate_dirs]` table says which top-level directory each member lives
in: a `[[package]]` row's crates under the program's own directory, leaves
under `shared/`, distributions under `distributions/`, clients under
`clients/` and tooling under `quality/`. `cargo xtask boundary-gate` fails a
member outside its row's directory, and a member no row claims.
The test-only `quality/arch-layers/examples/agent_admission.rs` admits managed
dependency proposals after candidate-bound checks. Its `--episode core-read`
observes historical source, probes the existing interface, and checks a typed
extension artifact with the compiler and package policy before receipt-bound
acceptance. This is a source-bound compiler projection, not an engine runtime
proof. Controls live in `quality/arch-layers/tests/agent_admission.rs` and its
core-read sibling; it adds no product API.
`baselines/` holds machine-written ratchet baselines, regenerated only via
`--update-baseline`, banked via `--tighten`. `cargo xtask quality` runs every
fast local gate with one table carrying FOUR verdicts: passed / failed /
could-not-judge / never-ran.

`twin-plants.toml` + `scripts/twin-census.py` are the sabotage runner for the
one-decider censuses: prove the census green, apply a real second
implementation, require a FAIL naming the expected substring, restore
byte-for-byte. 19 families. They prove named censuses; `cargo xtask
clone-gate` is the general detector, a ratchet on production lines covered by
an 8-line normalized window found in two or more files (`baselines/clones.tsv`).

Also here: `CONCEPTS.toml` (the concept register), `TARGET_ARCHITECTURE.md`,
`env-flags.toml`, `requirements.toml` + `requirements-enforceability.toml`,
`instruments.toml` (every instrument in the repo, one table), `DOMAINS.md`,
`DELETION.md`, `CLEANUP.md`, `REFACTOR_FACTORY.md`, `REFACTOR_LEDGER.md`.

## 3. corpus-engine — the shared knowledge layer

Both upstream projects use it through the same public API; neither knows the
other exists.

```
Acquirer → Extractor → Filter → Chunker → Embedder → Index
                                          (caller-supplied EmbedFn)
```

| Stage | Built-ins |
|---|---|
| Acquirer | `bulk_download`, `huggingface_dataset`, `local_file`, `http_api`, `web_crawl`, `inline` (the recipe's own `[[document]]` texts, OICP v0.5 §4.1), `custom` (runtime-registered seam) |
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
`svrn/docs/specs/FINANCIAL_CORPORA.md`.

### Storage

LanceDB vectors + Tantivy keyword; one dir per corpus, identical schema for a
full index or a shard.

```
~/.svrnmesh/indexes/<corpus>/
├── _corpus_meta.json        # authoritative metadata
├── chapters.json            # sections + the chunk_ids join
├── chunks.lance/            # each chunk names its stored text (text_sha256, schema v4)
├── documents.lance/         # one record per stored text per document
├── texts/<sha256>           # each document's canonical text, named by its sha256
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

**Stored texts** (ADDRESSED_TEXT §3). Every ingest path — the
main loop, the watched-folder delta, reindex — writes through one writer,
`corpus_index::index::TextWriter::store_document`. A text is
`normalize_content(doc.content)`, the string the chunks are cut from, written
once to `texts/<sha256>` (`Corpus::texts_dir`); its record (extractor, source
id, source sha256 or none for a record inside a file of many, ordinal,
metadata) goes to `documents.lance`, and every chunk carries the name. Reads
(`CorpusIndex::text`, `documents_for`, `documents`) answer a named
`TextAbsence` rather than an empty result: not held; texts not stored (an
index that held chunks before the store began, or a merge with an input that
had none); text not stored (`[index] store_texts = false`). The directory's
lifecycle carries the store: promote renames it, merges union it only when
every input had one (`text_store::carry_texts`), a snapshot captures
`documents.lance` transactionally, removal deletes it. The library digest is
sha256 of `oicp_types::evidence::texts_digest_preimage` over the records
(`text_store::texts_digest`). Published names are `kernel_types::Sha256Hash`;
`ContentHash` stays BLAKE3 and nothing converts between them.

**`[[document]]` blocks** (`recipe_documents.rs`, OICP v0.5 §4.1). A block
carries `name` + `text` (an inline document, read by `[acquire] type =
"inline"`) or `source` (metadata for one file under the source root), never
both; `Recipe::from_toml` refuses anything else. The inline acquirer writes
each text to `_downloads/<corpus>/inline/<name>` and `InlineExtractor` runs
the recipe's `[extract]` over each file alone, stating the result as source id
`name` and `DocSource::Hashed` (the sha256 of the text's UTF-8). `metadata` is
JSON text, validated at load and stored verbatim; `DeclaredMetadata::input`
builds every `store_document` input, and is the one place declared metadata
replaces the extractor's.

**Three readiness questions, three accessors — do not conflate them.** A
directory under `indexes/` is not an installed corpus; an ingest in flight
writes `<corpus_id>-partition-<node_id>/`. `installed_indexes()` answers "is a
writer active" (the resume paths). `usable_indexes()` answers "can I search
this", filtering on `IndexInfo::indexes_built` and tracing every drop.
`corpus_readiness` (`ready` / `building` / `absent`) is what `corpus status`
and `corpus install --wait` share. `corpus install <recipe.toml> --inline`
reads none of them: it registers nothing locally, sends the recipe in the
daemon's published OICP install, refuses it before sending past the
manifest's `max_install_bytes` (v0.6 §2.12), and its `--wait` polls the
published progress (`sovereign-pipeline` `corpus_cmd/inline_install.rs`).

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
enforced once, in cw-rails' gossip merge (commonwealth-rails `self_measure::apply`),
which clamps the free storage it measures to the budget left svrn declares
(`NodeCapabilities.storage_remaining_bytes`, never gossiped); every scheduler reads that one value.

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
  the navigation map; a relation declared with both ends also gets one focused
  Phase-1 call per section per `from` entity (`pipelines/relation_focus.rs`).
  A declaration whose claim kinds all have a force and a `subject` that is a
  source-free type with an `identity_criterion` or a metadata-sourced entity with
  declared identity fields is read document by document, with no switch
  (`OntologyPolicies::reads_documents`; a mix refuses at parse; the retired
  `document_reading` / `document_reader` keys load, are ignored and are named,
  `V1_RETIRED_KEYS`). Metadata-backed subjects bind to existing projected
  entities by exact identity and are never re-extracted. `DocumentRead`
  outcomes, evidence and `subject_local_ref`s travel through `SectionExtraction`
  cache/checkpoints, keyed by the read contract and source context; generic
  questions are skipped on this path, and accountable source identity and
  subject fields flow to RESOLVE. A claim carries its subject's readings whole
  (`SUBJECT_FIELDS_ATTRIBUTE`, each a `DocumentReadField` like its own fields),
  RESOLVE keys on their supported values and keeps them on the claim, and the
  projection sketches no subject (RESOLVE alone makes those records). The window reader is the one
  reader (ONTOLOGY_METHOD §Reading; the default from 2026-10-15, the passes reader and its waves deleted
  2026-10-16, `SOVEREIGN_READER` retired with them). It reads each
  chapter in one call (`document_read/window.rs`, prompt `window_prompt.md`), and `enrich init` then cuts the
  corpus into windows: every document in declared-clock order, cut by size only where a document ends
  (`sovereign-enrichment-build/src/window_manifest.rs`, `WINDOW_CHARS`, 20,000 characters since 2026-10-17, or the recipe's `window_chars`: a call
  writes about as many statements whatever it is shown, so the size sets coverage and read-to-read agreement, not
  only cost). The window reader names each
  particular once across its chapter; its envelope says so (`DocumentRead.subjects = Chapter`), the projection
  carries the reference (`CHAPTER_SUBJECT_ATTRIBUTE`), and RESOLVE links a chapter's statements by it as a
  sufficient key (`resolution_records.rs::CHAPTER_SUBJECT_KEY`), deciding only across windows. The prompt lists
  statement kinds and types under their own headings and the answer shape names each set's members; a
  particular's field is written once per stating document (`[{value, quote}]`), and each claim reads the entry
  its own document states (`window_particulars.rs::own_entry`), so `agree` and dated folds see every reading. A malformed value
  in the answer costs at most the element holding it (`window_answer.rs`): a bare fraction (`.02`) is read with
  its leading zero, and if the answer still does not parse each particular and claim is parsed alone, the ones
  that fail (and the claims about a failed particular) dropped and counted as `lost`; a claim key outside the
  claim's envelope (the keys `window_prompt.md` gives a claim, read from it) is a field written beside `fields` and
  is read in it, and so is each key of a `grade` written as an object (a grade is one of its kind's grades, never
  an object). The answer shape offers a claim's `grade` (`window.rs::GRADE_SLOT`) only when a kind the window reads
  lists grades; the envelope keeps it either way. A reference value resolves
  in `window_refs.rs` (label name, candidate, words held), by any of a particular's names when the answer lists
  `other_names` for it (a name a document says it also has; typed, never a delimiter, so "Input / Output" is one
  name; `window_particulars.rs::names_for`, also used for a subject's sourced identity and for placing an
  unstated particular, which is sent as its own statement on every document of the window whose own lines
  name it, 2026-10-20, so a reference in each of them finds it). Each statement carries who says it as the answer's `said_by` read it (the document's
  words, or none for its own voice; `window_voice.rs`, kept on `SAID_BY_ATTRIBUTE` and shown by the
  record row with its grade, `corpus-engine-atlas-reader/src/records.rs`), and the other names of what it names, typed by what it refers to
  (`OTHER_NAMES_ATTRIBUTE`), which RESOLVE shows beside the subject's name and the readings' values
  (`resolution_records/described.rs`, never compared) and `read_fields.rs::record_read` lands a reading by, its
  value first; a listed record answers its identity. Two particulars the reader
  read in one document stay two: a record holding one is never offered the other, folded or joined earlier in the
  same document (`resolve_records/apart.rs::read_beside`, tracked in `select::choose`); a record the same document
  opened is weighed by every window reference that has joined it, not its opener's alone, as the fold's recheck
  (`gate_plans`) weighs it, so a window-mate opens its own record instead of being refused (code enforces this; no
  question line restates it); and a statement follows its
  own particular, joining by key the record that already holds its reference whatever sibling a question let it
  share (`apart.rs::chapter_conflict`). Every veto event names its conflict. Each document in a window opens
  with its declared facts: the fields the declaration stamps or its metadata sources read
  (`window.rs::shown_fields`) and, as `Named here:`, the records those sources name of each type a read
  reference targets (`pick.rs::candidates`, read through the projection's own field readers,
  `resolution_sources/fields.rs`). A line carried from a document dated
  earlier, shown by structure (a passage of neighbouring lines that document
  holds in order, or its line under a mark it lacks there), is never shown as the document's own;
  a line that only repeats another's words is (`line_classes.rs`, indexed
  once per run over every chapter's documents and part of the section cache
  key). Two more classes come from what the extractor wrote, never from the
  words: a header line before the document's `body_start` and a line inside
  one of its `segments`, an embedded reply or forward
  (`extractors/email_segments.rs`, one segment per message of a history). Each embedded message is a
  document of its own since 2026-10-17 (`extractors/email_embedded.rs`): its own lines, dated and authored
  by its own header (`date_basis`: `header`, or "before its carrier" with why, never the carrier's date as
  if read), keyed by who said what so one carried by several replies is one document, and marked
  `embedded_in`; its carrier's segment names it (`message`). `LineClasses` gives every line one owner: a
  stored document never yields a line to an embedded one, an embedded one yields to any stored document
  holding the line as its own text and else to an embedded one dated before it (ties by key), and a
  segment's lines point to that owner. Before this, an embedded message no stored document held was read
  nowhere (ward-story tep/10., note f9142d79). The window reader shows, in place of each run of lines a document quotes
  from an earlier one, a pointer to that document (`[quoted from d3]` in the window, its date outside it;
  self-describing, so no prompt line explains it or assumes the document is a reply;
  `document_read/window_text.rs::shown_text`, `LineClasses::sources`), so a reply's referent is read where it
  was written while the quoted text stays unshown and uncitable: a claim whose quote is in the lines its
  document quotes or embeds and not in its own is refused by name (`window.rs::envelope`, `quoted_text`;
  counted as `carried` on the chapter-read event), a guarantee code makes, not the prompt. A claim's quote
  is widened to its sentence only within the run of own lines it stands in (`window_text.rs::own_text`
  leaves a blank line where lines were removed, `window.rs::sentence_of` stops there), so the evidence code
  composes is a passage of the body validation checks. Both are offsets into the canonical text
  the reader reads: `SourceDocument::raw_body` prefers the text store's text
  (`corpus_io::document_texts`, attached by `SectionDocuments::with_texts`),
  so no chunk title or overlap is read; a document the store cannot give is
  read from its chunks with those two classes not applied, and the trace says
  so. A particular no statement is about is carried on the outcome (`DocumentReadParticular`; validation
  refuses one of a type RESOLVE does not decide or cited off its document) and projected as a claim of no
  kind whose type rides on `ABOUT_ATTRIBUTE` (`document_read::about_type`, read by 3b's subject binding,
  which leaves it to RESOLVE, and by `resolve_type`, which makes it a statement of the type), so a document
  is a member of what it is about whether or not a line states a claim kind; the marker is removed after
  RESOLVE. Every read value carries `reader_window` at `Precision::Unmeasured`. The answer assembles into
  the `DocumentRead` envelope that validation checks against the document (exact source verification is
  mandatory) and the section cache keeps; the prompt and the ontology's rendering are part of the contract
  fingerprint (`schema::contract_value`). The one-shot reader is deleted.
  The Asker (`enrichment/asker.rs`, ONTOLOGY_METHOD §Reading, campaign C5)
  stands in front of the chat and embed ports of `enrich extract`,
  `atlas-resolve` (and `enrich build`, `delta`, `resolve-statements`), built in
  one place (`DaemonInferenceClient::into_asked_closures`): `--asker daemon`
  (default) records every answer in `enrichment/<id>/answers.jsonl` keyed by the
  hash of the whole prompt and per-call budget (or the text embedded), one answer
  per question per run, each stamped with the model the client routes it to
  (`AnsweredBy`), an alias stamped as the model the daemon's `/v1/models`
  binds it to (`corpus_index::v1_models::alias_bindings`; `daemon` and
  `recorded` refuse to start when the daemon does not answer it); `replay` answers from that store and never calls (or probes)
  the daemon; `recorded` answers from it what the run's own model gave and asks
  the daemon only the rest, recording them, so an iteration pays for the
  questions it changed; `gold` reads `answers.gold.jsonl`. Under `replay` and
  `gold` a question with no stored answer is refused, never defaulted.
  `change.document` names per-document metadata fields; resolution stamps each
  claim with `document_date` (ISO 8601), `document_thread` and `document_id`
  from its ONE document (`locate`: the document its evidence names, stamped from
  the read that cited it, else the one its anchor lands in; equal words in a
  second document never make a second candidate)
  (`enrichment/atlas/resolution_documents.rs`, rows from
  `corpus_io::section_documents`), and records a claim it cannot place in
  `resolution_failures.json` instead of guessing. Last, `place_index_chunks`
  (`enrichment/atlas/resolution_placement.rs`, every recipe, stamped or not) gives every
  evidence the installed chunk holding its quote (`ChunkRef.index_chunk`, the u64 the
  corpus chunk endpoint opens; the first in manifest order when overlap holds it twice); a
  record projected from document metadata cites its document's first chunk, and evidence
  it cannot place keeps its section and is counted by reason. An entity type's
  `source = { metadata = [...], attributes = {...} }` (`MetadataSourceDecl`)
  projects one atom per distinct identity value read from those same rows'
  fields through a closed reader set (`address`, `domain`, `display_name`,
  `value`; `enrichment/atlas/resolution_sources.rs`), provenance
  `SignalKind::DocumentField`, id `exact_entity_content_hash` of the value; in
  Phase 3a the reconciler's strict `ExternalIdSignal` names the model atoms
  carrying that value and the resolver's `merge_into_existing` folds them in,
  counted on the resolve step's output. A source's `refs` (`SourceRef`) link a
  declared `ref` attribute to another sourced type's atom by the identity value
  a reader reads on the same mailbox (Contact -> Account: `employer = { of =
  "company", reader = "domain" }`), linked after every type is projected;
  attributes merge first-wins, so a model's raw name never displaces the link.
  A source's `exclude` (`resolution_sources/fields.rs::Exclusion`) skips an
  identity value equal to, or ending at a word boundary in, a value it names
  or one of a `@bundled:<key>` list it names (`@bundled:mailbox_providers`,
  free-email-domains at a pinned commit), counted as `excluded`; code holds no
  list and skips nothing a recipe does not name (E5, 2026-10-10: the provider
  skip that was code is now this declaration). The window reader's declared facts and candidates
  read fields through the same readers (`field_records`).
  RESOLVE (`enrichment/atlas/resolve_records.rs`; ONTOLOGY_METHOD.md §The core)
  puts each statement of one document into an open record of one declared
  type, opens one, or holds it. An equal declared `identity` key decides without a call.
  Every declared document field (a `change.document` stamp, read by
  `read_stamp`, the one reader claim stamping uses too) is compared between a
  statement's document and each record (`resolve_records/fields.rs`), and a
  record holding the document's value is an alternative. Under a partition
  answerer the fields alone are weighed and link where they reach
  `identity_bar`; otherwise ONE grammar-constrained call per document, given the type's
  `identity_criterion`, the candidates the proposers offered
  (`resolve_records/propose.rs`: the records the document's fields name,
  the records of its declared thread, `change.document.thread`, the `k`
  records nearest each statement by the run's embedder (`Nearest`: a
  statement's words and surroundings against each seeded record's card and
  each opened record's opening statement, `resolution_records/nearest.rs`;
  `k` a cost knob), then TF-IDF over documents already resolved; each
  candidate shown with its `Reason`s, its statement and document counts, what
  it is as written (a seeded record's identity values, name and the records
  whose refs point at it, `mentions::card`) and the values its statements
  were read with and what its statements' readers read it to be (`Described`:
  the names a window read gave it and their supported values of its other
  declared attributes, folded; shown, never compared), never its folded comparison keys or its documents' stamps,
  and up to four of
  its statements' cited lines from other documents (`passage::lines_of`: a
  span widened to its whole line, or the reader's lines as they stand on the
  folded default-path text), one line each marked
  `QUOTE_LABEL` and nothing else of those documents (C2 narrowed 2026-10-10;
  a cite is still checked against the asked document alone),
  `answer::describe`) and the answer threads,
  wording and similarity alone propose (`ProposalRule`), partitions the
  document's statements into particulars, each the same as one candidate or
  none, with a passage per statement code must find in the document.
  `--answer proposed` takes that proposed answer with no call: the zero-model
  floor a model answer is held to, judged and folded by the same code.
  `--answer select` (`resolve_records/select.rs`) asks instead one forced
  choice per statement, in document order, over the shown candidates and the
  records this document opened, read as a distribution in one forward pass and
  kept on the outcome (`Choice`); its argmax decides whenever a record was
  shown, the record it picks (`Decision::Chosen`) or none, which opens one
  (operator delivery review 2026-10-12: on Ward the weighed decider overrode
  36 of the model's 39 picks of an offered company). The asked statement is
  shown beside what its reader read its subject to be, gathered over every
  claim of its window subject (`resolution_records/described.rs`). The default
  path, `enrich resolve` (`atlas_resolve_documents.rs`), asks the reasoned
  choice (`Answerer::Reason`): the model reasons, bounded by `REASON_TOKENS`,
  and its choice is read after (ONTOLOGY_METHOD §Identity, amended 2026-10-14).
  A type's `identity_necessary` attributes (each with declared `values`) are READ
  per asked statement as one forced choice over those values
  (`resolve_records/read.rs`), unless the document reader already chose it
  (`Statement::read`, filled from its subject fields by `resolution_records.rs`);
  a candidate whose SUPPLIED value (a declared field's, `Statement::keys`)
  differs is not offered (`resolve_records/apart.rs`, with the window reader's
  distinctions and the key and field joins that obey both), while a value a
  model chose, the reader's or RESOLVE's
  own READ, is one more weighed source (`necessary:<attribute>`); each
  document's decisions count the necessary values used by origin
  (`DocumentResolution::necessary`: supplied, reader, resolve_read). A partition
  answerer, which weighs nothing, still holds read values as constraints. Every source (each declared field, each necessary
  value, the proposed answer, the model's argmax) agrees or disagrees with each
  (statement, alternative) pair, and is weighed at what it is worth on the
  corpus being read: Fellegi-Sunter agreement weights fitted by EM over those
  pairs with no labels (`resolve_records/estimate.rs`; agreeing is never fitted
  as evidence against, and sources identical on every pair count once), refitted after every
  document in clock order, so a document is weighed at what the documents
  before it estimate; the recipe declares no precision (`identity_evidential`
  is a retired key, warned and ignored). The weighed posterior is kept beside
  every model choice; it decides only a statement no record was shown to the
  model for (a field-named or proposed record alone) and every statement under a
  partition answerer. That decider (`resolve_records/weigh.rs`)
  sums each alternative's weights with the pair prior into its log odds
  against none: one an agreeing source raised and whose posterior reaches
  `identity_bar` links (`Decision::Weighed`, with the sources that agreed);
  with no bar the most probable of the alternatives and none decides; no
  source raising one opens a record; anything between is `Outcome::Held`,
  counted, its alternatives, votes and necessary values kept, never opened as a
  record in its document. After the last document every held statement is
  settled (`resolve_records/settle.rs`, E3, called by `drive.rs` for both
  drivers): weighed again against the records it was held between as they
  stand, at the whole corpus's weights, with the model asked once more over
  those records only (`model_choice` or `reasoned_choice`), whose choice
  decides as in a document; with no model, at the bar it links, with no source
  raising a record it opens its own, and still unsettled the most probable
  decides, so nothing is left held. Each settled
  document reaches `on_document` again as a resolution with `settles`, its
  outcomes replacing the held ones (`RecordsReport::settled` is the held count
  before; `outcomes["held"]` after). The model
  is asked whenever a record is offered. The resolve step
  prints, per source, its estimated precision, weights and the links and
  vetoes it carried (`Resolver::sources_summary`).
  `--answer reason` reads the same choice after the model's own bounded
  reasoning (`reasoned_choice`). `Decision` is `Key | Cited |
  Weighed | Opened`; anything else is a counted `Refusal` or `Held`, never
  defaulted. A link carries `by`, each source with its `Precision`
  (`atlas/precision.rs`: declared, estimated on the corpus, or unmeasured), as
  do the reader's fields (`reader_window`) and each derived value (C3). Records keep the passage around each statement, which is
  what later calls compare. `svrn enrich resolve-statements` runs it alone over
  supplied statements (the gold-mention setting). The atlas build runs it after
  3b, which `enrich atlas-resolve` runs by default (`ResolvePhase::All`; `--phase 3a`
  is the only opt-out) (`enrichment/atlas/resolution_records.rs`, from `atlas_resolve_documents::apply`)
  as the ONE decider of every entity or event type that declares an
  `identity_criterion` and no `source` (`decides`; an event type's records are
  event atoms). When the document reader reads the declaration, a type a read
  `ref` names that nothing else records (an entity or event with no criterion and
  no source), or a base kind a `ref` names and no one declares (`of = "person"`,
  then declared as an entity), takes the one derived criterion
  `resolve_defaults::DEFAULT_CRITERION` where the policies are built
  (`recipe_ontology/resolve_defaults.rs`, 2026-10-20), so RESOLVE decides it by
  name and the references land on its records; `recipe validate` prints each
  (`identity: … → RESOLVE by name`). Under the general extractor a decided type no claim kind names
  as its subject has no statements, so `enrich extract` refuses it
  (`types_without_statements`) rather than leave it to 3a's merge, while the
  document reader asks About of every decided type and the window names its
  particulars, a kind about it or not (2026-10-15): its statements are the claims of each kind whose
  `subject` is the type, placed by their anchor in the one document `locate`
  finds (derivation's `document` step uses the same). Claims the reader gave one `subject_local_ref` in one document are one
  statement at the earliest span (`resolution_records/local_subjects.rs`), unless
  they disagree on a supplied identity value; a ref never joins across
  documents. Documents go in clock order (`resolve_records::clock`, through
  `resolve_in_clock_order`, the one loop `resolve-statements` shares, so input
  order never changes the records) under `Answerer::Select` with the adopted
  proposer (`ProposalRule::default`, `propose::{NEIGHBOURS, MAX_CANDIDATES,
  MIN_SIMILARITY}`); each record becomes an atom whose id hashes its opening
  statement, called by the name most of its statements' readers gave it (a
  tie to the first met in fold order, so one misplaced statement cannot
  rename it), its other read names its aliases and its opening passage its
  first passage (the record view's `text`), else by its opening passage
  (`resolution_records/named.rs`; shown, never compared); an event record's
  evidence is one chunk per distinct document among its statements, in
  statement order, each in the section that statement was read in
  (`resolution_records/evidence.rs`, 2026-10-20; an entity record keeps only
  its `first_appearance`), and
  each claim's subject its statement's record. 3b leaves such a
  subject unresolved, and the type's atoms 3a merged from Phase-1 sketches are
  retired with every reference to them, dropped and recorded, never repointed.
  Decisions land in `atlas/resolve_decisions.jsonl`, and each placed claim keeps
  how its subject was decided under `__resolve_link` (`resolve_link`: key,
  chosen or opened, with the model's uncalibrated choice scores for the decided
  record, the best other candidate and none). Around it run the declared
  derived attributes (`enrichment/atlas/resolution_derived.rs`; ONTOLOGY_PRIMITIVES
  §8): paths over `subject`, `document`, source fields (the `Participants` the
  source projection records) and refs, filtered by sets, combined by a fold from a
  closed registry; claim attributes before RESOLVE, a decided type's after, each
  outcome typed in `atlas/derived_decisions.jsonl` and kept on the atom itself
  under `__folds` (`outcome::write_fold`: every outcome, decided or not, with the
  readings it folded, each cited by statement, document and words; a
  non-decided outcome leaves the attribute unset, so no filter matches it). A decided type's read field
  is folded onto each record first, by its declared `by` or, with none, by `agree`
  (`resolve_defaults::DEFAULT_FOLD`, supplied where the policies are built, 2026-10-20), over what its statements
  read of it, by the same decider (`resolution_derived/read_fields.rs`, entity
  and event records alike); a `ref` reading counts as the record of the
  referenced type that its statement's document names in its fields and whose
  identity values it folds equal to (`key_name`, the projection's own naming),
  is dropped (`excluded`) when that record is in an exclusion set over the type
  (`DerivedPolicy::exclusions_of`, the one `pick.rs` reads too); failing a named
  record it counts as the one RESOLVE put the same document's statements a window read
  under that name on (`SUBJECT_NAME_ATTRIBUTE`, 2026-10-15); failing that, for a metadata-sourced type, the
  one record of it any document's metadata projected with that exact identity value (several refuse, with the
  count; `read_fields::sourced_anywhere`, 2026-10-20), and otherwise is
  never held: it stays on the derivation line under `unresolved`, cited by its
  statement, document, words and evidence, so a served reference is a record of
  its type or nothing; RESOLVE itself writes no record value. A read
  statement's own `ref` field (a claim kind's declared one, a relation's
  `to`) follows the same rule next (`read_fields::held`,
  `resolution_derived/statement_refs.rs`, added 2026-10-15): it holds the
  record its reading names or is removed, its reading kept on the statement
  and an unresolved one on the derivation line. Then each read state or
  relation statement is also a State or Relation atom beside its claim, one
  per statement, and every holder given a state has its trajectory chained
  again by 3b's own pass (`resolution_records/statement_atoms.rs`,
  `resolution/trajectories.rs`, 2026-10-15). When
  `change.document` declares a thread and an id, a record also holds each
  document the run read that states nothing of its type and whose thread's
  statements all sit on it, by declared id under `thread_documents`
  (`resolution_derived/membership.rs`, after the folds); every document read is
  counted by how it came to be held or not, and a document the reader could not
  judge is never placed. **Both tension axes degrade by REPORTING, never by
  enforcing a criterion the extraction did not fill.** The optional `by = "protocol"`
  fold projects a closed text state only after RESOLVE, from cited claims assigned
  through `^subject`. Each rule requires source-supported `DocumentRead` fields;
  absent qualifications and legacy caches without the additive field-evidence
  pointer remain pending. `change.document.date` orders state reports, while a
  configured claim `effective_time` is retained separately and never orders them.
  Undated, mixed-precision or unsupported partial report times remain alternatives;
  incompatible states at the same report time conflict. A protocol whose rules
  declare `by` (a declared path walked from the claim: who may make the move) or
  `from` (the states it may leave, from the protocol's `initial`) is a state
  machine: reports are replayed in report-time order and one no move admits stays
  in history as `unauthorized` or `not_from_state`, evidence and never the state
  (`resolution_derived/protocol_moves.rs`); a path may walk a document field
  backwards (`^author`: the documents a party wrote), so an event's actor takes a
  role from their other posts. A protocol declaring neither folds as before. The existing decision line
  retains rule fingerprint, claim basis, assignment, source document, field evidence
  and history, including duplicates, corrections and superseded reports. Existing
  ref-valued folds keep their semantics. Every pipeline writes
  `atlas/ontology.json`, so a reader can tell an author's declaration from a
  genre writing its fixed vocabulary down; built-in vocabularies are DATA at
  `pipelines/ontologies/<id>.toml`.
- **`tiered`** — three progressive tiers (T1 embeddings → T2 entity-graph +
  PPR → T3 RAPTOR cluster tree). The RAPTOR builder is in
  `sovereign-tools/src/raptor_atlas.rs`, injected via
  `TieredEnrichmentProvider` to avoid a cyclic dep. A summary call the host
  sheds comes back until `SHED_WAIT_CAP` (10 min) instead of taking the
  extractive floor. GLiNER augments the
  conversation path through the `LabeledEntityExtractor` seam. **The NER seam
  is input-bounded** — `BoundedInputs` caps batches at 16 chunks and holds
  back anything over 2,048 chars; an over-cap chunk is REPORTED
  (`refused_over_cap_chunks`), never truncated, and a large count is a signal
  about the CHUNKER, not a reason to raise the ceiling.

**Enrichment's chat host.**
- **One host.** `DaemonInferenceClient` talks to the one OpenAI-compatible host its config names (`base_url`, `/v1`-normalised) through `oicp_client::RemoteApiProvider` built `originating()` (the caller's OICP envelope crosses verbatim and no forward hop is spent), the same body builder, shed loop and response read as the daemon's mesh hop. Model ids go to the host verbatim. A hosted model is the daemon's `[engine]`, not enrichment's: there is no provider registry and no second dialect.
- **Consent.** Every dispatch passes `egress::verify` first. A host on this machine (`oicp_client::endpoint_is_loopback`) is Local; any other, a daemon or a bare server on another host included, faces a gate that is default-deny. The grant is the run's `--consent <class>` (`svrn-ingest ingest`, `enrich build`). It reaches child processes as `SVRNMESH_EGRESS_CONSENT` and is read once, at `DaemonInferenceClient::from_enrich_config`.
- **Structured output** is negotiated with the host (`inference_client/negotiate.rs`): `json-schema` unless its OICP manifest advertises only `json-object` (`oicp_client::StructuredOutputMode`, spelled by `oicp-client/src/chat_wire.rs`). On an `openai-compatible` host, a tool mode is OpenAI function calling, named after the schema's `title`. A host that answers a `json_schema` request with 400 is asked once more with the schema as a forced function call (`RemoteApiProvider::send_chat`); only a retry that answers switches that provider for its life, and only a last-resort endpoint (`waiting_out_sheds`) is asked again: a peer's refusal still goes straight back to the router. That is what DeepSeek needs: it rejects `json_schema` and enforces no schema under `json_object`.

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
verified when the entry declares one. The corpus-engine-recipes crate compiles
`registry.toml` in, so the engine works offline with no checked-in snapshot to drift.

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
must opt in. Enforced by `ingest/crates/corpus-engine/tests/main/recipe_back_compat.rs`.

Delta updates are `update/delta.rs`: per-document revision ids,
`ManifestDiff::compute`, three-phase apply, `_update_progress.json` for
resume. Crawl safety is hardcoded and not per-recipe: robots.txt compliance,
1s/domain rate limit, a declared UA, scope enforced against the seed domain.

---

## 4. Sovereign — the local agent

Desktop, CLI or daemon against the same `Runtime`. No data leaves
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
in `svrn/crates/sovereign-core/data/router/exemplars.toml`. `IntentRow` has no `Default`, so a row
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
context: raw observations accumulate in the worker's local conversation and
only a typed contract — the `return_schema` fields plus an always-present
`anomalies` channel — flows back. `crate::tool_loop` is the crate's ONE
tool-call protocol, and all four loops drive it: the executor's two send a
conversation (`PromptShape::Conversation`, the model's template writes the
call format) and read replies with the native path's parser, `chat_turn`;
recipe_author and the attached-document turn still parse the `<tool_call>`
envelope.

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
`crates/sovereign-core/data/calibration/axes_v1.toml` — a bank authored to fail somewhere
(74 cases, 32 `expect = "abstain"`). Two guards keep it honest: a margin floor
clamped to ≥ 0, and `FitReport::underpowered()` on any axis with fewer than
five cases per class. **The command writes no constant** — it names the
constant and the file and stops.

**Synthesis and the grounding gate.** The knowledge-turn path is organized by
`role.rs` (the resolver returns a `role::Tier`, load-bearing rather than
declarative). `runtime/grounding/` is the production gate: per-claim fail-open
accounting, a concurrent bounded claim fan-out, an audit pass with a plan and
an outcome, a citation stage whose support decider is the gate's own judge,
and a value-presence veto (a value that is a number is found by its value among
a passage's numbers, `grounding::number_presence`, never by its word pieces;
OICP verify's fast tier reads the same check). Every fail-open exit names WHY at one site. Judges
run against ONE register, enforced by `cargo xtask judge-funnel-gate`, and a
register change is priced in both directions or it is not judged. The
forced-choice body itself is built in one place for every subsystem,
`oicp_types::forced_choice::schema` beside its detector, and each caller issues
it through a census (`gate_call` here, RESOLVE's `decision_call` in ingest).
A WAVE (`forced_choice::wave`, advertised `x:forced_choice_wave`, routed only
to hosts that advertise it) carries several closed questions over one shared
prompt as branches; the engine renders each branch as the single request it
stands for, decodes the shared token prefix once, restores that sequence state
before each branch and reads each distribution as a single forced choice does
(`embedded/decide_wave.rs`), so every answer equals its single request's.
No ingest path issues a wave since the passes reader was deleted (2026-10-16).

**Retrieval** is `runtime/retrieval_pipeline.rs`, a step ledger where every
step accounts for what it did to the pool. `apply_atlas_grounding` is a CALLER
of `corpus_engine::enrichment::atlas::ground`, the same walk `corpus-mcp`
drives, so the two cannot diverge. The evidence budget is spent ACROSS the
ideas the walk reached, not down the ranked list. With
`retrieval.pipeline=debug`, content fingerprints track passages through every
step and the KnowledgeQuery handler's subsequent cohesion expansion, rerank,
late summaries and prompt admission; the pipeline's `scope_audit` is not the
final prompt pool. Atlas grounding reports raw request counts separately
from the pipeline's chunk-equivalent injection ledger because one request can
yield multiple passages.
`atom_enum` (opt-in `SOVEREIGN_ATOM_ENUM=1`) answers a list question its
Stage-1 gate calls ENUMERATE; when a corpus in scope declared types, the model
writes a typed query (`atom_enum_typed.rs`, primary slot, schema-constrained)
and the executed, cited table enters the pool as one `source=atom-enum`
chunk, with the degree-ranked entity fetch as the named fallback.

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
[`svrn/docs/inference.md`](../svrn/docs/inference.md).

**Which engine serves this node is config, not code.**
`engine_factory::build_engine(&SetupConfig)` is the ONE place `[engine] kind`
becomes an `Arc<dyn InferenceProvider>`. The vocabulary is one layer down in
`sovereign-contracts`, so an out-of-tree engine can name the selection without
naming any implementation. `llama`, `remote` and `llama-server` are typed
variants; anything else is `EngineKind::Custom(name)` resolved through
`register_engine`, and an unknown id refuses listing what IS registered rather
than falling back. `llama-server` is a router the node supervises: the factory
refuses it by name, because its endpoint exists only once serving assembly has
started the router. Its models come from `[models]` through
`sovereign_compute::llama_router::RouterPreset`, whose `engine_section` is the
remote half the factory then builds (svrn-docs ENGINE_SWAP_PLAN.md, step 1).
Each section sleeps (`sleep-idle-seconds`) on the `[daemon] *_idle_secs` of
the roles it fills, 0 pinning it; a sleeping model is woken by its next
request. Serving assembly starts the router on its first build (`llama_router::live`,
a supervised child), warms the preset models that fit the GPU together
(`capacity::check_fit`, primary first; `--models-max` is their count, the
rest load on demand) until `GET /models` reports `loaded`, and re-wraps it on every reload, refusing one whose preset differs;
`llama_router::provider::RouterProvider` reports it and each model under
`compute_children` and the node's weights as `OwnWeights`, and answers the
host facts from the router rather than the remote half's assumptions: each
preset role's residency (embed and rerank included) from the polled
`GET /models`, the code model, and `n_ctx_train` from the primary GGUF's header.
`[models.edit]` is a section too (pinned; on a GGUF another role serves, that
role's section): `RouterProvider` reports its edit slot with the FIM style
read from the router's `/tokenize` over the marker table
(`sovereign_contracts::fim::fim_style_from`), and sends every `PromptShape::Raw`
request for a preset model to the router's native `/completion` verbatim with
that family's stop strings (`oicp_client::llama_server_raw_completion`), so a
raw prompt is never chat-templated.
`[models.kinds] rerank` is a section too (`reranking = true`, `parallel = 4`:
the route scores one task per document, so its slots are what let documents
overlap). `RouterProvider` names that section on every rerank
(`rerank_model_id`), because the route serves a model per preset entry and
the remote half would name the chat model; a preset with no rerank section
answers `NotImplemented` and retrieval falls back to un-reranked fusion. The
route's `relevance_score` is a probability, and `oicp_client`'s
`rerank_over_route` returns `ln(p/(1-p))` so every caller keeps its
`margin > 0` bar (PPR admission, the rerank/fusion blend, H1's fitted Platt
map) — the ruler the embedded engine's `YesNoLogit` reads. The whole path is
`sovereign-stock`'s ignored e2e `llama_server_router_e2e`: boot on the
shipped binary, serve a completion and an embedding, and rerank through the
route (the rerank model id named, an answering document above 0 and an
unrelated one below it).
The binary is `llama-server` beside the daemon's executable, built by
`serve/crates/sovereign-compute/llama-server/build.sh` from `PIN.toml` there
(llama.cpp commit, llguidance, each patch by sha256, the binaries), per
backend (`--backend vulkan|metal|cpu|rocm|cuda`, defaulting by host) and
always with RPC, so `ggml-rpc-server` installs beside it, and writes
`llama-server.manifest.toml` beside both. The daemon compiles the pin in and
refuses a shipped binary whose manifest disagrees or names a binary not there
(`llama_router::build_pin::verify`); `[engine] server_binary` overrides the
binary and skips the check, and `/status` says which ran. The whole path is
`sovereign-stock`'s ignored e2e `llama_server_router_e2e`: boot on the
shipped binary, serve a completion and an embedding, SIGKILL the daemon, no
llama-server left (the rerank call is described above).
A node lends its GPU to another node's router with `[compute] rpc_peer =
"ip:port"`, under any engine: serving assembly's first build starts the
shipped `ggml-rpc-server -c` there as a supervised child
(`sovereign_compute::rpc_peer`, kept across reloads like the router), its
build verified against the same pin, `LLAMA_CACHE` set to
`<data>/rpc-peer` so its tensor cache is one the daemon names. The address is
static, with no discovery and no mesh registration, and must be this host's
LAN or tunnel address: loopback and every-interface binds are refused at
startup, because rpc-server authenticates nothing. The supervisor watches it
by connect until it first listens (`HealthTarget::TcpListening`), then by its
exit alone, since rpc-server serves one client on a backlog of 1. A head on
another build is refused by upstream's HELLO. Proof: `rpc_peer_child_e2e`.
A HEAD of such a peer names it in `[compute] rpc_peers = ["ip:port", …]`,
and the router's preset then loads across it with upstream's own split
(`sovereign_compute::llama_router::placement`): every preset section carries
`rpc` (the endpoints as llama.cpp's comma list), written before anything
that indexes a device, since a preset's options are a map keyed by each
option's first spelling and `--rpc` registers the devices `--tensor-split`
indexes (`arg.cpp:2598` names the same order for `-mmdev`). With no `-ts`
llama.cpp divides the model by each device's free memory, the peer's
queried over RPC, so the split is upstream's and nothing is planned here —
no `-ot` (that is D4's question). A peer's `[compute] rpc_peer` bind rule is
applied to the head's list too: loopback and every-interface endpoints are
refused by name, and one peer named twice is refused rather than loaded as
two devices. On peer loss the abort is upstream's and uncatchable
(`ggml-rpc.cpp:30`, `RPC_STATUS_ASSERT` — upstream never reconnects), and it
lands on the MODEL child, not the router: the router loads no tensors, so it
keeps answering and only the model's process exits (`instance name=<m> exited
with status N` in its `--log-file`, `/models` `failed (exit N)`, measured
2026-10-24). The repair loop already reloads that model with backoff, which
IS the head's half of the restart; the peer's half is its own supervisor
starting another `ggml-rpc-server`. `/status` names the lost peer on the
MODEL's line (`RouterPlacement::peer_loss`, keyed on the logged exit — never
on a TCP probe: a peer's rpc-server has a backlog of one and the head's model
holds it, so a probe times out on a peer that is serving). A distributed
preset is never loaded on the head's card alone: the recovered router's
`router.ini` is byte-identical to the one it was started with, and the digest
that covers it is what a reload is refused against.
Proof: `sovereign-stock`'s ignored e2e `router_rpc_split_e2e` — Qwen3.5-9B
split across a peer on loopback, decode at 90% or more of the same model on
one device (D2), a SIGKILLed peer child named by `/status` and answered
again after both supervisors restarted, and no orphan after the daemons are
SIGKILLed.
`BuiltEngine.llama` is `None` for every non-llama engine, and the VRAM
preflight is llama's own question, skipped for engines holding no weights.
The contracts are executable: `engine_conformance::{check_sync, check_serving}`.
Cold start and hot reload both reach it through ONE serving assembly,
`sovereign_compute::assembly` (`assemble_serving`, `ReloadFactory::build`),
which adds admission, llama's slot installs and the compute-child layer; a
reload re-wraps the running compute children instead of spawning new ones.

**The engine conformance battery measures the engine that serves.**
`POST /internal/engine/replay`, on the operator listener beside
`/internal/inference/warmup`, runs one provider call with a
`sovereign_contracts::engine_observe` sink installed and returns the answer
and what the engine recorded on the way. Its `probe` method is
`InferenceProvider::probe_logprobs`: greedy steps with each step's top-k
logprobs, on a context of the slot's own parameters (`embedded/probe.rs`).
The default refuses, and the wrappers on the daemon's path forward it.
`bench/lanes/engine-swap/battery.sh` drives the case bank through it per
target, and `svrn bench engine-conformance judge` holds the records to
`conformance.toml`. Rows whose oracle is a language rather than a record take
their verdict from `--scenario`: `bench/lanes/engine-swap/llg-parity`, a
standalone crate pinned to llama-server's llguidance, judges the corpus the
engine side writes (`llg_parity_corpus_tests.rs`) and emits one verdict per
row.

**A remote engine off this machine is a third party.** `build_remote` builds
one shape, `oicp_client::SplitInferenceProvider::engine` (`far_end.rs`), which
decides each half's `FarEnd`: a loopback endpoint is a server on this box
(locus `ForwardsOnBox`), anything else a third party (locus
`ForwardsToThirdParty`, client from `egress::model_client`). A third-party far
end is released by the operator's config: completions go to it, without our
OICP envelope, and texts to embed or rerank never do (`FarEnd::admit`, the
one rule, applied by `RemoteApiProvider::outbound`, the only way to build a
request). A hosted engine therefore embeds on this machine, and the ROUTER runs that
model: the hosted pair — a vendor `endpoint` with `[engine] embed_path` and
no `[models]` — is a one-section `LiveRouter` (`RouterPreset::embed_only`,
ENGINE_SWAP_PLAN A6), its embedder the section and its chat still the
vendor's. `EngineSection::hosted_router_section` is the one mover (it keeps
every key, writes `context_size = 0`, and is read by the plan, the boot, the
reload and the class check), so `svrn setup --hosted`'s file is served as
written. Nothing loads a GGUF in the daemon's own process any more:
`kind = "remote"` with `embed_path` refuses by name (`build_remote`), and the
one remaining refusal, a vendor chat beside an `embed_path` that also names
an `embed_endpoint`, is `EngineSection::unhostable_on_router` (`ASSEMBLY`
§the router owns the model processes). A no-`model_id` pair sends no `model`
on the wire (`HOSTED_DEFAULT_MODEL_ID` is a placeholder label) and reports
`embed_model_id` as the trait's `"unknown"`.
`EngineKind::needs_models` (every engine but remote) is the one answer to
whether `[models]` is read — by the load-time class check, `node_class()`,
`plan_serving` and the reload — so a hosted node needs none. The engine
reports its vendor models (and the local embed model) as resident slots
(`oicp_client::outbound::Hosted`), so its manifest advertises them to peers
like any loaded model, with `OPENAI_COMPATIBLE_FEATURES` instead of the
embedded set; `[engine] fast_model_id` names a second model for fast turns,
and a turn that names no model is pinned to the one for its speed. `[engine]
structured_output` is how the host is asked for a schema: unset is
`json-schema`, re-asked once as a forced function call after a 400 and
remembered; `tool-use-forced` sends that from the first request, for a host
that ignores `response_format` rather than refusing it (Anthropic's
OpenAI-compatible endpoint; `svrn setup --hosted anthropic` writes it).
The host's answer is read into the embedded engine's shape at the client, by
rules kept in one module for the message and both streams
(`oicp-client/src/response_wire.rs`):
`reasoning_content` goes back into the text as the `<think>` block the engine
writes (not in front of a structured answer), native tool calls, whole or as
streamed fragments, become the `<tool_call>` envelope, and a `tool_calls`
finish reads as `stop`, since the adapter derives `tool_calls` from the text
for every engine. A streamed request asks for usage (`stream_options`), and a
stream that fails, by an error chunk or the transport, ends in
`Finish{Error}`.
A client's vendor fields on `/v1/chat/completions` reach a third-party engine
(`sovereign-serving-host/src/inference_adapter/vendor_fields.rs`): thinking off in any
spelling (`thinking: {type: disabled}`, `enable_thinking: false`,
`think_budget: 0`) becomes a zero budget, which `chat_wire::write_thinking`
writes in every spelling, and `logprobs`/`top_logprobs` are asked of the host,
whose `choices[0].logprobs` come back verbatim on a non-streamed reply. An
engine on this machine reads no `thinking`, as llama-server does not, and
answers without logprobs.
The request is written in the spelling `[engine] grammar` names
(`oicp-client/src/chat_wire.rs`). To a llama-server (`llguidance`) an
`assistant_prefix` goes as a trailing assistant message the server
continues, and the prefix is cut from the front of the answer as the engine
leaves it out; a named `tool_choice`, which that server reads as `auto`,
goes as `required` with `tools` narrowed to the named one, and a reply to a
`required` request that calls no offered tool is asked once more, then
failed naming the miss (`call_check.rs`, non-streaming). To every host, a templated request carries the `enable_thinking` the
engine renders it with (`CompletionRequest::templated_thinking`, off unless
asked), a positive think budget also goes as `reasoning_budget_tokens`, and a
schema goes out closed by the same `additionalProperties` default the engine
compiles it with (`sovereign_contracts::structured_output`), which leaves an
object that declares no properties open. In the embedded engine a schema waits
for a think block the prompt opened (`think_budget::opens_think`): no mask and
no matcher step until the accepted bytes close it (`ThinkWait`), so the
reasoning is free and the answer after it is held to the schema; a block that
never closes is warned at the sampler's drop.

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

**Code intelligence** is served over MCP by `svrn code mcp` (also spelled
`svrn serve` and `svrn project serve`: no model, no daemon, no mesh; its tool
set is `sovereign-code`'s bundles behind the host kit's dispatcher) or, on
the stock install, mounted on the daemon's one `:9741/mcp` through code's
face (`sovereign_code::face`, pb-code-daemon-exit); a svrn daemon alone serves
no code tool and names `svrn code mcp`. The editor door (`POST /v1/edit_predictions`) is code's too
(`sovereign_code::edit_predictions`, pb-meshapp-rest): the stock binary mounts it on svrn's client
surfaces, its model lane dials serve, and svrn alone answers it with a 503 naming the code server. So is the
TDD solver (`sovereign_code::solve_http`, `/v1/solve/jobs*` and MCP `solve`, pb-meshapp-solve): its chat
dials serve, no serve is a named refusal, and svrn alone answers the routes the same way. The tools live in their own crate, `sovereign-code` — 18,431 lines
lifted out of `sovereign-tools` on 2026-09-21 (822681564), so that `svrn code`
is a program with a boundary a gate can read rather than a module inside the
knowledge server (docs/internal/FIVE_PROGRAMS.md §2). Tools under
`sovereign-code/src/`: the code index (`symbols`,
`code_search`, `recent_changes`, `working_set`, `brief`), the session brief
(`briefing`), the tree-sitter fact base (`facts`), the SCIP call graph
(`callers`, `callees`, `blast_radius`), watchers, notes,
drift, capability docs, project context, session reflection, and work-atlas
coordination (`declare_scope`, `release_scope`, `work_in_flight`).

**One daemon, each repo's own answers** (code-intel-repo-scope). The daemon
holds every repo's code corpus, and a call says which one it is about:
`ToolContext::corpus_scope`, filled at each surface's edge. Over MCP it is the
`x-svrn-corpus` header (`oicp_types::mcp::MCP_CORPUS_HEADER`, carried on
`McpRequestContext`); code's host admits only a corpus it holds
(`sovereign_code::admit_corpus`) and answers anything else -32602 naming the
corpora that exist, before any tool runs. On the CLI it is the code corpus
whose `source_path` is the cwd's git root (`tools_cmd/scope.rs`), else every
corpus with one stderr line saying so; `--all-corpora` asks for every one. The
graph tools take the scope as a SQL predicate (`corpus_id` is a column on
`symbols` and `refs`), and the chunk tools read one enumeration,
`code_indexes`. A connection that sends `x-svrn-effects: read` neither lists
nor calls a tool whose manifest effect is not `Read`
(`sovereign_contracts::mcp_host::effect_reachable`, the one rule both paths
apply). Each repo's `.mcp.json` names its corpus; the ralph shim hands a
worker its workdir's, never this checkout's. The merged graph still loads
once per daemon, so a corpus indexed later answers after a restart
(order `code-intel-graph-refresh`).

Code's tool graph and the Reindexer share ONE merged `ScipGraph` handle,
so updates are visible to `symbols`/`callers`/`blast` live. Each debounced
save runs an embed-free tree-sitter overlay; the heavy rust-analyzer export is
demoted (spawned, rate-limited, quiescence-gated, `nice +10`); a commit waits
out the same 15-minute cooldown from the last export's end, its messages
harvested at once (`reindexer/head_step.rs`), and the export is
one-writer, staging and renaming under a cross-process flock so a query in
flight always sees a complete graph. The export's synchronous tail (index
decode, bulk writes, the integrity-checked open, the merged import) runs on
tokio's blocking pool (`shared/crates/corpus-engine-scip/src/off_worker.rs`):
inline, it left the daemon runtime's I/O driver unpolled for the whole tail,
up to 113 s per rebuild here, and every request and timer waited on it. **One project owns one workspace**:
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
- **Skills** — TOML under `svrn/modes/`. `SkillRegistry` merges routing
  hints, planner templates, prompt overrides, memory rules and OICP
  requirements; skills carry `signature` / `signed_by` and a `TrustLevel`.

### Frontends

| Frontend | Notes |
|---|---|
| `sovereign-cli` (+ siblings) | Dispatcher. `sovereign <verb>` execs into `sovereign-cli-daemon`, `-dev`, `-llm` or `-mesh`, and `svrn agent-bench` execs the `sovereign-agent-bench` binary the same way rather than linking that crate. Unix execs (same PID); elsewhere spawn-and-wait. Discovery is `current_exe()`'s parent, overridable per sibling (`SOVEREIGN_CLI_{DAEMON,DEV,LLM,MESH}_BIN`, `SOVEREIGN_AGENT_BENCH_BIN`) |
| `sovereign-desktop` | Tauri 2 + Svelte 5, rail `Ask · Library · Reflect · Workshop · ⚙`. Layout is token-driven: `app.css` owns the scale and three global primitives (`.page-body`, `.page-measure`, `.page-header`). Do NOT re-declare padding or overflow on an element carrying `.page-body` — Svelte scoping wins silently and clips content with no way to scroll to it |
| `sovereign-mobile` | Thin Tauri 2 client — no local inference, Runtime or corpus. Consumes `sovereign-turn-client` and nothing else on the wire. Named ceiling: the client family has no auth seam, so the phone reaches a DAEMON |

Verbs by sibling: `sovereign-cli` holds the light delegators (`notes`,
`status`, `drift`, `session`, `design`, `plan`, `init`, `reflect`, `memory`,
`serve`);
`sovereign-cli-daemon` holds `daemon`, `setup`, `install-service`, `doctor`;
`sovereign-cli-dev` holds `tools`, every `code` subcommand (`code index`
included), `refresh` and the `project` lifecycle subcommands, and links no
corpus-engine: `code index`, `code finalize` and `code watch` exec ingest's
`svrn-ingest`; `sovereign-cli-llm` holds everything that
talks to a model or does heavy retrieval, except bench's verbs: `sovereign-cli-bench`
holds `bench`, `eval` and the `quality lane` lanes (pb-cli-llm-bench-move), and svrn's
white-box lanes under those spellings (`bench judge-replay|resolver-precision`,
`eval inner-chaos`) stay in `-llm`; and except ingest's verbs: `svrn-ingest`
(sovereign-pipeline) holds `enrich`, `corpus`, `atlas`, `meta-atlas`, `recipe`,
`pipeline`, `alignment` and `bench atlas` (pb-cli-llm-ingest-move), and the
sub-verbs under those spellings that are svrn's (`enrich raptor|raptor-index|summary-atoms`,
`atlas budget|status|list-corpora|list-atoms|show-atom|typed-extension`, `corpus
ingest|share|pull|catalog|extract-entities|watch*`) stay in `-llm`, one table in
the dispatcher's `ingest_bin.rs`; `sovereign-cli-mesh`, lifted out of
`-llm` on 2026-09-21, holds `mesh` (guest grants and media among its
subcommands), `meshapp`, `ring`, `job`, `publish`, `unpublish` and `run`. `code converge` is the one verb
LINKED rather than exec'd, from `sovereign-cli-dev`'s `[lib]` target.

### Subsystems with their own docs

| Subsystem | Doc |
|---|---|
| Slots, OICP, harness, cutoffs | [`svrn/docs/inference.md`](../svrn/docs/inference.md) |
| Inline completion (FIM) / next-edit | [`svrn/docs/INLINE_COMPLETION.md`](../svrn/docs/INLINE_COMPLETION.md) |
| Glassbox reading surface + Atlas Inspector | [`svrn/docs/knowledge-view.md`](../svrn/docs/knowledge-view.md) |
| Knowledge bases + tiered retrieval | [`svrn/docs/KNOWLEDGE_BASES.md`](../svrn/docs/KNOWLEDGE_BASES.md), [`svrn/docs/TIERED_RETRIEVAL.md`](../svrn/docs/TIERED_RETRIEVAL.md) |
| Retrieval redesign | [`svrn/docs/RETRIEVAL_REDESIGN.md`](../svrn/docs/RETRIEVAL_REDESIGN.md) |
| Epistemic state / the epistemic index | [`svrn/docs/EPISTEMIC_STATE.md`](../svrn/docs/EPISTEMIC_STATE.md), [`svrn/docs/specs/EPISTEMIC_INDEX.md`](../svrn/docs/specs/EPISTEMIC_INDEX.md) |
| Ontology primitives + migration | [`svrn/docs/specs/ONTOLOGY_PRIMITIVES.md`](../svrn/docs/specs/ONTOLOGY_PRIMITIVES.md), [`svrn/docs/specs/ONTOLOGY_MIGRATION.md`](../svrn/docs/specs/ONTOLOGY_MIGRATION.md) |
| Drift / correctness tooling | [`svrn/docs/DRIFT_DETECTION.md`](../svrn/docs/DRIFT_DETECTION.md), [`svrn/docs/CORRECTNESS_TOOLING.md`](../svrn/docs/CORRECTNESS_TOOLING.md) |
| Work-atlas peer coordination | [`svrn/docs/WORK_ATLAS.md`](../svrn/docs/WORK_ATLAS.md) |
| Desktop quality surface — START HERE to verify the desktop | [`clients/desktop/QUALITY_SURFACE.md`](../clients/desktop/QUALITY_SURFACE.md) |
| Browser actuation / TDD machine / Solver | [`svrn/docs/BROWSER_ACTUATOR.md`](../svrn/docs/BROWSER_ACTUATOR.md), [`svrn/docs/TDD_MACHINE.md`](../svrn/docs/TDD_MACHINE.md), [`svrn/docs/SOLVER_DESIGN.md`](../svrn/docs/SOLVER_DESIGN.md) |
| Mobile / session continuity / memory model | [`svrn/docs/specs/MOBILE.md`](../svrn/docs/specs/MOBILE.md), [`svrn/docs/specs/SESSION_CONTINUITY.md`](../svrn/docs/specs/SESSION_CONTINUITY.md), [`../docs/specs/MEMORY_MODEL.md`](../docs/specs/MEMORY_MODEL.md) |
| Worker pods / cloud peers | [`svrn/docs/PINNED_WORKER_AS_INFERENCE_PEER.md`](../svrn/docs/PINNED_WORKER_AS_INFERENCE_PEER.md), [`docs/CLOUD_PEER.md`](CLOUD_PEER.md) |
| On-call runbook / threat model | [`svrn/docs/RUNBOOK.md`](../svrn/docs/RUNBOOK.md), [`docs/THREAT_MODEL.md`](THREAT_MODEL.md) |
| FAQ / troubleshooting | [`svrn/docs/FAQ.md`](../svrn/docs/FAQ.md), [`svrn/docs/HAVING_TROUBLE.md`](../svrn/docs/HAVING_TROUBLE.md), [`svrn/docs/TROUBLESHOOTING.md`](../svrn/docs/TROUBLESHOOTING.md) |

Notable in-tree invariants: watched folders are read-only on source, and
sensitive folders never leave the machine.

---

## 5. cmnwlth — the coordination daemon

A symmetric daemon: every node runs the same binary, no master. It translates
"complete this chat with model X" into a plan that spawns `llama-server` on
one node and `rpc-server` on others, holds the OpenAI-compatible endpoint
open, and keeps the plan healthy as nodes come and go.

### Discovery and membership

Since pb-mesh-exit-transport the node's one mesh endpoint is `cw-rails`: it
holds the node's one Ed25519 key, the roster, gossip, joins, the known-mesh
list and the ring round, and svrn neither accepts nor dials a peer itself
(svrn reads the roster and reaches peers through the ports a distribution
composes from cw-rails, `hosted_mesh`). Identity is mesh-independent and
survives `leave` and every switch. mDNS advertises `_commonwealth._tcp.local`
when `svrn mesh up` starts cw-rails with `--mdns`, and refuses by cause on a
host where a send to the mDNS group leaves on no interface (mdns-sd itself
registers Ok there; `/v1/mesh/status` reads `mdns.advertising: false` with the
reason); gossip is a 10s round over
3 peers (`select_peers`: active keyed members, Online first, rotated by round)
with timestamp-LWW conflict resolution, and offline-decay reads this node's
contact clock, never a peer's `last_seen`.

`Mesh` carries **two** credentials and the split is load-bearing:
`mesh_secret` authorizes gossip and never rotates; `invite_key_hash` admits
joiners and rotates freely. A gossip round carries a keyed-BLAKE3 `mesh_proof`
bound to the sender and a 30s window rather than the raw secret; an OFFERED
proof that fails is a hard refusal, never a fall-through. Rotation is refused
while an Online peer is pre-split or unconfirmed (`?force` overrides), and the
confirmation is local observation (each merge's `peer_pre_split`), never a
peer's claim. `aliased_endpoint_keys` is the one implementation of "one
endpoint key, one member row"; `merge_from_authenticated` refuses to ADMIT a
collision, and `/v1/mesh/forget-member` retires one.

**Encryption.** Every mesh is encrypted: cw-rails founds no other kind, and
every class rides iroh QUIC dialled by key, admitted against the roster. The
daemon's internal API binds loopback only. The hops between a program and
cw-rails, and between `llama-server` and its local bridge, are loopback
plaintext on the host. Surface-by-surface posture
[`docs/THREAT_MODEL.md`](THREAT_MODEL.md).

### The PeerTransport seam

`mesh-reach` names (peer, traffic class) → ordered base URLs in one trait.
svrn and serve dial through `RailsTransport`, which asks cw-rails' reach door;
cw-rails resolves through `commonwealth-transport`'s `IrohTransport`
(dial-by-Ed25519-pubkey QUIC bridged to HTTP through localhost byte-tunnels).
The plaintext IP overlay is gone with the plaintext mesh (phase-b-36). Out of
seam by design: the join handshake, worker-pod transport and loopback
self-probes.

Seven ALPNs carry the mesh, and what a STRANGER gets differs per ALPN —
**holding the dial string is not a credential**, so cw-rails' acceptor routes
on `(ALPN, dialer)` through its origin registry (each program registers its
own loopback origin; the key the QUIC handshake proved is the discriminator):

| ALPN | member | stranger |
|---|---|---|
| `cwth/client/0` | serve's client origin (no bearer — federated inference carries none, its key is the credential) | svrn's guest listener (`Admit::MembersElse(cwth/guest/0)`) |
| `cwth/rpc/0` | serve's ggml rpc worker | REFUSED — it authenticates nothing |
| `cwth/media/0` | the `[media] origin` in cw-rails' rails.toml, for members inside `[media] allow` | REFUSED |
| `cwth/app/0` | one of several named HTTP apps, chosen by first path segment; each app claim carries its publisher's allow list (svrn's `[iroh] app_allow`) | REFUSED |
| `cwth/offer/0` | svrn's `[iroh] offer_origin`, for members inside `[iroh] offer_allow` | REFUSED — the dial string is gossiped, so a downgrade would publish a household's inventory |
| `cwth/guest/0` | — | svrn's guest listener; it reads the bearer |
| `cwth/http/0` | by registered prefix: gossip and join (any dialer), `/internal/ring/sync` and `/internal/ring/live` (members; the checkpoint is local-only), each program's peer prefixes | gossip and join, DELIBERATELY — a joiner is not a member and `/internal/join` is how it becomes one; any other prefix refused by name |

Federated media and named apps ride that surface: the holder declares an
origin (`svrn mesh media offer | admit | withdraw | origin | declare` write
`[media]` in cw-rails' rails.toml and the credential under its data dir, then
`POST /v1/mesh/media/reload`, which re-stands the origin with no restart —
`commonwealth-rails/src/origins.rs::stand_media`), the viewer asks cw-rails
for a loopback bridge URL, and the acceptor
tells the origin WHO is asking by rewriting request heads
(`X-Mesh-Member`/`-Node`/`-Pubkey`, every client-supplied `x-mesh-*` header
dropped first). Responses are a byte copy, which is why `Range` stays
byte-exact. `svrn mesh offers` enumerates the roster, so a neighbour
publishing nothing appears as a ROW carrying that refusal rather than absent.

### Scheduling and orchestration

Eight decision points, each with one home:

| Decision | Where |
|---|---|
| Joiner decides a turn is offload-eligible | `sovereign-scheduler/oicp_select.rs::offload_eligible` |
| Joiner picks peer-vs-local | `sovereign-serving-host/src/peer_inference.rs::select_peers_ranked` |
| Joiner resolves a *named* target | `peer_inference.rs::locate_named_model` — name resolution + min-in-flight, **not** the scorer. A HARD name is a constraint: unknown ⇒ error, never substitution |
| Hub picks a local model for a peer request | `sovereign-daemon/src/routes_inference.rs::route_with_oicp` |
| Serving peer picks Fast-vs-Slow slot | `sovereign-serving-host/src/slot_select.rs::pick_slot_for_oicp` |
| Synthesis tier (Fast vs Primary) | `sovereign-core/runtime/evidence.rs::resolve_synthesis_route` |
| Distributed placement (model > one node) | `sovereign-inference/embedded/rpc_distribution.rs` (load); plan and warm cache, FFI-free, `sovereign-inference/rpc_warm_cache.rs` |
| Collaborative ingest partitioning | `sovereign-grants/knowledge_assignment.rs` |

Slot policy is normative in [`svrn/docs/SLOT_POLICY.md`](../svrn/docs/SLOT_POLICY.md):
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
Findings are in `svrn/docs/specs/SCHEDULER_QUALITY.md`; **the standing one is F10 —
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
the only writer of a roster. It writes under cw-rails' root
(`commonwealth_media::rails_data_dir`, `$CW_RAILS_DIR`), where cw-rails reads
it, through `ring_cmd::journal_root`, the root `svrn mesh offers --why` reads
warrants from too.

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
loaded decides which acts are readable. `job status` folds `GET /v1/rail/log`
with the same `commonwealth_work::projection::fold` that `cw-rails`' own
donor loop runs (and serves at `GET /v1/work/projection`), so the terminal
and the donor cannot disagree about who holds a lease. The donor is cw-rails'
since pb-work-donor (`[work_offer]` in `rails.toml`); the daemon serves only
the `ingest:v1` execute origin it forwards units to.

### HTTP API

**Client API — :9741, binds 127.0.0.1 by default.** The wildcard bind is
reached only when something explicit asks for it. A non-loopback bind carries
a bearer token or serves nobody; when the token chain fails entirely the
posture installs NONE, so `client_auth` refuses every remote caller rather
than serving unauthenticated.

A presented credential decides, from any address. `client_auth` and the edge
resolver read one classification, `client_principal::Presentation`
(`client_principal/presented.rs`): a bearer that verifies (a live grant, a
named token, an API key, the daemon-wide token) admits as what it is; one in
the daemon's form (`svrn_`, which `client_auth::generate_bearer_token` writes
on every credential it mints) that verifies nothing is a 401 from loopback
too; one NOT in that form from a local process, where loopback is the owner,
is read as no credential (INTEROP §1's `OPENAI_API_KEY=local`), and from
anywhere else is refused.

A non-loopback caller presents one of two bearers. `client_token` is
daemon-wide. An **ephemeral guest grant** is the narrow one: short-lived,
revocable, bound to a closed `Scope` enum whose `paths()` is the only route
allowlist there is. A guest is not a mesh member and cannot mint further
grants, because no `Scope` variant names `/internal/*`. A grant also names its
`GrantHolder`: `Guest` (the default, every link a person holds), whose writes
must claim a name at the rail door, or `MemberPage`, which only the operator's
mint route sets and only `svrn ring show` asks for, whose writes are the
member's own.

A named credential is one record `{name, token, groups}` at
`<data_dir>/client-tokens/<name>.key` (`client_tokens::ClientTokenStore`; legacy
`<label>.token` files are read as the same record and rewritten as `.key` once
the posture is declared), minted and revoked by `svrn daemon key` — through the
running daemon's operator routes, or the store itself when none runs — and it
resolves to `Principal::Asserted { sub: name, groups }`. `[daemon] loopback`
declares the posture (`client_tokens::LoopbackPosture::resolve`, its one
reader): `"owner"` admits a local process presenting nothing as the owner;
`"none"` grants loopback nothing. Undeclared, it is inferred as before (`none`
iff a legacy `.key` file exists, warned at boot and in `svrn doctor`'s
`loopback_posture`), and minting refuses until it is declared.

A daemon under `loopback = "none"` is KEYED: `api_keys::seal` wraps every client
listener (the guest door's in `guest_door::door_router`, so its own bind and
the `GUEST_ALPN` forward take one sealed router), loopback grants nothing, a key resolves to `Principal::Asserted`,
conversations are stored `{sub}:{id}`, a non-`admin` key reaches only
`api_keys::KEY_SCOPE`, and `[retrieval] corpora` is every key's corpus grant.
`granted_http` serves the grant: `GET /v1/corpora`, the reading window
`GET /v1/corpora/{c}/chunks/{id}` (403 naming an ungranted corpus) and
`GET /v1/tools`; `GET /health` is an unauthenticated `ok`. The daemon's calls
to its own routes (the OCR cleanup pass) present
`client_tokens::self_credential()`, a per-process admin key a keyed store
admits and no disk holds; a CLI presents `SOVEREIGN_API_KEY`, read by
`setup_config::client_credential` (`svrn corpus watch*` sends it).

| Path | Notes |
|---|---|
| `POST /v1/chat/completions` | OpenAI-compatible; `LocalOnly` privacy → 400. An external client's conversation (no OICP envelope) takes llama-server's path: the model's template renders it whole, the reply is parsed in the model's own format with `reasoning_content` apart, and a stream is live with or without tools (`serve/crates/sovereign-serving-host/src/inference_adapter/conversation.rs`, `oicp_types::chat_turn::TurnStream`); a prompt over the window is 400 `exceed_context_size_error` |
| `POST /v1/responses` | OpenAI Responses-API adapter |
| `GET /v1/models` | Names this daemon can dispatch by name, built from the local OICP manifest + every reachable peer's — the same source `locate_named_model` resolves against, so a listed id resolves and an omitted one does not |
| `POST /v1/embeddings` | What peers call via `embed_http::http_embed_fn` |
| `POST /v1/rerank` (and each served kind's route) | Mounted from the served-kind registry (`sovereign_inference::served_kind`); answers with the rerank kind's slot or compute child |
| `POST /v1/knowledge/search` | Determines target corpora, fans out, merges, reranks. The local corpora searched, here and on the peer route `/internal/knowledge/search`, are the caller's `oicp_evidence::readable_corpora` (a member reaches `query_sharing` corpora, an unverified claimant none, a key its grant); one held here outside that set answers like one nobody holds. Every local hit, and the peer route's, carries `document` (OICP v0.5 §3): its text's record read through `chunk_text_sha256s` + `documents_for` by the one projection `oicp_evidence::knowledge_results`, or names why not in `document_absent` (`texts not stored`, `text not stored`, `document unreadable`); the v0.2 `metadata` map stays empty. Both routes search through `oicp_evidence::search_corpus`: a `documents` filter (text names, OICP v0.6 §2.8, `knowledge:scoped`) runs inside the index (`CorpusIndex::search_in_texts`, `only_if` on `text_sha256`), a malformed name is `400`, it is forwarded to peers, and `build_response` drops any merged hit from an unnamed text. Every hit also carries `span`, its range of its stored text located by `oicp_evidence::span_of` (the locator verify's passages share), or `span_absent` (`knowledge:range`). A hit's document, and its span's, carries the text's record by identifier when its install kept one (OICP v0.6 §2.16, `records_by_identifier::attach_to_hits`, read from the corpus's `_records.json`, on passage and sentence hits alike), the same record progress lists; the text and align routes' documents carry none yet. The span also carries `locator` (v0.6 §2.13, `knowledge:locator`), read by `oicp_evidence::locator_at` from the places the text's writer kept beside it (`StoredText::locators`, the `<sha256>.locators` sidecar of `TextWriter::store_document_located`; `oicp_types::locator::PlacedLocators`), shared by `span_of`, the sentence route's `hit` and align's `wire_alignment`; a declared transcript's turns and a plain text's heading paths are kept, and a span of a text with neither carries none. `svrn search` is the CLI client, and prints each hit's locator (`under BOOK I. › CHAPTER I.`, `at 0:01:04, HARBOUR MASTER`) and its catalogue entry (`search_cmd/catalogue.rs`: author, year, title, container, how many fields a resolver supplied, and any identifier that did not resolve) from that record, or from the declared metadata when there is none. Where its time goes: one DEBUG event per stage (`search_stage::SearchStage`: `admission`, `query_embed`, and per corpus `index_query`, `records`, `text_read`, `span_of`), each with `elapsed_us` and `count`, under one `search{route, query}` span (`query` is the query text's sha256, 12 hex), all on the `search.stage` target, so `RUST_LOG=…,search.stage=debug` shows them; the embed slot's own wait is the engine's `slot_wait_ms` on `inference.embed: done`, under the same span. A request's `priority` (OICP v0.6 §2.17, `knowledge:priority`) runs the handler under `sovereign_contracts::request_priority::asked`, and `sovereign_inference::priority_gate::PriorityGate` holds each step of model work at the embed slot (`embed`, `embed_query`, `embed_batch`) and at the generation slots' admission (`acquire_with_queue_gauge`, the step kept in `SlotPermit`): each site tells its `Step` `entered()` once it holds its stage (the embed context's lock, the slot's permit); an `interactive` step is counted from its ask, a `background` step waits until none is, and the background steps waiting to enter one stage (the stage named at `step`: `embed`, or the slot queue's label) go one at a time, a step inside its stage holding none back (PV3e) and a step keeping its place however long the ones ahead take (PV3h), its hold for interactive work bounded by `HOLD_CEILING` (2 s) counted from its place; a background completion's prompt is read in parts of `BACKGROUND_PART_TOKENS` (64) through `PriorityGate::in_parts` (`ModelSlot::prefill_reusing_prefix`, the request carried to the blocking thread by `sovereign_contracts::call_scope::carry`), pausing between parts while an interactive step that has entered its stage runs, never for one waiting on the slot the prompt holds, and a background embed batch takes the embed context once a sub-batch through `PriorityGate::parts_under` (`EmbedSlot::run_embed_batch_sync`), letting it go and pausing between them; an embed slot on the GPU keeps `EmbedSlot::writer`, a CPU context over a CPU copy of the model, and an `interactive` request's embeds compute on it (`EmbedSlot::context_for_request`, PV3d; `Writer::None` for `EmbedOnlyProvider`); events on the `priority` target say what each background step waited behind. At `granularity: sentence` (OICP v0.6 §2.11, `knowledge:sentences`) the handler hands the request to `routes_knowledge_sentences::search`, local corpora only: the query and the embedder's sentinel probe embedded in one call, each scoped corpus prepared by `sentence_index::SentenceIndex::prepare` (its texts split by `sentence_ranges`, a Project Gutenberg wrapper left out by `sentence_index::wrapper::body`, a sidecar an earlier splitter cut re-cut with its vectors kept by key, rows `(text, code-point range, byte range, sha256 of the sentence)`, vectors loaded from the corpus's `_sentences.bin` sidecar or embedded in the background by the one `sentence_index` worker, queued after an OICP install, at the first sentence search naming the corpus, and at start for every corpus with a sidecar), then either the `index` route (every scoped row scored by `scan::top_k`, threaded, up to the data file's `scan_ceiling`, named `truncated` past it) or, while any scoped sentence is pending or the vectors are another embedder's, the `passages` route (`search_corpus`'s hits' overlapping sentences, with `sentences_pending`), and, with `knowledge:sentence_queue` (v0.6 §2.11), `sentence_queue`: where each scoped corpus with sentences waiting stands in the worker's line, `embedding` now or `queued` behind the corpora ahead with their pending counts. The order is `sentence_index::queue::Fifo`'s, the one the worker takes from and finishes against; a search reads a `QueueSnapshot` of it (`SentenceIndex::queue`) and `queue::places` builds the wire places, naming no corpus the worker has finished and none ahead of a corpus with nothing before it (events: `embedding now`, `placed`, and `queue placed` on the search). A corpus whose directory goes away mid-walk (`svrn corpus remove`, which tells the daemon nothing) stops at the worker's next batch and is forgotten (`SentenceIndex::forget`; event `the corpus was removed while it embedded`), so the corpus behind it goes next. The scorer is `data/sentence_search.toml`'s (`cosine`, or `rerank` over cosine's best `rerank_candidates` through `rerank_batch`, its model the resident rerank slot's); one that cannot run is `503 scorer unavailable` by name. Hits are cut from their stored addresses, never `span_of`; its stages are `SearchStage::SENTENCE_ROUTE` (`query_embed`, `admission`, `sentence_scan`, `text_read`; admission's parts `local_embed_model`, `resident_slots` and `admit` on one `admission in parts` event, `search_stage::admission_parts`) under a `search{route="sentences"}` span, and the index's own events are on the `sovereign_daemon::sentence_index` target (`prepared`, `loaded from its sidecar`, `ready` with `elapsed_ms` and `resident_bytes`). `svrn search --sentences` is the CLI client, and prints each waiting corpus's place (`search_cmd::queue_lines`). `MeshKnowledgeClient` keeps it as `MeshScoredChunk::document` |
| `GET /status` | Node / mesh / inference / knowledge summary, incl. `process.pid` + `run_id` |
| `GET /oicp/v1/capabilities` | Provider manifest + federation info |
| `POST /oicp/v1/corpus/install` | OICP install, by registry id or (v0.5 `ingest:recipe`) with `recipe_toml`: the recipe loads through `Recipe::from_toml`, must name `corpus_id`, is written to the local registry, ingested, and stamped as `<corpus>/_recipe.sha256` (`corpus_index::corpus::recipe_sha256`, the one spelling; a snapshot reads it as `source_recipe_sha256`). A recipe `[[document]]` may declare `format = "transcript"` (v0.6 §2.14, `ingest:transcripts`; `DocumentFormat`, a closed set, so any other value fails the recipe load, `400`): `DeclaredMetadata::read` is the one place it is applied, reading the document with `oicp_types::transcript::TurnOutline::read`, so the engine's ingest loop (and reindex) store its words only and keep each turn's start and speaker as locators beside the text (Whisper's JSON never a speaker); a declared document in neither shape is listed `failed` with `not a transcript` and stores nothing. Every other document is read as plain text there too: its heading paths (v0.6 §2.13, `oicp_types::locator::HeadingOutline`, the one rule: a paragraph opening with a part word, a numeral and a stop, part words nesting in the order the text first uses them) are kept as locators beside the text, none for a text with no headings; the delta update (`update::delta::stored_chunks`) stores through the same `read`. The recipe-test runner (`harness::runner`) still chunks the raw source. The same `(corpus_id, recipe_sha256)` is `spawned: false`, a different recipe reingests (validated before the old index is removed). Outcomes map through `routes_internal::install_status`, shared with the internal route: an invalid recipe or parameters 400, an unknown id 404 (until 2026-10-08 all three answered 200 `spawned: false`). The spawn helpers live in `routes_internal/corpus_install.rs`. The route reads bodies to its own `routes_oicp_ingest::MAX_INSTALL_BYTES` (64 MiB, a route-level `DefaultBodyLimit` inside the surface's 8 MiB `MAX_REQUEST_BODY_BYTES`), stated in the manifest as `knowledge.ingest.max_install_bytes` (v0.6 §2.12); a body past it is `413 install too large` naming the limit. Until 2026-10-14 install read to the surface's 8 MiB and refused a 22.6 MB inline library with axum's unnamed 413. `allow_lookups` (OICP v0.6 §2.16, `ingest:lookups`, default false) is part of the install: the same recipe with the other allowance is a new install, reingested (`records_by_identifier::allowed_by_last_install`). Once the engine returns, and while the install is still listed active, `records_by_identifier::record_install` reads each ready text's stored metadata (its lowest `(source, ordinal)` record), and for a `DOI` or `ISBN` the data file `data/record_resolvers.toml` names a resolver for (Crossref's CSL-JSON `transform`, Open Library's Books API, one reader per reply shape in `records_by_identifier/vendors.rs`) asks it, only when the install allowed lookups (`RecordLookups::for_install` is the one place an allowance becomes a way out), each distinct identifier once, `concurrency` at a time, through `RecordFetch` (`WebFetch`, its client from the egress boundary's `search_client`; tests replay `tests/fixtures/records/` through `RecordedFetch`). `merge` keeps every declared field (origin `declared`), adds a returned field in the data file's `keep` set only where none is declared (origin the resolver), made CSL 1.0.2's by the file's `[csl]` table and the resolver's `types` or left out (`records_by_identifier::csl::CslSchema::shape`, through `ResolverTable::take_found`, the one place a reply's fields enter a record, for `merge` and the retry alike; platform row PB1b), and names each lookup `resolved`, `unresolved`, `unreachable` or `not_allowed`, the last three adding nothing. The records, and the allowance, are kept as the corpus's `_records.json` (`InstallRecords`, by text name); a document past the listed cap or in a corpus keeping no texts has no text name, so no record. `unreachable` is a fact about one moment, not about the identifier: a held install (same recipe and allowance) whose kept records name any lookup `unreachable` asks those, and only those, again (`records_by_identifier::retry_last_install`, called from `spawn_install` before the held answer, while the install is still listed active; records only, no text is read or ingested), merging under `merge`'s rules and rewriting `_records.json`; `resolved` and `unresolved` stand (live 2026-10-15: one Crossref 429 had pinned a lookup `unreachable` until the corpus was removed). Every branch traces under `records:` on the `sovereign_daemon::records_by_identifier` target |
| `GET /oicp/v1/corpus/progress` | OICP progress (`routes_oicp_ingest::corpus_progress`): the internal `corpus_progress` map projected onto coarse phases, unioned with `active_ingests` so an install with no engine callback yet is `pending` from its spawn (v0.6 §2.9, `ingest:documents`). Once an install returns, its entry is `complete` (written by the install task itself) and lists each document from `IngestResult::documents`: the engine's ingest loop records each one `Ready { text_sha256 }` or `Failed { reason }` (`corpus_index::ingest_port::daemon::DocumentOutcome`), named as the recipe names it (`DeclaredMetadata::source_name` / `name_of`: an inline name, a file's path relative to the acquired root); an extractor names the document it failed on with `Error::of_source`. Each ready document carries its record by identifier when its install kept one (v0.6 §2.16, `IngestDocumentState::Ready.record`, from `_records.json`), whether or not any search has hit it; `_records.json` is read as `RecordsRead::{Kept, NotWritten, Unreadable}`, and an unreadable one (corrupt, or the install could not make its records and wrote `unread`) is named in the entry's `detail` (`records unreadable: …`) rather than listed as no records, the same file makes a hit's document and span `document unreadable` (`attach_to_hits`) and the last install's allowance unknown (`allowed_by_last_install` is `None`, so the next install reads again). Kept with the corpus as `_documents.json` (`Corpus::stamp_install_documents`, an `InstallDocuments`), cleared, with the last attempt's terminal entry (`complete` or `failed`), by the next attempt's `clear_last_attempt`, so a re-install is `pending` from its spawn (platform row PV3g); capped at `DOCUMENTS_LISTED_CAP`, the rest counted in `documents_unlisted`. A corpus held from before the daemon's last start has no in-memory entry and is listed `complete` from that record (`held_with_documents`); a stamped corpus with no record is read again on its next install rather than answered `spawned: false` |
| `GET /oicp/v1/text/{text_sha256}` | A stored text by name (OICP v0.5 §2.2, `routes_oicp_text`): whole, or `start`/`end` code points with `context`. Consults only the corpora `oicp_evidence::readable_corpora` admits (the caller's kind and the turn's `PrincipalScope` over the runtime's `corpus_principal`, both; a mesh member reads only `query_sharing` corpora), each opened by `oicp_evidence::open_for_reading` (`Corpus::open`, without the query path's embedding-width gate: a text read compares no vectors), in `corpus_id` order, so the record served is the lowest `(corpus_id, source.id, ordinal)` the caller may read. Refusals are `oicp_types::evidence::reasons`, mapped from `TextAbsence` by `oicp_evidence::absence_reason`; a corpus that did not answer is a 503, never `text not held`. Its second user is svrn's own answers: each `answer_segments` entry carries the quotations the quote guard verified in a stored text whose first byte it holds, each a `QuoteAddress` (`sovereign_contracts::types`: the answer's byte range, `corpus_id`, `text_sha256`, code-point `start`/`end` and `exact`), placed by `quote_surface::address_quotes`; a quotation verified only against the prompt's evidence or a chunk has no address. The desktop opens one where it stands through its `read_text_slice` command (`clients/desktop/src-tauri/src/commands/reading.rs`), which reads the `text_endpoint` the daemon's manifest advertises under `evidence:text` and hands a refusal to the reader by its published reason; the webview never fetches the daemon itself |
| `POST /oicp/v1/align` | OICP v0.5 §2.3, with v0.6 §2.18 (`routes_oicp_align`): where a quotation stands in the stored texts the caller reaches (`oicp_evidence::readable_corpora`, the one read decider the text route shares: the caller's kind — the owner and its clients every corpus, a member the `query_sharing` ones, a guest or unverified caller none — and the turn's `PrincipalScope`, keyed attribution and grant, must both admit a corpus). Each corpus, opened by `oicp_evidence::open_for_reading` as the text route opens it, gives its FTS top `candidate_k` chunks (`align.toml`), which name their texts by `text_sha256`, and, for verbatim recall (OICP v0.6 §2.18), every stored text `quote_align::holds_words` keeps (`CorpusIndex::texts_holding`, one read of each stored text); `quote_align::align` reads those texts, keeping an alignment whose word coverage (a mark is not a word) reaches `coverage_floor` and whose matched words are specific (rarity-weighted share at `specific_floor`, or a run of `specific_run` words); a span names the lowest `(corpus_id, source.id, ordinal)` document whose chunk matched. Every requested corpus is in `corpora` with its `texts_digest` (the engine's, through `IngestPort::texts_digest`) or in `corpora_unavailable` by name (`corpus not held`, `texts not stored`, …). Advertised as `evidence:align` with `knowledge.evidence.align_endpoint` |
| `POST /oicp/v1/verify` | OICP v0.6 §2.6, by meaning under §2.15 (`routes_oicp_verify`): a unit judged claim by claim against the stored texts the caller reaches (`readable_corpora`, `requested_corpora`; unconstrained, the sentence-indexed ones). Claims are the unit's sentences. A claim holding a marker of the opinion rule (`data/opinion_markers.toml`, matched by `routes_oicp_verify_opinion`: a first-person view or a judgement, whole words, case-insensitive) is offered as the writer's view and is `not_checkable` (`class/opinion`) before any search or model call; `verifier` names the rule `opinion=<digest of its markers>`. Each other claim's specifics by kind (`figure`, `date`, `quantity`, `name`) come from `sovereign_core::runtime::sentence_specifics`, which reads a number in digits or a cardinal in words ("forty-eight thousand", "a hundred"; valued by `runtime::number_words`, the one number-word reader, shared with deep research's figure tokens; never an ordinal, a lone "one", a bare word under ten, or a scale word after digits), leaves out everything inside an author-date citation, a citekey, a numbered reference or a locator and keeps a figure in any other parentheses; a claim with none is `not_checkable` (`class/no-specifics`). Every checkable claim carries them as `specifics` (unit ranges). Its candidates are the claim's sentences from `routes_knowledge_sentences::sentences`, the one retrieval path sentence search answers from, queried by the claim less its citations and locators (`without_citations`, cut once per claim and read by the search and the checker alike), `CANDIDATES_K` (8) asked, each with its span; a refusal is `could_not_judge` by its name (`sentences/search`). None at or above `verify_floor` (`data/sentence_search.toml`, on the scorer's scale, named in `verifier`) is `unsupported` with no evidence at every tier (`sentences/floor`). `fast` (no model): each specific looked for in the candidates with `value_present_in_chunks` (numbers by value), the misses named in `specifics_absent` as a hint; the tier says `abstained`. `checker`: the gate's per-claim register `claim_chunk_support` over the best `CHECKER_PASSAGES` (3) candidates at `SUPPORTED_TAU` (`checker/sentence-support`), shown the claim less its citations and locators (`sovereign_core::runtime::without_citations`, cut by the same `cited_ranges` the specifics read, whitespace tidied; the wire's claim `start`/`end`/`exact` stay the unit's), on the daemon runtime's inference, which only the operator listener layers on (`client_surface::operator_listener_router`, `VerifyJudge`); elsewhere `could_not_judge: no checker`. A candidate it does not back that states a figure, date or quantity of the claim otherwise (`sovereign_core::runtime::differing_specifics`: same kind and unit, read by value, `48,000` and `forty-eight thousand` alike; a bounded one, "more than 50" (`FigureBound`, from the words before the number), differs only from a figure no value of its range meets; a figure and a date both differing is another period, a date alone needs something else of the claim carried) is `contradicted` with `differing` naming the pair, ranges in the unit and the stored text; the claim is `supported` on any support, else `contradicted` on any contradiction, else `unsupported` with its candidates. A checker call the model queue sheds (`Error::QueueShed`, kept by `claim_chunk_support_checked`) stops the unit, which is answered `503` + `Retry-After` through `admission::local_queue_shed_response` and journals nothing. Every verdict is `uncalibrated`. Advertised as `evidence:verify` with `knowledge.evidence.verify_endpoint` and `verify_tiers`, and `evidence:verify_sentences` beside `knowledge:sentences` when the data file reads; `priority` holds its steps as search's do (§2.17). `svrn verify` is its CLI client, printing a `not_checkable` claim as `not checkable (<rung>)` and counting it apart |
| `POST /oicp/v1/verify/correction` | OICP v0.6 §2.10 (`routes_oicp_verify_journal`): verify keeps each decided claim as a `GroundingLine::Decision` under surface `oicp_verify` in the grounding journal (`sovereign_contracts::types::grounding_journal`), by reference only (rung as `action`, violation = 1 − support, `(corpus, chunk)` handles, `SpecificsTally`), and hands back its `episode_id`; `fast` `abstained` and `not_checkable` keep nothing. This route keeps a client's correction as `GroundingLine::Correction` joined by `episode_id`, after reading its witness range back (`WitnessCheck`). The journal directory is the `VerifyJournal` extension, layered on the operator listener only (`client_surface::operator_listener_router`), so a router without it keeps nothing and refuses corrections `no journal`; `svrn journal grounding` reports each surface apart and counts corrections and the unsupported-with-every-specific-present case. Advertised as `evidence:verify_corrections` with `verify_correction_endpoint` |
| `/api/{version,tags,ps,show,chat,generate,embed,embeddings}` | Ollama-native compatibility shim, pure translation over the OpenAI handlers |
| `POST /internal/ring/sync`, `/v1/rail/*` | The ring rail: anti-entropy, append, log, membership (the one walk `admit` calls, read off the same fold — `svrn ring membership`), and the LIVE lane (delivery, not record — nothing reaches a store or a disk). The journals live at `cw-rails` since fp-54: `/v1/rail/*` dials the rails daemon's doors through the rail port, and a guest's WRITE carries a `GuestAttestation` the daemon signs with its node key for the session's lifetime (`AppState::attest_guest`); rails verifies it against the namespace's roster and a refusal comes back by name (`signer_not_in_roster`, `expired`, …, 403) |
| `/internal/guest/grant`, `…/revoke`, `…/list`, `/internal/client/token*` | Mint / kill / list guest grants and named credentials. On the Operator bind ONLY, and the OWNER's: `client_auth::owner_only` admits a local process presenting nothing where loopback is the owner, or an `admin`-group credential, so a named client on loopback gets 403 `owner-only`. The holder's side, `GET /internal/guest/route` (the base URL its stored link reaches, opening the mesh tunnel; 412/502 named absence otherwise), is serve's loopback router since pb-mesh-exit-mesh (`sovereign_serve::guest_route`) |
| `/v1/mesh/*`, `/v1/admin/*`, `/mcp/*` | Loopback-only. Since 2026-10-08 `/mcp` and every router the daemon merges after the client router sit behind the same `client_auth` layer (`client_surface::operator_listener_router`), so a presented credential decides there too, and a named client's MCP calls carry its principal label into `ToolContext::caller` and the call log of whichever program served the tool: svrn's (`SqliteStateStore::log_tool_call_by`) or code's (`NoteStore::log_tool_call_by`, `notes.db` schema v13's `caller` column), so every MCP call names its harness. One `client_request` span per request carries the principal's label |

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
`sovereign-daemon/tests/main/loopback_parity.rs`.

**Internal API — :9742, plaintext under perimeter trust.** No per-request
auth: gossip, scheduling intent and plans, model transfer, RPC warm, index
shard push/pull/serve, inter-node knowledge search, latency probe. Binds
`0.0.0.0` by default — pin `[daemon] internal_bind`, or create the mesh with
`require_encryption`. **The historical per-session-cert mTLS scaffolding was
removed; never describe `:9742` as mTLS.**

**The loopback guard has three layers and one trap.** Router-level
`from_fn(loopback_only)` middleware, per-handler `ConnectInfo` extraction, and
a pinned listener-shape test. Every layer asks one decider,
`host_kit::locality::RequestLocality`. A loopback peer address is necessary
and not sufficient: the request's `Host` must name loopback, and it must carry
no `Origin` or `Sec-Fetch-Site` of another origin, because a browser on this
machine is a loopback peer. `client_auth`'s loopback admission, the
`X-Principal` owner naming, the internal gate and both `/mcp/stats` handlers
read the same decider. The listener MUST use
`.into_make_service_with_connect_info::<SocketAddr>()` — bare `axum::serve`
leaves `ConnectInfo` absent and the guards fail closed for *every* caller.
The guard lives in the host kit (`host_kit::shell::guard`, re-exported at
`loopback_guard`), whose `shell::serve` always attaches `ConnectInfo`;
cw-rails, the compute child, `meshapp dev` and `ring show` serve through it,
and the daemon's listeners adopt it in pb-daemon-adopts.

### Admission and fairness

Two disjoint layers, so a request meets exactly one and is never double-gated.
`peer_admission_layer` rations traffic that NAMES a node;
`client_fairness_layer` rations traffic that does not, returning early when
`X-Node-Id` is present. Both key the same
`serving_policy_core::fair_sched::SchedCore<Principal>`, which also backs the
daemon's serving part (`sovereign-daemon/src/state/serving/part.rs`), so every
admission gate is fair by identical rules.
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

`ShardManager` (sovereign-grants) decides who takes part in a collaborative
merge and pulls the peers' partitions; the merge and its finalize are
ingest's, reached through `corpus_index::ingest_port::merge::PartitionMergePort`
(pb-grants-merge). The daemon holds ingest as one `Arc<dyn
corpus_index::ingest_port::daemon::IngestPort>` and hands each consumer the
narrower port it names (pb-ingest-dial-daemon-ports). `merge_participants` is the ONE merge implementation, and it
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

`commonwealth-state::MeshStore` is a KV — a pure-Rust in-memory backend, with
the SQLite file store (`MeshStore::open`) behind the default-off `sqlite`
feature — that is a **local PROJECTION of the ring rail, not a replica**.
Writes queue a `rail_outbox` row atomically with the row; the fold picks, per key, the act with the greatest
`(t, actor, id)` among admitted non-voided ops, where `t` is the ORIGINAL
write time, so a snapshot re-append after a seal does not hand every key to
whoever snapshotted last. Acts this build cannot read are COUNTED, never
dropped silently. Retention is part of the fold, because on a projection
nothing else can be: a row a local sweep deletes has no incumbent and returns
on the next round. The node's replicated store and its pump are `cw-rails`'
(`commonwealth-rails/src/kv.rs`): the store is `in_memory()`, so the pump's
first act at start is to rebuild it from the journals or hold nothing at all,
and a peer's ops reach it through the `/v1/rail/ingest` door and are folded on
the next pump tick, not at a ring round. The `mesh-measurements` and `work`
seal arms run on the same tick (`commonwealth-rails/src/plane_seal.rs`, the
daemon's until pb-mesh-exit-mesh), and each tick runs on the blocking pool so
a seal's synchronous snapshot never queues the API behind it (F13); the measurements live set is serve's, whose reconcile loop
(`sovereign-serve/src/measurements_rail.rs`) re-appends it when the journal's
digest moves.
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
`clients/desktop/src-tauri/Cargo.toml`, so the ability is gone, not merely
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
beside their media server: join an invite, run, serve three loopback routes —
plus, since five-programs fp-44, the ring rail's doors
(`/v1/rail/{append,log,live}`) over journals under its own data root, signed
with the same node key the mesh identity uses — and, since five-programs
fp-46, the media-presence poll (credentials under the store
`commonwealth_media::house_dir_under` derives, `GET /v1/mesh/media/presence`
serving the reading into gossip) and the
`/v1/mesh/offers` mount — and, since fp-77, a mesh store of its own:
`/v1/mesh/kv/*` over an in-memory `MeshStore` projected from those journals
and pumped back onto them (`commonwealth-rails/src/kv.rs`; it seals only the
KV namespaces its own outbox feeds, and the daemon keeps serving its own
store until fp-82; since fp-108 a start also rehydrates each local-only
journal on disk through `MeshStore::apply_own_projection`, which merges
only this node's own signed rows; since fp-109 a namespace the
`/v1/rail/ingest` door took new ops for is re-projected on the pump's next
tick, once per namespace, so a peer's write reaches the running store;
since phase-b-5 `RailsDaemon::run` finishes that projection BEFORE the
listener binds, so `/v1/mesh/status` answering means the store is loaded
(`tests/ready.rs`), and the ed25519 stack builds optimized even in dev so
a 12.9k-line store projects in under 2 s, not 118 s;
since pc-solo-durable a door that changed the store drains the outbox onto
the journal before it answers, so an acknowledged write survives a kill
(`tests/kill_durable.rs`), and the tick keeps the seal check;
since fp-87 `svrn portfolio` and `svrn newsworthy` read and write it through
the daemon's `RailsKv`, after migrating their legacy SQLite files once —
`sovereign-cli-mesh kv-export` reads, `sovereign-cli-llm/src/legacy_store.rs`
writes — so neither the daemon nor cli-llm names commonwealth-state). Since fp-78 it also serves the typed ledger doors,
`/v1/ledger/*` (`commonwealth-rails/src/ledger.rs`: contributions, activity,
peer preferences, processed shards, inference state — each the
commonwealth-state writer over that store, the writer's node id in the body)
and runs the contributions `RetentionGc`; the daemon's dialing side is
the daemon's own `ledger_port` + `rails_client/ledger.rs` (moved in from sovereign-mesh at pb-mesh-exit-mesh). Since pb-rails-untether
(phase-b-31) svrn's boot and cli-llm's `rails_kv()` only dial it, and an absent one is a named
absence; ONE opt-in verb brings it up, `svrn mesh up` (sovereign-cli-mesh `rails_up::ensure_rails`),
a `ServingHost` bring-up of the binary `CW_RAILS_BIN`/sibling/PATH names, logging to `<data dir>/rails.log`;
a refused dial never re-ensures, and nothing starts it by default; since pb-mesh-exit-transport `svrn mesh up`
then enables a `cw-rails.service` user unit running the same argv (`rails_unit.rs`, through host-kit's `service`
writer that `svrn install-service` also uses), and doctor's `rails_boot_unit` reads its state. Since phase-b-3 `ensure_rails` runs the daemon's one-time journal
handover (`rail_migration::hand_over`) first, before any bring-up; with a cw-rails already answering it
moves nothing and warns with the namespaces that wait. Until the handover runs, the daemon's boot log,
`svrn daemon start|restart` and `svrn mesh status` name `svrn mesh up` (`node_identity::mesh_handover_notice`,
keyed on the pre-handover `node_key` the identity handover renames; pb-distribution-f8). Since fp-solo-hermetic the bring-up is `run --listen <rails_base port>`,
plus `--local-only` (`[relay] discovery = "none"`) on a local-only node, which also refuses an already
running cw-rails whose `/v1/mesh/status` `relay.n0_services` is true; and `run` exits when its `rails.lock`
is unlinked or replaced. Since phase-b pb-membership it founds (`cw-rails found`), serves its invite as
`join_link` on `/v1/mesh/status`, admits at `/internal/join` through commonwealth-discovery's
`accept_join_with_identity` (an admission it cannot persist is refused with a 500 naming the data dir,
and rolled back), and speaks mDNS by key (`run --mdns`; a dial-less join browses for a keyed
founder), all keyed by its own node key; its closure did not grow (319 crates vs 743 at the 2026-09-11
measure; the rail doors cost three more). Since phase-b pb-rails-membership a running cw-rails also
serves the daemon's membership doors (`/v1/mesh/{create,join,join/preview,rotate,leave,switch,forget}`,
`commonwealth-rails/src/membership.rs`) over its own known-mesh store (`known.rs`, parked meshes under
`<data-dir>/meshes/<id>/`), listed as `meshes` on its status. A mesh a door makes active is
self-stamped before anyone can read it (`RailsDaemon::swap_membership` → `gossip::stamp_before_publishing`),
so a joiner admitted before the founder's first round on it still leaves with the founder's dial — without
it, a join inside that window on a local-only mesh never gossiped (2026-10-03) — and a stamp never moves our
row's event time back (`self_stamp` writes `now.max(event_time + 1)`, as a departure does). Since phase-b pb-rails-origins its acceptor holds no arm per
protocol: `/v1/mesh/origins` registers any program's loopback origin (an ALPN, or `cwth/http/0` path
prefixes) in commonwealth-media's `OriginRegistry`, from which the acceptor table, the advertised ALPNs
and the gossiped capabilities are all read (a renew carrying `claims` replaces its registration's
declaration, pb-mesh-exit-transport-claims); each registration is handed a tie (`X-Mesh-Tie`) its origin
checks with `tied_pubkey`, and an unregistered ALPN or prefix is refused by name. svrn registers its peer
routes there (`sovereign-daemon/src/peer_origin.rs`), declaring the gossip round's capabilities, anchor
excepted, and the ring namespaces its pages drain (`live_namespaces`: each live grant's rail namespace,
the wall's for a wall grant) at every 10 s renew — a renew carrying `namespaces` replaces them, and
cw-rails' live lane buffers a peer's payload only for a declared namespace (`ring_routes.rs` `ring_live`);
its internal resolver believes a forward carrying
that registration's live tie as it believes its own acceptor's mark. Outbound (pb-rails-reach),
`GET /v1/mesh/reach?peer=&class=` answers any peer's endpoints for a traffic class from its own transport,
and the `mesh-reach` leaf's `RailsTransport` is the `PeerTransport` that asks it, so a program that is not
the mesh endpoint dials peers through this one. Since phase-b pb-rails-parity it runs the ring round itself
(`commonwealth-rails/src/ring_sync.rs`, moved from sovereign-mesh; the daemon runs the same code over its
rail port until the flip), woken by its append door, and serves `/internal/ring/{sync,live}` to members, each
path registered by name, and `/internal/ring/checkpoint/{ns}` to local callers only (`ring_routes.rs`) and `GET /v1/mesh/relay-candidates`
(address discovery, moved to `commonwealth-discovery::mesh_discovery`). It also runs the reachability
watchdog (`iroh_watchdog.rs`, moved from sovereign-mesh) over its own endpoint, whose rebuild re-binds and
swaps the endpoint, transport and acceptor as one (`self_heal.rs`); `/v1/mesh/status` carries
`self_reachability`.

---

## 6. How the four projects fit together

**Sovereign standalone** — Tauri / CLI against `EmbeddedLlamaCpp`,
knowledge via `MeshCorpusManager` (named for the mesh case but works without
one).

**cmnwlth standalone** — daemon on `localhost:9741`; any OpenAI-compatible
client points at it. Ingest uses `embed_http::http_embed_fn`, so a node with
no local embed model still indexes.

**Integrated** — `EmbeddedDaemon` runs cmnwlth in-process; runtime inference
is wrapped in `InferenceRouter`, built once by serve (`sovereign_serve::rank`)
and handed to svrn by the stock distribution, which OICP-routes synthesis to
peers when scoring favours them; svrn alone ranks nothing and relays, and a
standalone serve ranks its own OpenAI face over cw-rails' roster (`RailsVenues`),
reaching peers' member clients on `cwth/client/0`. Both sides share `oicp_select`, so the Joiner's selected
model and the Founder's served slot cannot drift. Skills with
`privacy = "local_only"` short-circuit to local.

**Desktop attach mode** — both the desktop and `svrn daemon` want :9741. The
desktop probes `/v1/models` and on success enters Attach: inference through
`RemoteApiProvider`, mesh mutations over HTTP, `save_config` POSTing
`/v1/admin/reload`. **Boot is gated on identity, not on a port** —
`ClientListener` is a watch (`Pending` / `Bound` / `Failed`) and
`/status.process` carries `pid` + `run_id`, so a caller can ask WHO answered.
On the dialing path `/status.serve_reach` names the follower's last read of
serve's self-report (`answered`, `unreachable`, `did_not_answer_in_time`) and
its age; `/status` never dials serve itself.
A fixture daemon that loses the port, keeps running and logs success used to
probe green while the app ingested into the operator's real daemon.

`/v1/admin/reload` rebuilds only what changed: the three model slots swap
atomically via `ProviderFactory` (on the dialing path serve rebuilds, and the
reload refuses, naming each slot, when serve's self-report does not hold what
`[models]` asks for); `client_port`, `internal_port`,
`client_bind`, `client_token` and `data.dir` answer `restart_required: true`;
`[iroh] enabled`, `transport`, `media_origin` and `media_allow`, which nothing
in svrn reads since cw-rails became the mesh endpoint, answer `unread_fields`.

---

## 7. Build, test, run

Prerequisites: Rust stable, `cmake` (llama.cpp), `protoc` (LanceDB). For
cmnwlth, `llama-server` + `rpc-server` on `PATH`. For desktop, Node.js +
Tauri 2.

The repo is **one unified Cargo workspace** — every crate a member under the
root `Cargo.toml`. The program dirs (`svrn/`, `serve/`, `cmnwlth/`,
`ingest/`, `code/`, `bench/`), `shared/`, `clients/` and `distributions/`
are directories of member crates, not separate workspaces.

```sh
cargo build --workspace                    # recipes + assets compiled in by corpus-engine-recipes
cargo check --workspace --all-targets      # what CI's `check` job runs

# The CLI spans 5 binaries — rebuild all of them, since editing one and
# rebuilding only the dispatcher is a silent no-op:
cargo build -p sovereign-cli -p sovereign-cli-daemon \
            -p sovereign-cli-dev -p sovereign-cli-llm -p sovereign-cli-mesh
```

For local deployed-daemon iteration use `scripts/dev-release.sh` rather than
plain `--release`: same opt-level, LTO and codegen-units overridden via env,
so a one-line change costs seconds instead of ~7.5 minutes. A custom cargo
profile cannot do this — `llama-cpp-sys-4`'s build script panics under any.

**The gate is the two scripts**, not bare cargo — they resolve the repo's real
feature contract (`corpus-engine/treesitter` + `sovereign-cli/dev-tools`, and
the per-crate flags `scripts/lib/cargo-scope.sh` resolves) and carry guards
bare cargo has no equivalent of.

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

Tests that boot a daemon, in process or as the binary's `run`, share
nextest's bounded `daemon-boot` group (`.config/nextest.toml`). Its filter
is derived from test source by `binary_boot_rails_census.rs`, which fails
when a booting file is outside it or a clause names none.

No tests require GPU, models or network. Sovereign uses
`DeterministicInference` + in-memory SQLite + real FTS5; cmnwlth's harness
runs simulated meshes deterministically.

**`scripts/pre-push.sh` is the primary gate, CI second** — a metered gate that
aborts on a billing failure is nearly indistinguishable from one that passed.
Held to a one-minute budget, it scopes to the diff and runs rustfmt, the
compile, the eight blocking xtask ratchets (docs / arch / boundary / layer /
lock / layout / env / concept) and the desktop node gates concurrently, then
two advisory size ratchets and the advisory clone ratchet. Install via `scripts/install-git-hooks.sh`, which
points `core.hooksPath` at the version-controlled `.githooks/`. It fails
closed: a push range it cannot diff gates everything.

**A ratchet failure is not fixed by `--update-baseline` on your working tree**
— that absorbs your own growth along with everything else. Re-pin at
`origin/main` and say in the commit body what the lines bought: the baseline
diff is the record, and there is no separate ledger file.
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

---

## 8. Where to look for what

| You want to | Read |
|---|---|
| Understand the agent runtime | `sovereign-core/src/runtime.rs` + `runtime/handlers/` |
| See how plans are executed | `sovereign-core/src/executor.rs` |
| Add a tool | `sovereign-contracts/src/traits.rs`, a file under `sovereign-tools/src/`, a `[[tool]]` block in `sovereign-contracts/tool-manifests/` |
| Run a workflow | CLI `svrn workflow run` → `workflow-host::run_workflow_in_process`; desktop `workflow_commands.rs` → `run_workflow_with_provider` |
| Add a corpus extractor / filter | `ingest/crates/corpus-engine/src/extractors/` then register in `engine/ingest.rs`; `src/filters/` + `recipe.rs::FilterConfig` + `filters/loader.rs` |
| Bundle a generated data file | `sovereign-recipes/<corpus>/data/`, a `pub const` `include_bytes!` line in `ingest/crates/sovereign-recipes/src/lib.rs`, a key row in `ingest/crates/corpus-engine/src/recipe_source/bundled.rs::ASSETS` |
| Write a recipe | `sovereign-recipes/<id>/recipe.toml`, then `registry.toml` |
| Add an investigation recipe | `enrichment.type = "investigation"` + `[[entity_types]]` + `[[relationship_types]]` + `[[patterns]]` |
| Write a skill / tune models per hardware | `svrn/modes/<id>/skill.toml`; `shared/crates/sovereign-contracts/data/models.toml` |
| Understand the SCIP call graph | `shared/crates/corpus-engine-scip/` (`scip_graph.rs`, `scip_export.rs`) |
| Classify a symbol / detect trait dispatch | `shared/crates/corpus-engine-scip/src/descriptor.rs` — the ONE decider. Do NOT read `symbols.kind` (88.7% `unknown`) or `refs.ref_kind` (100% `direct`) |
| Find a duplicated concept | IDENTITY `svrn code converge census` / `noun <Name>`; ROLE `converge roles`; SHAPE `converge shape`. Duplicated BEHAVIOUR is `code dry-report`; oversized FILES are `code suggest-seams` |
| Understand index storage on disk | `shared/crates/corpus-index/src/index/mod.rs` |
| Understand the v2 atlas pipeline | [`ingest/crates/corpus-engine/ENRICHMENT_V2.md`](../ingest/crates/corpus-engine/ENRICHMENT_V2.md) + `enrichment/pipeline/mod.rs` |
| Drive v2 enrichment / build inside the daemon | `sovereign-pipeline/src/enrich_cmd/`; `enrich_now` (`sovereign-tools/src/local_corpus/atlas_dispatch.rs`) |
| Understand delta updates / scope expansion | `ingest/crates/corpus-engine/src/update/delta.rs`, `engine/expand.rs` |
| Understand KnowledgeView | `sovereign-tools/src/knowledge_view/`; injected at `LandscapeDigestProvider::splice_landscape_digests` |
| Run the long-running daemon | `sovereign-cli-daemon/src/daemon_cmd/` + `sovereign-service/data/` |
| Serve something the desktop used to compute in-process | the client-router families in `sovereign-daemon/src/*_http.rs` — §5 |
| Find which program owns a crate, or where a new one goes | `[crate_dirs]` in `quality/ARCH_LAYERS.toml`; `cargo xtask boundary-gate` fails a member outside its row's directory |
| Read a path spelled before the top-level-programs move | `quality/top-level-moves.toml` through `scripts/lib/top_level_moves.py` (`back` / `forward`) — the one table the history-reading instruments use |
| Prove a deleted twin cannot come back | `scripts/twin-census.py` over `quality/twin-plants.toml` |
| Prove desktop and CLI answer one question alike | `clients/desktop/tests/e2e/real/journeys/surface-parity.journey.spec.ts` |
| Trace a `/v1/chat/completions` end-to-end | `cmnwlth/docs/routing-field-guide.md` |
| Understand OICP routing | `shared/crates/oicp-types/src/lib.rs` + `sovereign-scheduler/src/oicp_select.rs` + [`svrn/docs/inference.md`](../svrn/docs/inference.md) |
| Point an outside tool at the daemon | [`docs/INTEROP.md`](INTEROP.md); [`docs/INTEGRATION_SURFACES.md`](INTEGRATION_SURFACES.md) for which surfaces are contracts |
| Deploy to a shared air-gapped box | `distributions/deploy/onprem/` — **read `EGRESS.md` before claiming this system makes no outbound connections** |
| Rent a GPU by the minute | [`docs/CLOUD_PEER.md`](CLOUD_PEER.md); `scripts/dev-pod.sh`. A `--mesh` flight puts the join link on third-party hardware — end it with `svrn mesh rotate` |
| Know which CLI use cases are promised | `svrn/docs/cli-contract.toml` — `[[command]]` the verb surface, `[[journey]]` the sequenced use cases, `[[experience]]` the promises |
| See what the CLI promises and how much can fail | **`svrn contract`** (`map` / `census` / `nightly`). `census` splits the manifest into steps a lane RUNS and steps nothing runs, because a step in a never-run journey is a written intention |
| Judge architecture health at a glance | **`svrn code fieldglass [corpus] --open`** — one deterministic self-contained HTML, evidence only: no scores, no gates. [`docs/FIELDGLASS.md`](FIELDGLASS.md) |
| Price or execute a refactor | `svrn code refactor plan` / `gate` / `status`; `code suggest-seams <file> --plan` → `cargo xtask refactor-apply`; `code wire-check`. Process [`quality/REFACTOR_FACTORY.md`](../quality/REFACTOR_FACTORY.md) |
| Judge the judgment, not just the code | `gym/comaintainer/` + [`docs/COMAINTAINER.md`](COMAINTAINER.md); landing seat `scripts/co-review.sh`. Its `design_reuse/` lane compares bounded BASE-only source discovery with supplied dossiers, freezes run inputs, and reports reference/label agreement separately from unjudged semantic adequacy |
| Check an order's extension references in another source repository | `scripts/co-order.sh check <id> --repo <path>`; `CO_FEATURES` selects the order directory, while `--repo` selects the Git tree for revision/citation checks. This checks reference validity — including that a `home:` holds the names it claims and a citation's span names what the claim names — not ownership suitability or approval |
| Is any quality subsystem's posture stale? | **`svrn posture`** — one table: drift / arch / capability / contract-nightly / watchers / env-gate / bench baselines, each row naming its refresh command |
| Is the resident stack BROKEN right now (not drifted)? | **`svrn quality check [--lane <id>]`** — the curated ~30-minute check. Lanes are DATA in `quality/instruments.toml`; each states its verdict as a `kernel_types::Judgement` on its last stdout line. `--distribute` runs the same selection as work on the `work` ring |
| Did my change regress retrieval / routing / synthesis / enrichment? | **`./scripts/sovereign-ci-bench.sh`** (~2-4h) — the FULL nightly, where drift against committed baselines is judged |
| A bench says regressed — real or noise? | [`svrn/docs/RUNBOOK.md`](../svrn/docs/RUNBOOK.md) §6 — per-lane noise bands, baseline-age semantics, the legitimate re-mint path |
| Pick the next daemon test to write | [`svrn/docs/TESTING_SURFACE.md`](../svrn/docs/TESTING_SURFACE.md) |

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
`quality/env-flags.toml`, enforced by `cargo xtask env-gate`; a `removed` row
warns at startup when still set (`kernel_types::env_bridge::REMOVED_ENV`,
printed by `promote_legacy_env`).

**Committed contracts (versioned, reviewed):**

| Surface | What it declares | Writer |
|---|---|---|
| `quality/ARCH_LAYERS.toml` | crate layer map, exceptions, package boundaries, where each crate lives | humans |
| `quality/top-level-moves.toml` | the top-level-programs move, old path → new, for instruments that read history | generated once; humans |
| `quality/env-flags.toml` | the env-knob registry | humans |
| `quality/baselines/` | shrink-only ratchet baselines | **machine only** |
| `quality/CONCEPTS.toml` | the concept register — one canonical owner per noun | humans |
| `quality/TARGET_ARCHITECTURE.md` | the noun-convergence destination; four regions generated | `cargo xtask target-arch`; humans for the prose |
| `quality/source-tree.toml` | the residual "not our source" dirs a gate walk must skip | humans |
| `quality/requirements.toml` | the 625-requirement registry, carrying `spec_hash` | **machine only** |
| `quality/requirements-enforceability.toml` | the ONE hand-authored column: how each can be settled | humans |
| `quality/conformance/<crate>.toml` | which test claims each requirement, from `covers:` tags | **machine only** |
| `quality/instruments.toml` | every instrument and every trigger venue | humans |
| `svrn/docs/cli-contract.toml` | CLI verbs, journeys, experiences | humans |
| `shared/crates/sovereign-contracts/data/models.toml` | model selection per hardware | humans |
| `ingest/crates/sovereign-recipes/registry.toml` | recipe registry | humans |
| `clippy.toml` | lint budgets + the path-SSOT ban | humans |

**Repo-local `.sovereign/`:** `project.toml` + `project.json`, `sovereign.toml`
(per-repo daemon/watcher posture — watchers deliberately off in this repo),
`notes.db`, `features.db`, `SOVEREIGN.md`. (No `mesh.db` since five-programs
fp-33: the work-atlas store is cw-rails', dialed by the code program.)

**Per-user root `~/.svrnmesh`:** `config.toml` (`SetupConfig` — THE per-user
config), `work-atlas.toml`, `projects.json`, the indexes / drift / arch /
capabilities / sessions trees, plus models, corpora, recipes and logs,
`daemon.pid` and `worker_owner_key.bin`; `sovereign.db` is svrn's store,
its memory notes included (lessons, the tool-decision dossier, commitments,
svrn's MCP call log), and `notes.db` is the code program's (pb-notes-memory).

**`[models]` is optional.** Absent — or present naming no primary — plus a
`[node] entry` is `NodeClass::Terminal`: a full mesh member holding no weights
that forwards what it cannot do to a named entry node. A remote `[engine]`
with no `[models]` (what `svrn setup --hosted` writes) is a `Holder`: it
serves turns from the vendor model it names, which `primary_model_stem()`
returns. The class is DERIVED by
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
  **Registry** is the catalog at `ingest/crates/sovereign-recipes/registry.toml`.
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
  `shared/crates/corpus-engine-scip/src/tool_path.rs`, the ONE decider for "where is this
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
- **Ring rail** — the append-only, Ed25519-authored total order per namespace
  carrying mesh state, work and measurements. **Work atlas** is cross-mesh
  peer awareness: `work_in_flight` / `declare_scope` / `release_scope`.

---

## 10. Architecture roadmap

Work intentionally deferred, so the next engineer inherits a todo list rather
than a surprise. A big file or a documented gap without an entry is a bug; one
with an entry is sequenced work. When an entry completes its chronicle moves
to [`HISTORY.md`](./HISTORY.md) and the row is dropped.

**The acceptance record is the arch-gate baseline diff in `quality/baselines/`;
there is no separate ledger file.** The deferral tables left this file because
they are an append-only record keyed to the arch-gate workflow — the gate tells
you to re-pin, and the re-pin diff is the acceptance, which is a ledger's job
and not a map's. `quality/` is where the gate baselines already live.

When `cargo xtask arch-gate` reports a NEW oversized file, add a row there and
re-baseline, or split the file. Three standing classes live in that ledger:
**Sovereign deferrals** (per-file split debt), **corpus-engine deferrals**
(files that shrink by carve-out under `ingest/crates/corpus-engine/DECOMPOSITION.md` rather
than by a local split), and the dated **size / fan-in acceptance** rows, each
naming what the lines bought.

Doc posture: this file states what IS and is gated by `cargo xtask docs-gate`,
which resolves every repo path it cites. The narrative reconciliation above
that — drift, capability-reconcile, check-spec — is mesh-side and advisory.
