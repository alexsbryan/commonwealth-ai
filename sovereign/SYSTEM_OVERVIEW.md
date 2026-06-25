# Commonwealth AI — System Overview

A navigation primer. Read this on day one to know what exists, how
the pieces fit, and where to look for each subsystem. Read
[`sovereign/ARCH_PRINCIPLES.md`](./ARCH_PRINCIPLES.md) on day two for
the rules of engagement.

This file is a contract per `ARCH_PRINCIPLES.md §1.1`: every claim
must be verifiable against the code on the commit it appears in. If
you change a subsystem, update its entry in the same PR.

---

## 1. The projects

```
commonwealth-ai/
├── oicp-types/                # OICP wire types — no other deps
├── corpus-engine/             # Knowledge layer (LanceDB + Tantivy)
├── corpus-engine-scip/        # SCIP call graph + per-language exporter dispatch
├── corpus-engine-notes/       # NoteStore + project_docs index (carved out of corpus-engine)
├── corpus-engine-atos/        # ATOS feature store + plan items + design signals (carved out) — opt-in behind `--features atos`
├── corpus-engine-archaeology/ # Git archaeology + rough-edges + atom-provenance (carved out)
├── sovereign-recipes/         # Canonical recipe TOMLs + catalog + data lists (vendored into corpus-engine at build)
├── sovereign/                 # Local AI assistant (CLI / desktop / server)
├── commonwealth/              # Mesh coordination daemon
├── packages/chat-ui/          # Shared Svelte chat render surface (desktop + mobile)
└── sovereign-mobile/          # Thin Tauri 2 mobile client (iOS + Android), tailnet or iroh dial-by-key
```

| Project              | Role                                          | Depends on                                            |
|----------------------|-----------------------------------------------|-------------------------------------------------------|
| `oicp-types`         | OICP v0.3 wire types + scoring helpers        | —                                                     |
| `corpus-engine`      | Acquire → extract → filter → chunk → embed → index | `oicp-types`, `corpus-engine-scip` (treesitter feature), `corpus-engine-notes`, `corpus-engine-atos` |
| `corpus-engine-scip` | SCIP call graph store + exporter dispatch     | —                                                     |
| `corpus-engine-notes`| NoteStore + project-docs index + notes↔alignment sync (carved out of corpus-engine for blast-radius control) | `rusqlite` |
| `corpus-engine-atos` | ATOS feature store + plan items + DESIGN.md design signals (carved out). **ATOS is an opt-in experiment** behind the `atos` Cargo feature — the recipe-author workspace uses `sovereign-store::RecipeProjectStore` instead, and default product builds (server/desktop/daemon/cli) carry zero ATOS | `rusqlite` |
| `corpus-engine-archaeology` | Git history mining + rough-edge surfacing + atom-provenance eval (carved out) | — |
| `sovereign-recipes`  | Canonical recipe TOMLs + catalog + data lists (vendored into corpus-engine at build) | —                                       |
| `sovereign`          | Local agent runtime                           | `corpus-engine`, `corpus-engine-scip`, `oicp-types`   |
| `commonwealth`       | Symmetric mesh daemon                         | `corpus-engine`, `oicp-types`                         |

Dep direction is one-way. Sovereign optionally embeds Commonwealth
in-process via `sovereign-mesh` — the only place the two upper
projects meet.

```
       oicp-types          sovereign-recipes
            │              (recipe TOMLs + data/)
            │                       │ build.rs include_bytes!
            │                ┌──────▼──────┐
            │                │ corpus-engine│  (LanceDB + Tantivy)
            │                └──────┬──────┘
            │                       │  EmbedFn / InferenceFn
            ├───────────┬───────────┼──────────────┐
            │           │           │              │
        Sovereign       │      both call          Commonwealth
       (sovereign/)     │   identical APIs        (commonwealth/)
            │           │                              │
            └─ sovereign-mesh (in-process embed) ──────┘
```

Two shared protocols cross the Sovereign/Commonwealth boundary:

- **OICP** — declared in `commonwealth/docs/oicp-v0.3.md`; types
  in `oicp-types/src/lib.rs`; re-exported as `sovereign_core::oicp`
  and `commonwealth_core::oicp`. Downstream crates use the
  re-exports, never the types crate directly.
- **`EmbedFn` / `InferenceFn`** — `corpus-engine` accepts these
  closures from any caller; each project supplies its own
  implementation.

---

## 2. Workspace map

One line per crate. For folder-level detail, `ls` the crate's `src/`
or read its `lib.rs`. See `sovereign/docs/` for subsystem deep
dives.

### corpus-engine

A self-contained library between "raw source on the internet" and
"ranked search hits with provenance." See
[`corpus-engine/README.md`](../corpus-engine/README.md),
[`ENRICHMENT_V2.md`](../corpus-engine/ENRICHMENT_V2.md),
[`ATLAS.md`](../corpus-engine/ATLAS.md), and
[`INCREMENTAL_ATLAS.md`](../corpus-engine/INCREMENTAL_ATLAS.md).

Major modules under `corpus-engine/src/`:

- `engine/` — `CorpusEngine` façade (`ingest`, `expand`, `reindex`)
- `acquirers/`, `extractors/`, `chunkers/`, `filters/` — pipeline stages
- `asset_store/` — content-addressed filesystem store for binary
  payloads (raw bytes + optional typed parsed-form caches +
  append-only ledger). Architecture-over-Enron AD-1; the substrate
  the described-asset dispatcher and future calendar /
  transactions / sensor verticals share.
- `recipe.rs`, `registry.rs` — TOML schema + recipe catalog
- `index/` — LanceDB (IVF-PQ) + Tantivy FTS, `IndexMeta`, `ScopeMeta`
- `enrichment/` — v1 field-engine (5-phase domain pipeline) and v2
  atlas (typed atom graph; `Pipeline` trait + registry + exemplar
  bank). See `ENRICHMENT_V2.md`. Plus `enrichment/reconciliation/`
  — the multi-origin merge primitive (Phase 4 of the architecture-
  over-Enron push) with reversible oplog + pluggable merge signals.
  Signals are identity-grade only (exact name fold, nickname /
  initial-surname, exact shared email or email-alias, org+role); the
  fuzzy email↔name and bare-name-alias paths were removed after they
  chained 2,013 polluted atoms — Lay + Skilling + Fastow + every org —
  into one cluster (train B³ precision 0.26 → 1.00 once removed).
  `candidate_pairs` blocking keeps the O(n²) scan sub-second on the
  18.8k-atom multi-wide corpus (~650× fewer pairs, behaviour-identical).
  Corporate-suffix normalization (`strip_org_suffixes`, Institution-only)
  collapses "El Paso" / "El Paso Corp." / "El Paso Corporation" while
  keeping distinct bases apart, lifting train B³ recall 0.66 → 0.75
  (F1 0.86, precision held at 1.0). `sovereign bench enron diagnose`
  is the glass-box: per-gold coverage + cluster spread + over-merge
  bridges for the tuned policy.
- `atlas_traversal/` — query layer over atlas graphs
- `update/` — code/file watchers, delta updates, lint/test watchers
- `meta_atlas/` — cross-corpus articulation classifier + index
- `pii.rs`, `alignment_projector.rs` — operator-facing scanners
- **Carved out into sibling crates** (see §1): NoteStore +
  project-docs index → `corpus-engine-notes` (`notes.rs`,
  `project_docs.rs`); ATOS FeatureStore + plan items + design
  signals → `corpus-engine-atos` (`features.rs`, `plan_items.rs`,
  `design_signals.rs`); git archaeology + rough-edges + provenance
  eval → `corpus-engine-archaeology` (`git_archaeology.rs`,
  `rough_edges.rs`, `archaeology_eval.rs`)

### sovereign

```
crates/
├── sovereign-core           # Traits, runtime, planner, executor, router, memory
├── sovereign-inference      # llama.cpp slots, remote OpenAI-compat, hybrid w/ failover
├── sovereign-store          # SQLite + Postgres + in-memory StateStore
├── sovereign-tools          # Built-in tools (search, knowledge, docs, web, MCP, code-intel)
├── sovereign-workflow       # Step·Artifact·Runner — typed dataflow over local-model steps (P0+P1 + content cache + `for_each` collection-map; `sovereign workflow run`). Diffed byte-for-byte against the real corpus chunk→embed stage. Owns the `StepKind`/`WireKind` wire-kind catalog the authoring schema derives from (§2.1 source of truth).
├── sovereign-workflow-host   # Daemon-runnable workflow host — assembles the standard tool registry + daemon inference + content cache to run a workflow in-process; the catalog/resolve surface; the living trigger; the `recipe:` corpus-ingest stage; and the NL workflow-author tool bundle (`workflow_write`/`_write_structured`/`validate`/`test`, the JSON-Schema-constrained author mirroring recipe-author).
├── sovereign-atos           # ATOS lib (charter, approval, report, session, local orchestrator) — opt-in experiment behind `--features atos`; no product crate depends on it by default
├── sovereign-work-atlas     # Coordination atlas for agents on the mesh
├── sovereign-mesh           # In-process Commonwealth embed
├── sovereign-server         # Axum REST + WebSocket, multi-tenant + approvals
├── sovereign-desktop        # Tauri 2 + Svelte 5
├── sovereign-cli            # User-facing dispatcher — execs into sibling binaries
├── sovereign-cli-shared     # Tiny shared lib (dirs, repo, help, prompts, tracing init)
├── sovereign-cli-daemon     # Long-running host + lifecycle (~241 MB binary)
├── sovereign-cli-dev        # Workbench: ATOS + project lifecycle + code intel + tools
├── sovereign-cli-llm        # Model interaction + heavy retrieval (chat/bench/eval/atlas/…)
├── sovereign-pipeline       # Pipeline / pod-lifecycle helpers
├── sovereign-eval           # Eval surfaces
├── sovereign-agent-bench    # Eight-problem agent-coding battery
├── commonwealth-agent-tools # Canonical agent-tool primitives (cross-runner contract)
└── commonwealth-tdd         # Unified TDD solver loop (HTTP + MCP transports)
```

Top-level: `modes/` (skills — recipe-author, inner-work),
`models.toml`, `models/`, `bench/`, `inquiries/`, `router/`,
`sovereign-server.toml`.

### commonwealth

```
crates/
├── commonwealth-core         # Shared types — ids, mesh, capabilities, ledger, aliases
├── commonwealth-transport    # PeerTransport seam — (peer, traffic class) → endpoints; IP today, iroh-ready
├── commonwealth-discovery    # mDNS, gossip, latency probe, hardware, TLS, peering
├── commonwealth-inference    # Scheduling + orchestration
├── commonwealth-api          # HTTP servers (client 9741 + internal 9742 mTLS)
├── commonwealth-knowledge    # corpus-engine integration over the mesh
├── commonwealth-app          # Mesh-app platform (manifest, lifecycle, proxy)
├── commonwealth-state        # MeshStore — gossip-replicated SQLite KV w/ TTL GC
├── commonwealth-daemon       # CLI entry + signal handling
└── commonwealth-test-harness # SimulatedMesh, SimulatedNode, MockLlamaServer
```

`contrib/` ships `install.sh`, systemd unit, launchd plist.
`docs/oicp-v0.3.md` is the canonical OICP spec.

### sovereign-recipes

The **single source of truth** for corpus recipes — recipe definitions only.
corpus-engine vendors this tree at build time (`build.rs` → `OUT_DIR`) for the
offline bundle, so there is no second copy. Catalog is `registry.toml`; field
reference is `SCHEMA.md` (generated from `corpus-engine/src/recipe.rs` and gated
by the `recipe_schema` test); `GETTING_STARTED.md` + `_templates/` onboard
contributors.

Current set: `wikipedia`, `wikipedia-simple`, `wikipedia-newsworthy`,
`wikipedia-article`, `wikipedia-catalog`, `sep`, `stackexchange`,
`stackexchange-knowledge`, `openalex`, `gutenberg`, `gutenberg-work`,
`crs_reports`, `alignment`, `codebase`, `conversations-anthropic`,
`conversations-chatgpt`,
`scotus-opinions`, `olc-opinions`, `federal-register-presidential`,
`us-code`, `arch-principles`, `system-overview`. Underscore directories like
`_templates` carry scaffolding.

Bench/eval harnesses (`knowledge-gym`, `search-gym`, `routing`, `book-report`,
per-corpus question banks) live under `sovereign/bench/`, not here — the gym
*commands* (`sovereign search-gym`, `knowledge-gym`) still exist; only their
fixtures moved.

The **Reasoning-Fidelity Validation Harness** (`sovereign bench
mechanism-fidelity`) is a different shape of bench: a *metamorphic* audit of
whether a frozen model reasons from a causal **mechanism** or from a memorized
label. It is organized as a **registry of reasoning classes** behind one
generic orchestrator. Pure logic lives in `sovereign-eval/src/mechanism_fidelity/`:
the `ReasoningClass` trait (`class.rs`) + `registry.rs` resolve `--class <id>`,
and each class emits a flat list of finished `RenderedProbe`s (a base case + its
perturbations × full / stripped-control render, each carrying the structural
prior's probability). Three classes ship today — `wealth_tax_relocation`
(synthetic logistic prior; DIR-P1 anti-gestalt collapse / DIR-P2 saturation /
INV-I1 identity invariance), `attribution_support` (corpus-grounded: mines
`Claim` atoms + their evidence from a corpus's `atlas/atoms.json`, exact 0/1
oracle, blindfold negative control via withheld passage), and
`aggregation_threshold` (synthetic counting-under-a-threshold). The
class-agnostic `score.rs` scorer + three-pool discipline are shared.

The inference-coupled orchestrator (`bench_cmd/mechanism_fidelity.rs`) elicits a
forced-choice **logprob** distribution in ONE forward pass per probe (the
candidate set rides inside `structured_output` as a sentinel the daemon's
embedded path reads off the masked next-token logits — `model_slot.rs`), maps it
to a scalar via `class.target_prob`, and scores each perturbation's `d_agent`
against the structural `d_struct`. Elicitation is **sequential** so byte-identical
control prompts stay deterministic — the negative control's "provably blind"
guarantee (its `d_agent` must be exactly 0). Train/Dev run **anytime-valid
early-stopping** (empirical-Bernstein confidence intervals read at a pre-
registered checkpoint schedule, `stopping.rs`): a model is resolved and its
remaining cases skipped the instant the overall verdict is decided (any required
band fails → NO-GO; all pass → GO). Each run distils a per-`(model, class)`
**fidelity card** (`card.rs` → `~/.sovereign/model-fidelity-cards/<model>.json`,
stamped with the manifest fingerprint so stale bands invalidate it) — the
"characterize once, read free per query" artifact. It reuses
`entity_resolution_bench::PeekBudget` for the sacred test pool and emits
`ResultRow` JSONL read by the Python verdict sidecar. See
`sovereign/bench/mechanism_fidelity/README.md`.

The **Chaos-Monkey** bench (`sovereign bench chaos-monkey`) is the
calibration counterpart: where every other bench measures competence *when the
corpus can answer*, this one measures the situated-agent property of answering
capably + cited **when the facts are in persistence** and abstaining honestly
**when they aren't** — without being fooled by distractors. Pure logic
(`sovereign-eval/src/chaos_monkey/`: a question schema whose *fairness contract*
is enforced at load — answerable items must ship a witness, absent items must
not — plus a **two-red-line scorer** that never blends competence-when-present
and honesty-when-absent into one number, so neither a hallucinator nor a
blanket-abstainer can game it). The orchestrator
(`bench_cmd/chaos_monkey.rs`) drives the live `handle_message_stream` path
sealed to one corpus via `enabled_corpora`, classifying answer-vs-abstain with a
forced-choice judge and checking everything else deterministically against the
bank's witnesses. Its sealed corpus installs under a **machine-stable**
corpus_id via a committed recipe (`sovereign-recipes/chaos-secret-agent/`,
installed by `scripts/setup-chaos-corpus.sh`) rather than a path-hashed
`corpus watch`, so the gate is reproducible across boxes. See
`sovereign/bench/chaos_monkey/README.md`.

The **Governance** bench (`sovereign bench governance`, FR-9) gates the
event-sourced common-law tool — the `govern` verbs (`seed`/`tensions`/`resolve`/
`accept`/`ask`) over a corpus's `GovernanceView` + `GovernanceOplog`. Two lanes
share the chaos tracked-run + gate pattern. **Lane A** (`run`/`diagnose`) is a
precision/recall *detector* bench: it maps each `EdgeType::Tension` edge in the
enriched atlas to a pair of source sections and scores against an exhaustive
`truth.json` (pure scorer `sovereign-eval/src/governance_bench.rs`). **Lane B**
(`qa`) reuses the chaos two-red-line scorer over the governance corpus: because
the sealed corpus carries a `governance_oplog.jsonl`, the live turns become
*governance turns* — the gated active-set step in `shared_core_steps()`
(`runtime/retrieval_pipeline.rs`) drops the retrieved chunks of any *amended
section* (`GovernanceView::dead_law_sections`, bridged to chunk row ids via
`chunk_to_section_map` over `chapters.json`, since atoms cite section ids) and
the cite-or-abstain gate runs as `GateSurface::Governance` — so the bank's
`SupersededTrap` rows add a **third red line, RL-3 (no dead law)** alongside
RL-1 (no confabulated rule = `hallucination_rate`) and RL-2 (honest abstention
= `honesty`). Lane B drives the *same hardened turn* `govern ask` ships — intent
pinned to a factual lookup + the governance answering discipline — via the
general bench knobs `--pin-intent` / `--custom-instructions`, so the metric
tracks the shipped tool, not a bare chat path. Dropping is section-level (a chunk holds a whole section's
rules), so an amended section's co-located un-amended provisions go with it;
sub-chunk filtering is the precise future refinement. The "Maple
House" fixture installs under the machine-stable `maple-house` id via a
committed recipe (`sovereign-recipes/maple-house/`), set up + seeded + resolved
by `scripts/setup-governance-corpus.sh`; both lanes gate against committed
baselines via `bench gate governance` / `governance-qa`.

Finally, **`scripts/sovereign-ci-bench.sh`** is the single ≤2h core-regression
gate a developer runs for confidence that chat + inference hasn't regressed. It
*composes* the existing benches (each a visible, re-runnable command) rather
than reinventing them, with a clear gate policy: deterministic baseline-diffed
lanes (retrieval recall, enrichment atom-F1, intent routing) are **hard**
(build-breaking via `bench all`'s exit code); the synthesis answer-equiv judge
lane is **soft** (judge variance shouldn't flake the build); chaos-monkey,
mechanism-fidelity, the multi-turn degradation thread, and the FR-9 governance
lanes (detector + Q&A) run as **tracked**
(advisory) lanes whose *absolute* verdict — a true finding for the current
system, not a regression (chaos is built to break the present agent; mechanism
returns NO-GO for any non-faithful model) — never gates, each paired with a
**hard `*-gate` lane** (`sovereign bench gate <lane>`) that re-scores the same
artifact and fails *only on regression vs a committed baseline*
(`sovereign/bench/<group>/baselines/<id>/`; first-run passes). The gate logic
is one shared, self-describing metric/direction/tolerance primitive
(`bench_cmd/lane_baseline.rs` + `gate.rs`). Overall exit 0 iff every hard lane
stays within baseline and the run fits the budget.

---

## 3. corpus-engine — the shared knowledge layer

Self-contained library; both upstream projects use it through the
same public API; neither knows the other exists.

### Pipeline

```
Acquirer → Extractor → Filter → Chunker → Embedder → Index
                                          (caller-supplied EmbedFn)
```

Each stage is a trait. A **Recipe** TOML configures the whole
pipeline. Built-ins per stage:

| Stage      | Built-ins                                                          |
|------------|--------------------------------------------------------------------|
| Acquirer   | `bulk_download`, `huggingface_dataset`, `local_file`, `http_api`   |
| Extractor  | `mediawiki_xml`, `stackexchange_xml`, `jsonl`, `wikipedia_jsonl`, `wikipedia_structured`, `html`, `html_sections`, `csv`, `parquet`, `plaintext`, `code`, `email` (RFC-5322 + MIME), `described_asset` (content-addressed binary dispatcher), `column_aware` (typed Entity atoms from parquet parsed-form caches), `tabular_atoms` (deterministic typed Entity atoms per row from tabular JSON, e.g. the SF assessor parcel roll) |
| Filter     | `pageview_rank`, `title_list`, `boilerplate` (email signature / quoted-reply / disclaimer stripping), composed via `[[filter]]` (`Any` / `All`) |
| Chunker    | `paragraph`, `sentence`, `fixed`, `semantic`, `passthrough`, `portal_event_bullet`, `threaded_turns` |
| Index      | `CorpusIndex` over LanceDB (IVF-PQ) + Tantivy FTS                  |

The `tabular_atoms` extractor (deterministic, no inference) types each row
of a structured public dataset into a `parcel`-style `Entity` atom whose
numeric/string columns land in `Entity::attributes` (atoms.json schema
2.3). The SF land-value-tax demo folds those atoms into revenue-neutral
land-rate aggregates via the `parcel_analytics` lib (`enrichment/atlas/
analysis/`) + the read-only `parcel_analytics` tool, which emits both
compact cited figures and a full-precision `derivation` trace. The "no
confabulated numbers" guarantee — *the model never originates a number* —
is enforced in three coordinated places: **(1)** the model is shown only
the COMPACT figures (the step summary prefers a tool's `summary` over raw
JSON) and narrates with those, never retyping long precise values (which a
mid-size model corrupts into digit-salad); **(2)** the ComplexTask
synthesizer appends the tool's `derivation` VERBATIM — rendered by the
system, not the model — so the reader sees the exact formula, inputs, and
result; **(3)** a deterministic audit (`runtime::numeric_audit`)
value-matches every $/% figure in the model's prose against the union of
the tool's formatted figures and raw numeric outputs (with a fraction↔%
bridge), flagging only model-originated numbers — a *computed* value is
provenanced by its computation, not a source chunk. `sovereign corpus
export-parcels` writes the exact input set to CSV for independent
re-summing. Routing reaches this agentic path via the router's
tool-relevance gate (a closely-matching registered tool overrides a
toolless intent) and the planner's per-tool input-param hints + example
calls (so a relevant tool is reliably planned as a `tool` step, not a
`reason` step). See `sovereign-recipes/sf-assessor-roll/`.

The `email` + `described_asset` extractors and the `column_aware`
extractor land together as the substrate of the architecture-over-Enron
push (5-phase plan in `~/.claude/plans/this-is-a-whole-serialized-cake.md`).
Each future binary-bearing vertical (Firm Inbox, sales intelligence,
project memory, calendar / transactions / sensor ingest) inherits the
same dispatcher + asset-store pair unchanged. See HISTORY.md's
`enron-entity-resolution` section.

### Storage

LanceDB vectors + Tantivy keyword. One on-disk dir per corpus;
identical schema for a full index or a shard.

```
~/.sovereign/indexes/
├── wikipedia/
│   ├── _corpus_meta.json                # authoritative metadata
│   └── chunks.lance/{...}
├── stackexchange-shard-0-6200000/       # same schema as a full index
└── enron-sample-onemailbox/             # architecture-over-Enron substrate paths
    ├── _corpus_meta.json
    ├── chunks.lance/{...}
    ├── assets/                          # AD-1 content-addressed asset store
    │   ├── ledger.jsonl                 # append-only LedgerEntry per sha256
    │   ├── <hh>/<sha256>                # raw bytes, sharded by leading 2 hex
    │   └── parsed/<sha256>.<ext>        # typed parsed cache (parquet/ical/…)
    └── atlas/
        ├── atoms.json                   # AtomsFile SCHEMA_VERSION 2.2
        ├── asset_atoms.jsonl            # AD-2 Asset envelopes (sidecar union'd
        │                                # into atoms.json on next atlas write)
        ├── asset_edges.jsonl            # EdgeType::Attaches edges
        └── reconciliation_oplog.jsonl   # Phase 4 reversible Merge/Split ops
```

`(corpus_id, chunk_id)` is the citation handle and is **structurally
unique** — `installed_indexes()` dedupes on `corpus_id` (prefers the
dir whose basename equals `corpus_id`, warns and drops collisions).
Out-of-band names (`.legacy-backup`, `.retired`) are excluded.
`IndexMeta` carries `ScopeMeta { filter_descriptions,
filter_signature, expandable }` plus an optional `filter_override`
so a corpus can be expanded in place (relax filters → delta-ingest
the additions → rebuild IVF-PQ).

### Injection contract

`corpus-engine` never embeds or generates text itself.

```rust
pub type EmbedFn     = Arc<dyn Fn(&str) -> Pin<Box<dyn Future<Output = Result<Vec<f32>>> + Send>> + Send + Sync>;
pub type InferenceFn = Arc<dyn Fn(&str) -> Pin<Box<dyn Future<Output = Result<String>> + Send>> + Send + Sync>;
```

| Caller       | `EmbedFn`                                | `InferenceFn` (enrichment)        |
|--------------|------------------------------------------|-----------------------------------|
| Sovereign    | wraps local Embed slot                   | wraps Main responder slot         |
| Commonwealth | `embed_http::http_embed_fn` → `/v1/embeddings` | mesh inference endpoint     |
| Tests        | zero-vector mock                         | canned-JSON mock                  |

Default embedding model: `qwen3-embedding-0.6b` (1024 dims, the
canonical `corpus_engine::DEFAULT_EMBED_DIM`).
`_corpus_meta.json` records the model; opening with a mismatched
model fails with `Error::IncompatibleEmbedding`. The Embed slot is
a cross-peer interoperability contract — nodes sharing a corpus
**must** produce bit-compatible vectors (`EmbedModelInfo` must
match).

### Sharding — three operations

| Operation                              | Effect                                          |
|----------------------------------------|-------------------------------------------------|
| `index_stats(corpus_id)`               | Total chunks, ID range, size on disk            |
| `extract_shard(corpus_id, range, dir)` | New index containing only chunks in range       |
| `merge_shards(dirs, dir)`              | Reconstitute a complete index from N shards     |

Shards are structurally identical to full indexes —
`CorpusIndex::search` doesn't know or care which it operates on.

### Per-node storage budget

Settings → Knowledge sets a ceiling. Enforcement lives at
`sovereign-mesh::capabilities::build_local_capabilities`, which
clamps published `free_storage_gb` to
`min(actual_free, max(0, budget − used))`. Every scheduler
(`assign_knowledge_shards`, the three
`plan_collaborative_ingestion*`) reads that one value, so the
clamp self-enforces for both local installs and peer-driven shard
distribution.

### Enrichment

**Three coexisting systems**, selected per-corpus by `[enrichment] type`
(dispatch at `engine/ingest.rs:1581`). See
[`corpus-engine/ENRICHMENT.md`](../corpus-engine/ENRICHMENT.md) — the
canonical umbrella that reconciles all three — before assuming "enrichment"
means one thing.

- **`field_model` — System 1, `enrichment/field_engine.rs`** — five-phase
  *whole-corpus* pipeline (skeleton → cluster → align → fault lines → open
  questions). `Domain` trait + `DomainRegistry`. Domains include
  `philosophy`, `multi` (Wikipedia), `personal` / `conversational` /
  `institutional` (KnowledgeView). Legacy but live (KnowledgeView digests,
  full-corpus SEP epistemic flow).
- **`atlas` — System 2, `enrichment/pipeline/`** — *per-document* typed
  atom graph (Entity, Claim, Event, Question, Position, Opposition,
  ArgumentReconstruction, Configuration, Asset). `Pipeline` trait +
  registry + `ExemplarBank` + `PhaseCache`. Pipelines: `literary`,
  `literary_atlas`, `philosophy_atlas`, `referential_atlas`,
  `engineering_atlas`, `conversation_atlas`. State at
  `~/.sovereign/indexes/<corpus>/atlas/`. Deep-dive:
  [`ENRICHMENT_V2.md`](../corpus-engine/ENRICHMENT_V2.md).
- **`tiered` — System 3, the RAPTOR + GLiNER gold standard** — three
  progressive tiers (T1 embeddings → T2 entity-graph + PPR → T3 RAPTOR
  cluster tree). The single RAPTOR builder lives in
  `sovereign-tools/src/raptor_atlas.rs` and is injected into `corpus-engine`
  via the `TieredEnrichmentProvider` trait (`enrichment/tiered.rs`) to
  avoid a cyclic dep. GLiNER (real ONNX NER) augments the conversation
  path. Used by attached docs, conversations, Obsidian / watched folders.
  `sovereign enrich raptor <corpus>` (sovereign-cli-llm) retrofits this
  tier-3 tree onto an already-installed corpus additively — writes
  `conv_raptor_nodes` keyed by `source_doc_id`, reuses the existing
  leaf embeddings, and carries `--strip-furniture` + doc-level resume
  (the SEP whole-document-summarization retrofit, 2026-06-06).
  Bucketing is corpus-shape-aware (2026-06-11): vault/watched-folder
  corpora classify per-FILE units via `ConvBucket::classify_note`
  (Tiny only at 0-1 chunks — the conversation-tuned 8-chunk floor
  tiny-bucketed half the live vault into title-only synthetic nodes),
  while document corpora keep the conversation thresholds. For folder
  corpora the retrofit also finishes with `finalize_corpus` (vault
  synthesis + the typed-extension pass into `atlas/atoms.json` —
  see TIERED_RETRIEVAL.md's typed-extension section); document
  corpora never touch the atom-graph atlas. Query-time
  grounding (`apply_raptor_grounding`) reads those summary nodes via a
  derived per-corpus `raptor_summaries.lance` ANN index — built at the
  end of `enrich raptor` (or standalone via `enrich raptor-index
  <corpus>`) by `sovereign-tools/src/raptor_index.rs` over the pure
  `corpus-engine::index::raptor` primitives, with a `max(created_at)`
  freshness gate and the brute-force `conv_raptor_nodes` cosine scan as
  fallback (spec `docs/specs/RAPTOR_ANN_INDEX.md`).
  Deep-dive: [`docs/TIERED_RETRIEVAL.md`](./docs/TIERED_RETRIEVAL.md).

See [`corpus-engine/ENRICHMENT_V2.md`](../corpus-engine/ENRICHMENT_V2.md)
for status table, landing-by-landing scope, and validation targets.

### Recipe registry

Six-plus recipes shipped in `sovereign-recipes`, consumed via
`RecipeRegistry`:

- **Bundled snapshot** — `build.rs` vendors `sovereign-recipes/registry.toml`
  into `OUT_DIR` and `registry.rs` `include_str!`s it from there, so the engine
  works fully offline with no checked-in snapshot copy to drift.
- **Bundled fallback** — `recipe_builtin.rs::bundled_recipe_toml(id)` returns the
  full recipe TOML (also vendored from `sovereign-recipes/` into `OUT_DIR`) for
  snapshot entries when the live URL is unreachable.
- **Live refresh** — `RecipeRegistry::refresh()` pulls the latest
  from GitHub.
- **Resolution order** — local override on disk → remote → bundled.
  SHA-256 verified when the entry's `sha256` is non-empty.
- `cargo xtask update-registry-snapshot` refreshes the snapshot.

### Recipe-authoring platform

The recipe schema is open. Domain experts (financial journalist,
legal aid attorney, grad student) author a TOML; the engine runs
it. Generic primitives:

- **`http_api` acquirer** — URL templating with `{name}`
  placeholders, four pagination strategies, JSONPath document-URL
  follow with bounded concurrency, token-bucket rate limit.
- **`[recipe.parameters]`** — String / Int / Date / List with
  defaults and required flags. `Recipe::resolve_parameters`
  validates user input.
- **`html_sections` extractor** — multi-regex section extraction
  with a `MissReport` sidecar so `recipe test` can show
  "section X missed in filing Y; nearby text: …"
- **Investigation enrichment pipeline**
  (`enrichment/investigation/`) — recipe-declared
  `[[enrichment.entity_types]]` + `[[relationship_types]]` →
  JSON-Schema → llguidance grammar. Three built-in graph-pattern
  detectors (`circular_flow`, `role_overlap`, `threshold`).
- **Lifecycle** — `sovereign recipe {validate,test,publish,list}`.
- **Agent-callable tools** under
  `sovereign-tools/src/recipe_author/` — eight Tool impls behind
  `Permission::RecipeAuthoring`. Wired into MCP via
  `MCP_TOOLS_ALWAYS` in `sovereign-tools/src/mcp_surface.rs`.
- **Recipe-author agent loop** — `recipe_author/project.rs`
  (project model), `situated_context.rs` (per-turn renderer),
  `sovereign recipe-agent {new,show,list,live-trial}` CLI. Skill
  manifest at `sovereign/modes/recipe-author/skill.toml`
  (privacy = `local_only`). The project model carries an
  `ArtifactKind` (recipe | workflow), so the same checkpoints /
  decision log / desktop workspace back **workflow** authoring too
  (checkpoints snapshot `recipe.toml` or `workflow.toml` by kind) —
  the recipes×workflows merge.

### Schema back-compat

Recipes live outside the repo (community registry, user
authoring), so a TOML written six months ago must keep loading.
Convention enforced by the reader + a regression-fixture suite
(`corpus-engine/tests/recipe_back_compat.rs`):

1. New fields carry `#[serde(default)]`.
2. Renamed fields keep the old name as `#[serde(alias = "old-name")]`.
3. Removed enum variants get a deprecation arm in
   `translate_parse_error` that emits "use `<replacement>` instead".
4. `[corpus] schema_version` bumps only when readers must opt in
   to interpret a recipe. Reader refuses recipes declaring
   `schema_version > MAX_SCHEMA_VERSION`.
5. Reserved variants — declare today, validator warns, runtime
   emits placeholder finding; later PR adds executor without
   touching the schema.

`[display]` (`recipe.rs::DisplayMeta`, `category` + `icon`) flows
through `_corpus_meta.json` and surfaces on `IndexInfo` so
retrieval reads category off the index summary without re-resolving
the recipe. Drives Atlas View rail grouping and the synth prompt's
"From your conversations" rename.

### Delta updates + safety

`update/delta.rs` — `VersionManifest` per-document revision IDs;
`ManifestDiff::compute` produces additions/updates/deletions;
three-phase apply (delete → update → add); `_update_progress.json`
for resume.

Hardcoded safety (not configurable per recipe): robots.txt
compliance, 1s/domain rate limit, UA
`CorpusEngine/0.1 (+https://sovereign.dev/corpus-engine)`, crawl
scope enforced against the seed URL domain, download size warnings
at 1.5× estimate.

---

## 4. Sovereign — the local agent

A single-machine local AI assistant. Runs as desktop, CLI, or HTTP
server against the same `Runtime`. No data leaves the machine
unless the user opts in to web search or a Commonwealth mesh.

### Trait architecture

`sovereign-core/src/traits.rs`:

| Trait                          | Surface                                                     |
|--------------------------------|-------------------------------------------------------------|
| `InferenceProvider`            | `complete`, `complete_stream`, `complete_stream_with_id`, `embed`, `embed_query`, `capabilities`, `code_model_id` |
| `Router`                       | `classify(message, ctx, tools) → RouterClassification`      |
| `Planner`                      | `plan(goal, context, tools) → Plan`, `replan(...)`          |
| `Tool`                         | `descriptor`, `execute`, `validate`, `retry_config`, `required_permissions` |
| `LandscapeDigestProvider`      | `splice_landscape_digests(ctx, active_skill)`               |
| `ApprovalChannel`              | Human-in-the-loop tool approval (CLI / Tauri / Server / Auto) |
| `MeshKnowledgeSource`          | Fan-out knowledge search to mesh peers                      |
| `SensitiveCorpusOracle` / `FolderMetadataOracle` | Watched-folder privacy + UI surface |
| `InsightStore` / `InsightSink` | Long-term insight extraction + persistence                  |

`StateStore` is decomposed per ISP into focused sub-traits aggregated
by a single blanket impl: `ConversationStore`, `TaskStore`,
`MemoryStore`, `RoutingStore`, `DocumentStore`, `CorpusStateStore`,
`BudgetStore`, `PermissionStore`, `HealthStore`,
`DocumentSessionStore`, `DocumentAssetStore`, `InsightStore`.
Callers narrow bounds to what they need.

### Runtime data flow

```
User message
  → Router.classify           (Quick slot, two-pass coarse → refine)
       → RouterClassification { primary, alternatives, rationale, … }
  → decide_policy(classification, ConfidenceThresholds)   (pure fn)
       → RoutingPolicy { tier, move_kind: Commit | Propose | Ask, … }
  → SessionStore.begin → QuerySession (CancellationToken-bearing)
  → Dispatch by Intent:
       ├─ SimpleQuery / DeepQuery / KnowledgeQuery → search → synthesize
       └─ ComplexTask → Planner.plan (Main slot)
                      → Executor (topological batches)
                          ├─ ReasonWithTools loop
                          ├─ Best-of-N sampling (LlmJudge / Random / Best)
                          ├─ Evaluation passes
                          └─ Tool steps with permission + approval
  → Provenance recorded into Message.metadata
  → Memory extraction at conversation end
```

`Plan` is a flat JSON DAG (`steps`, `edges`). `StepKind`: `Reason`,
`Tool`, `UserInput`, `Branch`, `ReasonWithTools`. Planner emits
`[sample:N:method]` / `[eval:name]` annotations; the executor
parses them into config.

The router emits **facts**; the runtime applies **policy**.
Splitting them keeps classification testable without a model and
lets thresholds calibrate without touching the trait.

**Router classifier stack — one wiring path, all surfaces.** Before the
coarse→refine LLM cascade, `Router.classify` consults a stack of
embedding-centroid pre-checks: the **embed router** (intent exemplars → a
direct intent when confident, skipping the LLM passes), the **scope**
classifier (personal vs external), the **effort** classifier (a high-effort
referential `Answer` → `DeepQuery` → primary slot — the exhaustive-ask
escalation), and the **current-info** classifier (drives `force_action` for
time-sensitive queries). All four are assembled by the single helper
`sovereign-core/src/router_bootstrap.rs::build_llm_router`, which **every**
surface calls — CLI/bench, desktop, and the served daemon. Exemplars are baked
into the binary (`include_str!` of `sovereign/router/*.toml`), so the stack
works regardless of CWD or `.app`-bundle layout; a `SOVEREIGN_*` env var or
repo-relative file overrides the baked default. This is **parity by
construction**: before 2026-06-09 the stack was wired only in the CLI/bench
bootstrap, so the desktop app (bare router) and the daemon (current-info only)
silently under-routed to the fast slot while the benches — which *did* wire it
— kept improving ("desktop kind of sucks even as benches get better"). The fix
collapsed the three call sites onto one path; `tests/router_bootstrap_parity.rs`
asserts `all_wired()` so it can't silently re-diverge. Effort-tier escalation +
robust coarse-verdict recovery default **ON** (`SOVEREIGN_KQ_EFFORT_TIER=0` /
`SOVEREIGN_ROUTER_ROBUST_COARSE=0` disable).

**Pre-built router-embed cache (`router_embed_cache.rs`).** The four boot
classifiers embed ~310 static exemplars at every process start — ~5.7s on Apple
Silicon, *minutes* on a CPU-only embed slot (Intel Macs, which `embed_slot.rs`
gates off Metal). So the embeddings are pre-computed for the prescribed embed
model and committed at `sovereign/router/router-embed-cache.json`, baked into
the binary (`BAKED_ROUTER_EMBED_CACHE`) and loaded as the fallback when
`~/.sovereign/router-embed-cache.json` is absent — first launch HITS instead of
re-embedding. A sentinel cosine probe validates it against the live model, so a
genuinely swapped embed model is rejected → one-time re-embed, surfaced as the
`RebuildingRouterEmbeddings` bootstrap phase. Freshness is a pure no-inference
gate (`check_cache_fresh`: exemplar-key coverage + a `family|hf_url` model
fingerprint) enforced by `tests/router_cache_fresh.rs` (CI + a
`desktop-release.yml` pre-flight) and regenerated by `sovereign router-cache
rebuild`, which `scripts/bump-desktop-version.sh` runs when stale. The same
`EmbedSlot::load` auto-detects the Qwen3-Embedding family from the gguf
architecture, so the prescribed model under a non-default filename still gets
last-token pooling + the query instruction-prefix (and thus matches the cache).

**Synthesis role layer (`role.rs`).** The knowledge-turn path is organized as
three data-defined roles — the synthesis-side counterpart to the agent-loop
roles in `commonwealth-agent-tools/src/role/`, lifting the same
`RoleProfile`/`RoleModelMap` shape (ARCH §6: profiles are *data*). **Router**
classifies + resolves the route (mechanism: `EmbedRouter` +
`resolve_synthesis_route`); **Synthesizer** assembles the grounded answer
(mechanism: `build_synthesis_system_prompt` — the one prompt-body builder all
synthesis sites now call); **Critic** is a *separate verification pass*
(mechanism today: the bench grounding/abstain/caveat classifiers — defined in
`role.rs` so bench + any future prod critic share one definition, **not yet
wired into prod synthesis**). Each `RoleProfile` ships with its
`verify_predicate` (the keystone: the predicate defines correctness, the bench
measures it). Two SSOT decisions back this: `build_synthesis_system_prompt`
(one prompt body, byte-equivalence-tested) and `resolve_synthesis_route` (the
single traced FastFocused-vs-PrimarySynthesis decision with a typed
`RouteReason`, truth-table-tested against the legacy ladder) — together they
end the "live path mis-identified three times" illegibility. `role.rs` is
**load-bearing**, not just declarative: the resolver returns `role::Tier` (via
`SynthesisRoute::tier()`, surfaced in the KnowledgeQuery trace as
`role=synthesizer tier=…`), and the chaos bench sources the Critic's gate model
from `default_profile_for(Role::Critic)` (`--critic-model`, default primary).

**The production grounding gate (`runtime/grounding/`, shipped 2026-06-11) —
and the keystone verdict it reversed.** The 2026-06-09 chaos result ruled
Critic-as-gate out of prod empirically (competence 0.46 → 0.08: it gated
present-answerable questions — `present-wife` at violation_prob 0.806 — when
retrieval missed the supporting passage). What changed the verdict was not the
judge but the **evidence universe**: the v12–v15 gate verifies claims against
the *sealed corpus* (per-claim hybrid search via `ClaimSearcher`), not just the
prompt snapshot, and feeds failed claims' corrective passages into the rewrite
(replace, don't delete). Under that stack the gate is net-positive and PASSES
the full bank (secret-agent 0.67/0.82/0.18 production-config, 2026-06-11;
holdout honesty 0.91/0.09). Mechanism: **hold → verify → corrective retry
(short answers) / per-claim audit → rewrite → annotate (long-form) → grounded
abstention**, fail-open on judge failure. Judge prompts are byte-pinned to the
bench critic so the bench-calibrated τ=0.9 transfers. Module layout:
`grounding/config.rs` (`GateSurface` closed enum + per-surface
`GroundingProfile` budgets + `grounding_gate_flags()` registry), `judge.rs`
(claim extraction, forced-choice support, joint long-form judge), `search.rs`
(`SealedEvidenceSearch` trait — claim-conditioned widening that can never
widen corpus scope), `mod.rs` (the ladder: `gate_answer` over an
`EvidenceContext`). Gated surfaces today (all env-gated;
`SOVEREIGN_GROUNDING_GATE` global default, `SOVEREIGN_GROUNDING_GATE_<SURFACE>`
override): streaming/non-streaming KnowledgeQuery + streaming DeepQuery
(dual-bank validated), attached-doc (dual-bank validated: Conrad dev bank +
Meridian holdout under `bench/attached_doc/`; `AttachedAssetSearcher` seals
claim search to the asset), complex-task (narration gated per-claim against
the step transcript, verbatim derivation appendix untouched; calibration bank
pending), simple-query (non-witness, retrieval-matched turns only). The
refinement overwrite path re-gates verify-only (`RefinementGuard`): a
gate-released answer is never overwritten by text that fails the same gate.
Corpus-deictic questions ("the story", "this document") close the GK-caveat
exemption like entity anchoring does. Metalingual answers are structurally
grounded instead of gated: decode-committed term-absent caveat / source
attribution + the quote guardrail (calibration bank pending). (Note: effort-tier escalation default-ON *improves* chaos
competence 0.33 → 0.46 — a net win, not a regression.)

**Retrieval pipeline (`runtime/retrieval_pipeline.rs`).** The
retrieval-injection orchestration — which grounding/boost/expansion steps run,
in what order, under which `SOVEREIGN_*` gates — is **data**: a
`RetrievalPipeline` is an ordered list of named `RetrievalStep`s run by one
tracing runner (one `tracing::info!(target: "retrieval.pipeline")` line per
step with `chunks_before/after/delta`). The governing principle: **the
intent decides HOW to answer (model tier, expansion, synthesis shape) — never
WHERE knowledge lives.** Both pipelines are composed as **the SHARED 3-step
evidence-gathering head (local corpora ∥ mesh fan-out → personal-scope filter
→ StateStore corpus docs) + the SHARED 12-step core + a per-intent tail**:
`kq_pipeline()` (KnowledgeQuery / ComparisonQuery, 16 steps; tail = audited
truncate, then route-aware expansion post-pipeline) and `deep_pipeline()`
(DeepQuery / SimpleQuery, 17 steps; tail = plain truncate + strategy-driven
top-sources expansion; attached-doc turns drop the head and the two grounding
steps). Golden tests pin the step lists and the head+core identity, so
reordering is an explicit, reviewed act. The Phase 2 convergence (2026-06-09,
CI-bench-A/B'd) moved the deep path's atlas/RAPTOR grounding to the KQ
post-floor position (the old pre-floor position let the noise floor silently
drop zero-overlap virtual grounding chunks) and extended `dedupe_merged` to
the KQ path; per-intent differences (comparison-aware entity boost/reserve)
ride `PipelineState`, not divergent code. The injection helpers
themselves (`apply_atlas_grounding`, `apply_raptor_grounding`,
`meta_atlas_boost`, `fan_out_decomposed_queries`, `expand_from_top_sources`, …)
are unchanged `impl Runtime` methods in `retrieval.rs`; the step bodies are
verbatim transplants of the orchestration that previously lived inline (and
duplicated, with silent drift) in `prepare_knowledge_query_plan` and
`prepare_knowledge_context` — both handlers now build a `PipelineState` and
call `pipeline.run(...)`, then keep their post-pipeline concerns
(evidence-shape routing + route-aware expansion + prompt/request assembly on
the KQ side; provenance + prompt/history assembly + seal audit on the deep
side). `retrieval_pipeline_flags()` is the SSOT registry of every retrieval
env knob (name + default + purpose). The 2026-06-10 divergence-archaeology
pass resolved the remaining per-intent divergences (see the module doc's
resolution log): deep's expansion decision now goes through the same
`decide_expansion_strategy` SSOT the KQ planner uses (chunk-set-identical by
the helper's internal guard; emits the same `expansion_decision` audit), the
personal-scope filter is one shared whole-pool step on both paths (mesh
strays now drop on personal-scope turns), and the store-search leg reuses
the pipeline's query embedding (closing a missed `embed_query` retrofit from
2026-05-18). The last accretion artifact — KnowledgeQuery turns silently
skipping the mesh and the doc store (Deep/Simple have searched both since
2026-04-21) — was resolved the same day by unifying both pipelines onto
`shared_head_steps()`: which knowledge sources exist is a property of the
install, not of the intent label. Environments without a mesh or
store-ingested corpora see identical behavior; the known mesh round-trip of
local corpora is collapsed by the shared `dedupe_merged` step. Open
follow-up: KQ provenance doesn't yet surface mesh peer attribution
(`search_method` labels live on the deep handler).

Per-intent handlers live in
`sovereign-core/src/runtime/handlers/{simple,ask_move,conation,commissive,metalingual,expressive,document_op,complex_task,attached_doc,knowledge_query}.rs`
as `impl Runtime` across files (no vtable hop on dispatch).

### Inference

`sovereign-inference/src/embedded.rs` wraps `llama-cpp` with a
lazy-loaded slot system (Quick / Main / Code / Embed). Hybrid +
remote providers wrap OpenAI-compatible servers (vLLM, Ollama,
llama.cpp, TGI, Commonwealth). Full detail — slots, polished slot
management, sibling pool, decode paths, MTP, OICP scoring, harness
adapters, cutoff legibility, conversation-history compaction — in
[`docs/inference.md`](./docs/inference.md).

### Tools

| Tool                    | Purpose                                                      |
|-------------------------|--------------------------------------------------------------|
| `SearchTool`            | Local vector + FTS5, coverage assessment, optional web fallback |
| `WebSearchTool`         | NL → keywords (Quick), search backend, fetch top 3, synth w/ citations |
| `WebFetchTool`          | Single-URL fetch + HTML→text                                 |
| `KnowledgeTool`         | Direct corpus query                                          |
| `ClaimSearchTool` / `EpistemicLandscapeTool` | Enriched-corpus retrieval         |
| `DocumentTool`          | Map-reduce summarize/analyze (4 chunks/batch, 8K reduce)     |
| `ShellTool` / `FileTool` / `EmailTool` / `CalendarTool` / `ComputeTool` | Standard tools (sandbox + approval) |
| `McpClient` + `McpToolAdapter` | stdio JSON-RPC + HTTP+SSE; wrap remote MCP servers as native tools |

**External MCP servers (client direction).** HTTP MCP servers are configured in
the `[[mcp_servers]]` array of `~/.sovereign/config.toml`
(`SetupConfig.mcp_servers`) — added via `sovereign mcp add` or **Settings →
MCP** — and loaded into the agent's tool registry at startup by the one shared
loader `sovereign_tools::mcp::load_from_setup_config`, which **every** chat
surface calls (`sovereign chat`, the desktop bootstrap, `sovereign serve`).
Each MCP tool's descriptor is enriched (`McpToolAdapter` synthesizes an example
call from the input schema + passes through any `outputSchema`) so the planner
reliably emits a tool step instead of a reason step; tools declare
`Permission::Network`, so the executor's approval gate fires on first use
(add-time trust on the auto-approving CLI). The config DTO lives in
`sovereign-core::mcp_config` (so `SetupConfig` can carry it without a crate
cycle) and is re-exported from `sovereign_tools::mcp`. `sovereign mcp
demo-server` runs a sealed-fact reference server
(`sovereign-cli-llm/src/mcp_demo_server.rs`) for an end-to-end demo: a tool
whose output exists nowhere else, so a correct answer in chat proves the model
actually called it.

**Attach-a-file-for-tools (desktop).** Vision / audio MCP tools take a file
*path* (the model stays text-only — the tool does the modality work). The
desktop's media-attach (image/audio) binds a file's absolute path to the turn
and prepends a `▸ attached file: … path: …` block to the message before the
runtime sees it — the same "augmented message" rail `context_chunks` uses
(`commands/chat.rs::build_tool_files_preamble`), so the model passes the path to
e.g. `describe_image(path)` / `transcribe_audio(path)` on a *local* MCP server
with no Runtime change. Distinct from a *document* attachment (which is ingested
for RAG and discards the path). Spec: `docs/specs/ATTACH_FILE_FOR_TOOLS.md`
(P1 shipped; P2 threads a typed `ToolContext.attached_files`).

**Code-intelligence MCP server**. Long-running variant via
`sovereign daemon`; ad-hoc via `sovereign project serve`. Tools
under `sovereign-tools/src/code/` cover code index (`symbols`
= `symbol_lookup`, `code_search`, `recent_changes`, `working_set`,
`brief`), SCIP call graph (`callers`, `callees`, `blast_radius`),
lint/test watchers (`lint_status`, `get_lint_output`,
`test_status`, `run_tests`, `get_run_output`, `build`), notes
(`write_note`, `read_notes`, `delete_note`, `suggest_note`,
`promote_note`, `read_note_by_id`, `read_note_digest`), ATOS
feature lifecycle (`provision_feature`, `archive_feature`,
`record_atos_event`, `write_redteam_finding`, `atos_plan_emit`,
`atos_utils`, `atos_verify`, `spec`), drift (`drift`,
`drift_posture`, `drift_findings`), project + design context
(`project_context`, `design_signals_extract`, `check_doc_paths`,
`index_health`), session reflection (`session_reflection`), and
work-atlas coordination (`declare_scope`, `release_scope`,
`work_in_flight` — see [`docs/WORK_ATLAS.md`](./docs/WORK_ATLAS.md)).

A `CodeWatcher` re-indexes modified files and marks them stale in
the call graph. Staleness levels carry calibrated confidence:
`None` / `SomeCallSitesMayBeStale` / `GraphIsAging` / `GraphIsStale`
/ `LanguageNotIndexed`. `blast_radius` does BFS over the call graph
and appends a `macro_hints` text scan for references SCIP doesn't
capture.

### State, memory, skills

- **State**: `sovereign-store` provides `SqliteStateStore` (default),
  `PostgresStateStore` (deadpool + tokio-postgres),
  `MemoryStateStore` (tests). One trait, three impls. Schema in
  `migrations.rs`: `conversations` + `messages` (FTS5), `tasks`,
  `memories`, `documents`, `corpus_states`, `routing_log`,
  `search_budget`, `permissions`. Every record carries a Lamport
  `version`; soft-deletable rows have `deleted_at`. Two stores can
  union-merge without schema migration.
- **Working memory** — compressed every message via
  `memory::compress_working_memory` (Quick slot, ≤200 tokens) into
  `{ current_goal, facts, active_documents }`.
- **Long-term memory** — extracted at conversation end. Each
  `Memory` has `confidence`, `created_at`, `last_used`. FTS5
  retrieval. Exponential monthly decay; pruned below
  `prune_threshold`.
- **Routing-correction memory** —
  `RoutingCorrection { message_hash, classified_as, was_correct }`
  fed back into the router prompt as "avoid these mistakes."
- **Skills** — TOML files under `sovereign/modes/` (current:
  `recipe-author`, `inner-work`). `SkillRegistry` merges routing
  hints, planner templates, prompt overrides, memory rules, and
  OICP requirements into the runtime. Skills carry
  `signature` / `signed_by` and a derived `TrustLevel
  { CommunityReviewed, AuthorSigned, Unsigned }`.

### Frontends

| Frontend            | Purpose                                                                              |
|---------------------|--------------------------------------------------------------------------------------|
| `sovereign-cli` (+ siblings) | User-facing dispatcher. `sovereign <verb>` execs into one of three siblings — `sovereign-cli-daemon`, `sovereign-cli-dev`, `sovereign-cli-llm` — based on the verb. Same UX as one binary; faster builds. Discovery: each sibling at `current_exe()`'s parent dir; override via `SOVEREIGN_CLI_{DAEMON,DEV,LLM}_BIN`. Unix execs into the sibling (same PID); other platforms spawn-and-wait. |
| `sovereign-server`  | Axum REST + WebSocket on configurable port; multi-tenant via `tenant.rs`; server-side `ApprovalChannel` w/ `/v1/tasks/{id}/approve`. **Mobile-facing surface** (`docs/specs/MOBILE.md`): WS `/v1/conversations/{id}/stream` streams `ServerEvent::Token`→`Complete` token-by-token down the requesting socket (not the shared broadcast — avoids cross-tenant leak); `projection.rs` surfaces typed `provenance` + `citations` on REST message responses; `GET /v1/corpora` lists `CORPUS_REF`s (Knowledge-only, with `scope`/`mesh_shared` privacy posture derived from `IndexInfo.mesh_sharing`); a `scheduler.rs` `FairScheduler` bounds concurrent turns — a weighted-fair queue + per-origin cap with live `ServerEvent::QueuePosition` over WS and `503 + Retry-After` shed (`busy.rs`) on REST, sharing its `commonwealth_core::fair_sched::SchedCore` policy core with the mesh peer-admission gate (so both are fair by identical rules); reciprocity weights from the contribution ledger rank a contributor's turns up. |
| `sovereign-desktop` | Tauri 2 + Svelte 5; setup wizard, chat w/ streaming + provenance, knowledge management (`KnowledgeStatus`, `CorpusProgressBanner`), skill manager, mesh UI, `sovereign://` deep-link handler, system tray. Reuses the shared `@sovereign/chat-ui` package (`packages/chat-ui`). |
| `sovereign-mobile` (`/sovereign-mobile`) | Thin Tauri 2 client (iOS + Android) — **no local inference/Runtime/corpus**. Reaches a host's `sovereign-server` over the tailnet, authenticates as a tenant (token in keychain), renders streamed chat. Rust core owns transport (HTTP + WS), SQLite cache of the spec's cached projections, and a fail-closed connectivity monitor; re-emits the SAME `message-chunk`/`message-complete` events the shared chat FSM consumes. Conversations are cached for display and referenced as a conversation `CORPUS_REF` once host-indexed (`indexed_in_corpus`); long-context is host-side (phone sends only the new turn + conversation id, never re-uploads history or embeds); local-only sources are privacy-badged (`scope`/`mesh_shared`). Detached from the Cargo workspace (own `[workspace]`); scaffold pending a Tauri-mobile-toolchain build. See `docs/specs/MOBILE.md`. |

Verbs by sibling binary:

- `sovereign-cli` (dispatcher + light delegators, no LLM dep) —
  `notes`, `status`, `drift`, `audit`, `claim`, `charter`, `amend`,
  `design`, `plan`, `init`, `milestone`, `refresh`, `reflect`,
  `rough-edges`, `archaeology-eval`, `git-archaeology`,
  `agent-bench`, `nudge`, `serve`, `stop`.
- `sovereign-cli-daemon` — `daemon` (owns :9741), `setup`,
  `install-service`, `doctor`.
- `sovereign-cli-dev` — `atos`, `project`, `code`, `tools`.
- `sovereign-cli-llm` — `chat`, `bench`, `eval`, `voice`,
  `reading-diag`, `atlas`, `meta-atlas`, `enrich`, `recipe`,
  `recipe-agent`, `maintainer`, `pipeline`, `mcp`, `alignment`,
  `mesh`, `corpus`, `newsworthy`, `knowledge-gym`, `search-gym`,
  `awareness`.

There is no interactive REPL. Bare `sovereign` prints usage and
exits; use `sovereign chat` for the interactive shell, which
streams through the daemon's `/v1/chat/completions`. `project init`
prompts for AI-assistant harness (Claude Code / opencode / both /
skip) and writes `.opencode/config.json` + `AGENTS.md` and installs
the ATOS opencode plugin.

The daemon (`sovereign-cli-daemon::daemon_cmd::run`) rotates its
own logs at startup via `util::log_rotation` — copy-truncate, 10
MiB cap, 5 backups, 30-min sweep loop; preserves the inode for
launchd-held FDs.

### Deep-link handler

`sovereign-mesh/deep_link.rs` parses `sovereign://create?name=<name>`
and `sovereign://join?key=<key>` (with relay hints for NAT
traversal). The desktop app registers as the system handler.

### Subsystems with their own docs

| Subsystem | Doc |
|---|---|
| Slots, OICP, harness, cutoffs | [`docs/inference.md`](./docs/inference.md) |
| Glassbox reading surface + Atlas Inspector | [`docs/knowledge-view.md`](./docs/knowledge-view.md) and `sovereign-tools/src/atlas_view/` |
| KnowledgeView landscape splice | [`docs/knowledge-view.md`](./docs/knowledge-view.md) |
| ATOS — agent task orchestration | [`docs/ATOS.md`](./docs/ATOS.md), [`docs/ATOS_RUNNER.md`](./docs/ATOS_RUNNER.md) |
| Architectural-correctness tooling | [`docs/DRIFT_DETECTION.md`](./docs/DRIFT_DETECTION.md), [`docs/CORRECTNESS_TOOLING.md`](./docs/CORRECTNESS_TOOLING.md), [`docs/GIT_ARCHAEOLOGY.md`](./docs/GIT_ARCHAEOLOGY.md), [`docs/ARCHAEOLOGY_EVAL.md`](./docs/ARCHAEOLOGY_EVAL.md), [`docs/PLAN_ALIGNMENT.md`](./docs/PLAN_ALIGNMENT.md) |
| Knowledge bases + tiered retrieval | [`docs/KNOWLEDGE_BASES.md`](./docs/KNOWLEDGE_BASES.md), [`docs/TIERED_RETRIEVAL.md`](./docs/TIERED_RETRIEVAL.md) |
| Work-atlas peer coordination | [`docs/WORK_ATLAS.md`](./docs/WORK_ATLAS.md) |
| TDD machine | [`docs/TDD_MACHINE.md`](./docs/TDD_MACHINE.md), [`docs/TDD_MACHINE_DESIGN.md`](./docs/TDD_MACHINE_DESIGN.md) |
| Solver design | [`docs/SOLVER_DESIGN.md`](./docs/SOLVER_DESIGN.md) |
| Local corpora / Obsidian / watched folders | `sovereign-tools/src/local_corpus/` — invariants pinned via tests in that crate |
| Wikipedia freshness layer | `corpus-engine/src/update/newsworthy*.rs` + `sovereign-recipes/wikipedia-newsworthy/` |
| Per-document index recency (Atlas fresh-first) | `corpus-engine/src/freshness.rs` — source-agnostic `source_doc_id → unix` sidecar (`_doc_freshness.json`) stamped at the single reindex convergence point (`engine::reindex::reindex_by_source_doc_id`); `ChunkRef.source_doc_id` carries the join key onto atoms, and `sovereign-tools::atlas_view::atom_browse` sorts atoms fresh-first + sets `AtomSummary.updated_at`. ANY re-indexing source (newsworthy, watched-folder edit, delta) makes its content "fresh" with no per-source code — freshness is emergent from indexing. |
| Pinned worker pods as inference peers | [`docs/PINNED_WORKER_AS_INFERENCE_PEER.md`](./docs/PINNED_WORKER_AS_INFERENCE_PEER.md), [`docs/EPHEMERAL_WORKER_PODS.md`](./docs/EPHEMERAL_WORKER_PODS.md) |
| Cloud peer deploy | [`docs/CLOUD_PEER_DEPLOY.md`](./docs/CLOUD_PEER_DEPLOY.md) |
| Mesh load awareness | [`docs/MESH_LOAD_AWARENESS.md`](./docs/MESH_LOAD_AWARENESS.md) |
| Voice contract harness | `sovereign/bench/voice/README.md` |
| Production search integration | [`docs/PRODUCTION_SEARCH_INTEGRATION.md`](./docs/PRODUCTION_SEARCH_INTEGRATION.md) |
| Features overview | [`docs/FEATURES.md`](./docs/FEATURES.md) |
| FAQ / troubleshooting / dev | [`docs/FAQ.md`](./docs/FAQ.md), [`docs/TROUBLESHOOTING.md`](./docs/TROUBLESHOOTING.md), [`docs/DEVELOPMENT.md`](./docs/DEVELOPMENT.md) |
| **On-call runbook** (incident decision tree, supervision, memory budget, glassbox map, bench noise bands, sibling-rebuild map) | [`docs/RUNBOOK.md`](./docs/RUNBOOK.md) |
| Retrieval pipeline steps + env-knob registry (GENERATED, freshness-gated) | [`docs/retrieval-pipeline.md`](./docs/retrieval-pipeline.md) |

### Notable in-tree invariants

These are structural commitments enforced by tests, not policy
toggles. Code reviewers should call out attempts to bend them.

- **KnowledgeView privacy** is layered three deep — recipe
  (`scope=local`, `mesh_sharing=false`), acquirer SQL (excludes
  `local_only` skills at ingest), splice (suppresses non-personal
  views when the active skill is `local_only`). See
  [`docs/knowledge-view.md`](./docs/knowledge-view.md).
- **Watched folders are read-only on source**; sensitive folders
  are excluded from ambient retrieval (Filter 3 in
  `Runtime::search_corpus_indexes`); multi-root corpora dedup by
  content hash. Invariants pinned in `local_corpus::watched::`
  tests.
- **Single-flight chat dispatch** — `ChatView` dispatches
  `SEND_INITIATED` before any bridge await (fixes the 60s
  blank-window bug); `ensureConversation` uses `CONVERSATION_BOUND`,
  not `HYDRATE`, to preserve the in-flight user bubble.

### Agent-coding battery + canonical tools + TDD

`sovereign/crates/sovereign-agent-bench/` — eight-problem graded
battery measuring end-to-end coding agents (pi / opencode / codex /
aider, model-agnostic). Problem mix: three algorithmic, three
system-design, two code tests; languages span Rust × 3, Go × 2,
TypeScript × 2, Python × 1. Scored `0..=3` on correctness /
approach / efficiency, `72` max. CLI: `sovereign agent-bench
<run|list|show>`. Dispatch via `AgentRunnerRegistry`.

`sovereign/crates/commonwealth-agent-tools/` — canonical tool
surface. Five primitives (`inspect_workdir` polymorphic over
file/dir/find/grep, `write_file`, `cargo_build`, `cargo_smoke`,
`agent_done`); every runner translates to/from this set. Plus a
role layer (Planner / Implementer / Evaluator) operating on the
same model weights via different prompts + tool subsets + forced
first tools.

`sovereign/crates/commonwealth-tdd/` — unified solver loop for any
TDD-shaped workflow. One function `run_trial(Trial) → TrialResult`
with `Polarity::{MaximizePassing, GenerateOneFailing}`. Transports
HTTP (`POST /v1/solve`) + MCP (`tdd_solve`) live in
`sovereign-server`. See [`docs/TDD_MACHINE.md`](./docs/TDD_MACHINE.md).

---

## 5. Commonwealth — the coordination daemon

A symmetric daemon. Every node runs the same binary; no master.
Members find each other via mDNS on the LAN or transitively over a
VPN (Tailscale/WireGuard) and converge on shared state via gossip.

In one sentence: translates "complete this chat with model X" into
a plan that spawns `llama-server` on one node and `rpc-server` on
others, holds the OpenAI-compatible HTTP endpoint open, and keeps
the plan healthy as nodes come and go.

### Discovery and membership

- **Join keys** — `cwth-XXXX-XXXX-XXXX`.
  `membership::generate_join_key` stores BLAKE3 hash, discards
  plaintext. `verify_join_key` is constant-time. First node calls
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
- **Hardware detection** — `discovery/hardware.rs` tries
  `nvidia-smi`, then `rocm-smi`, then Metal.
- **TLS / mesh encryption** — A plaintext mesh (the default) serves
  the internal API (:9742) in the clear; the unused per-session-cert /
  `TrustStore` scaffolding (`discovery/tls.rs`) was removed (2026-06-15)
  rather than left as a security façade. TLS *is* used on the separate
  worker-pod path (`sovereign-mesh/worker_daemon.rs`,
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
    mesh. Never claim blanket end-to-end encryption.
- **Mesh peering** — `peering.rs`; two `PeerTrustLevel`s:
  `ModelAndKnowledgeSharing`, `Full`.

### The PeerTransport seam (commonwealth-transport)

How this node reaches a peer is decided in exactly one place:
`commonwealth-transport`'s `PeerTransport` trait resolves
*(PeerContact, TrafficClass) → ordered base URLs*; call sites keep
their own reqwest clients/timeouts and append route paths. The live
instance hangs off commonwealth-api's `AppState`
(`peer_transport()` / `install_peer_transport`).

- **`IpTransport`** (production): today's tailnet/LAN overlay. Owns
  the Tailscale CGNAT/ULA address ranking (`peer_addr::rank` — no
  other production caller) and the last-working-address promotion
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
  daemon's gossiped `node_key` and `spawn_routed`s it across both
  ALPNs — `cwth/http/0` → internal router, `cwth/client/0` → client
  router — so a peer/phone reaches this daemon by key with no VPN
  (`sovereign-mesh/src/iroh_access.rs`, `MeshIrohAccess`; additive,
  fail-soft, held in `DaemonState::Running`). W2: `MemberRecord` carries
  `relay_url` + `iroh_direct_addrs` (serde-defaulted, MUTABLE
  reachability — normal LWW, unlike `node_pubkey`'s anti-downgrade);
  the daemon self-stamps its live dial info each gossip round via a
  pull-provider on `AppState`; `IrohTransport` dials peers purely from
  the gossiped contact (**membership = dialability**). W3: `[iroh.transport]
  <class> = "iroh"` installs a `RoutedTransport` for the flipped
  classes (IP fallback retained); **no class is flipped by default**,
  so the daemon still routes its own traffic over `IpTransport` until
  an operator flips one (recommended order: gossip first, then soak).
  Join-over-iroh is W2b.
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
  tensor traffic (stays on the IP overlay until a tunnel proxy is
  worth building).
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
| Joiner picks peer-vs-local for a turn | `sovereign-mesh/peer_inference.rs::select_peer` | OICP claim score × operational adjustments (observations, load, locality, cold-start, throughput, availability) |
| Hub picks a local model for a peer request | `commonwealth-api/routes_inference.rs::route_with_oicp` | OICP claim score over synthesized claims |
| Serving peer picks Fast-vs-Slow slot | `sovereign-mesh/oicp_select.rs::pick_slot_for_oicp` | latency_class→Speed map + hint veto |
| Synthesis tier (Fast vs Primary) | `sovereign-core/runtime/evidence.rs::resolve_synthesis_route` | intent + atom-enum + evidence-shape heuristic |
| Distributed placement (model > one node) | `sovereign-inference/embedded/rpc_distribution.rs` | LocalOnly default; StreamSplit ≤500MB; warmed owned-overrides as last resort |
| Collaborative ingest partitioning | `commonwealth-inference/scheduler/knowledge_assignment.rs` | `plan_collaborative_ingestion*`: embed-model-compatible peers, storage-proportional contiguous blocks, zero-storage peers skipped |

The composed OICP scoring product lives ONCE in `oicp-types`
(`score_with_adjustments` + `ScoreBreakdown`, Phase B of the
rationalization) and is consumed by sovereign-mesh and
sovereign-inference; leader election lives in
`commonwealth_core::partition::elect_leader`.

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

The strong-peer-topology roadmap (latency-class hierarchy: cascade
routing, draft-on-spoke/verify-on-hub speculation, hub queue
discipline — each reality-checked against this codebase) is
[`docs/specs/MESH_INFERENCE.md`](./docs/specs/MESH_INFERENCE.md).

`commonwealth-inference/orchestrator/` (multi-process supervision for
the standalone-daemon topology — flagged for its own liveness
investigation in OICP_RATIONALIZATION.md):

- `ManagedProcess` tracks lifecycle states (`Starting | Running |
  Unhealthy | Failed | Stopped`); graceful SIGTERM with timeout,
  then SIGKILL.
- `HealthTracker` polls every 5s; 20-sample latency window;
  `Unresponsive` after 3 consecutive failures.
- `GracefulDeparture` — 30s countdown state machine
  (`Announced → Rebalancing → Draining → Complete`).
- `FaultDetector` collapses health changes into `FaultEvent`s.

### HTTP API

Two listeners, two trust domains.

**Client API — :9741, no mTLS, binds 0.0.0.0** (federated inference
needs peer reachability)

| Path                          | Notes                                                  |
|-------------------------------|--------------------------------------------------------|
| `POST /v1/chat/completions`   | OpenAI-compatible. Routing differs by daemon shape (embedded vs standalone) — see `commonwealth/docs/routing-field-guide.md`. `LocalOnly` privacy → 400. |
| `POST /v1/responses`          | OpenAI Responses-API adapter (codex 0.130+). Wire-format translator over chat-completions. See [`docs/inference.md`](./docs/inference.md). |
| `GET  /v1/models`             | Loaded models w/ capabilities + performance estimates  |
| `POST /v1/knowledge/search`   | Determines target corpora, fans out, merges, reranks   |
| `GET  /status`                | Node / mesh / inference / knowledge summary            |
| `GET  /oicp/v1/capabilities`  | Provider manifest + federation info                    |
| `/api/{version,tags,ps,show,chat,generate,embed,embeddings}` | **Ollama-native compatibility shim** (`routes_ollama.rs`). Pure translation over the OpenAI handlers above — lets Ollama-native clients (Open WebUI's Ollama mode, IDE plugins) connect. `chat`/`generate` are non-streaming-backed in v1: the inner handler runs `stream:false` and the complete answer is framed as Ollama NDJSON (one content frame + terminal). No CORS layer + same no-auth posture as `/v1/*` (documented in-module); incremental streaming + per-request auth are tracked follow-ups. |
| `/v1/mesh/*` `/v1/admin/*` `/mcp/*` | **Loopback-only** (router middleware + per-handler `enforce_localhost`) |

**Internal API — :9742, mTLS**

| Path                                | Purpose                          |
|-------------------------------------|----------------------------------|
| `POST /internal/gossip`             | Gossip exchange                  |
| `POST /internal/scheduling/intent`  | Scheduling decision notification |
| `POST /internal/scheduling/plan`    | New shard plan distribution      |
| `POST /internal/model/transfer`     | Model file transfer (peer-to-peer) |
| `POST /internal/rpc-warm`           | Distributed inference: host asks a worker to seed its RPC tensor-cache shard before a distributed load (auto-warm). `serve_model_file` honors `Range` for shard-only fetch. The host distributes only to ELIGIBLE workers (`sovereign-mesh::worker_eligibility` — settle + flap-quarantine, surfaced in `sovereign mesh status`); a remote crash mid-compute `GGML_ABORT`s the host, so distributed inference requires host supervision. See `docs/RPC_DISTRIBUTED_INFERENCE.md`. |
| `POST /internal/index/transfer`     | Corpus shard upload (push)       |
| `GET  /internal/index/serve`        | Corpus shard download (pull)     |
| `POST /internal/knowledge/search`   | Inter-node shard query (fan-out target) |
| `GET  /internal/latency/probe`      | Latency probe response           |

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
- `embed_http::http_embed_fn` — POSTs to `/v1/embeddings` so a node
  without a local embed model still ingests via the engine.
- `grounding.rs` — `GroundingConfig` + `search_for_grounding` +
  `format_knowledge_context`.
- **Dimensional contribution ledger**
  (`commonwealth-core::contributions`) — append-only event log
  (`LedgerEvent` variants `InferenceServed`, `InferenceReceived`,
  `KnowledgeQueryServed`, `ShardTransferred`, `StorageSnapshot`)
  with pure aggregation into per-node `NodeContributions`. No
  `balance`, no exchange rate, no ranking — units are
  incommensurable. Storage in
  `commonwealth-state::ContributionEmitter` (gossip-replicated
  `MeshStore` under `app_id = "contributions"`). Pull-side
  `ShardTransferred` is emitted by the merge leader on behalf of
  the peer that shipped bytes — the schema carries an explicit
  `from_node`, and the aggregator credits `bytes_served` to it.
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
- See [`docs/MESH_LOAD_AWARENESS.md`](./docs/MESH_LOAD_AWARENESS.md)
  for peer-admission, contribution ceiling, and foreground-yield.

### Test harness

`commonwealth-test-harness`:

- `SimulatedMesh` — orchestrates many `SimulatedNode`s in-process,
  each with its own `AppState` and HTTP listeners on random ports.
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

`commonwealth-state::MeshStore` — gossip-replicated SQLite KV (WAL
mode): `StoreEntry { app_id, key, value: Bytes, timestamp, origin:
NodeId }`, LWW conflict resolution, per-`app_id` namespace,
`RetentionGc` for TTL.

`commonwealth-app` — mesh app platform: `MeshAppManifest`
(gossiped), `AppPermissions` (`mesh_store_read`/`_write`,
`inference_access`, `knowledge_access`), `AppRegistry`,
`AppProcess` lifecycle, `AppPortMap` + `forward()` reverse-proxy.

**Mesh-replicated workspace**. The `alignment` family replicates a
working set of files (default `~/.claude/`) across mesh peers
without a central server. Newest-mtime-wins via `merge_shards`'s
`mutable_merge = "source_doc_id_newest_mtime"` policy. Projector
under exclusive lock; mtime-stable. CLI: `sovereign alignment`.
Corpus bytes are local-only (mutually-authenticated peers only —
not gossiped onto the open mesh).

### CLI

```
commonwealth init --name "..."          Create a mesh, get a join key
commonwealth join <key>                 Join an existing mesh
commonwealth status                     Mesh state, members, models, capacity
commonwealth balance                    Contribution ledger
commonwealth corpus install/remove/update/list/consolidate
commonwealth pause / resume             Graceful departure and return
commonwealth leave                      Permanent departure
commonwealth mesh members / set / revoke / peer
commonwealth daemon start/stop/status
```

The Commonwealth CLI is mostly placeholders today —
`daemon start` and `balance` are real; most others print
`(In production, this would …)` and exit 0. The HTTP API on :9741
is the actual control plane. Lifecycle UX lives under `sovereign
daemon`. See §7 Roadmap.

### Deployment

`contrib/`: `install.sh` (curl installer),
`systemd/commonwealth.service`,
`launchd/com.commonwealth.daemon.plist`.

### Desktop production-readiness (W1–W6)

A coordinated stack supporting the friends-and-family launch.
Failure modes addressed: daemon crash drops the whole UI, peer
work pins the GPU while the user is chatting. Components:

- **W1 — child-process daemon supervisor**
  (`sovereign-desktop/src-tauri/src/supervisor.rs`) — Tauri-free,
  broadcast-driven, heartbeat, exponential backoff
  (1s→5s→30s→2min), crash-loop ceiling, bounded stderr buffer,
  crash-log persistence to `<data_dir>/crash-logs/`. Opt-in via
  `SOVEREIGN_USE_SUPERVISOR=1`. The child-process boundary makes
  "daemon crashed → click Reconnect" a recoverable UI state instead
  of a dead window. Motivated by ggml/llama.cpp SIGSEGVs an
  in-process supervisor can't catch.
- **W2 — peer-admission middleware**
  (`commonwealth-api/admission.rs`) — applied to client-port
  `/v1/chat/completions` + internal-port
  `/v1/knowledge/search`. Local requests admit unconditionally;
  peer requests are rejected with 503 + `Retry-After` when paused,
  yielding to a recent local foreground request, or refused by the
  fair scheduler. The flat ceiling became a **`commonwealth_core::fair_sched::SchedCore<NodeId>`**
  (`AppStateInner.peer_sched`): a runtime-mutable global ceiling
  (`set_slots`, `0` = reject all) **plus a per-node concurrency cap**
  so one peer can't hog the pool, **reciprocity-scaled** — a
  contributor's effective cap rises toward the ceiling, read from a
  cached per-node weight table (`reciprocity_weights`, refreshed
  ~30 s by a daemon loop from the contribution ledger). This is the
  host-side convergence point for a shared-model fleet (every
  consumer's turn lands here as a peer request keyed on `X-Node-Id`).
  `PeerInflightGuard` is RAII (`release`s the node's slot on drop,
  accurate under panic unwind). The **same `SchedCore` policy** backs
  the chat server's turn scheduler (`sovereign-server/scheduler.rs`),
  so both admission gates are fair by identical rules.
- **W3 — tray status chip + pause submenu**
  (`sovereign-desktop/src-tauri/src/tray.rs`).
- **W4 — first-mesh-join consent** —
  `DesktopConfig.first_mesh_consent`; ConsentGate renders when
  unset.
- **W6 — crash-bundle "send to Alex"**
  (`crash_bundle.rs`) — markdown file at
  `~/Desktop/sovereign-crash-<ts>.md`, prefilled `mailto:`. No
  auto-upload; v1 ships transparency.

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
DTO-out, Tauri-free) so the desktop host and the `sovereign meshapp dev`
CLI server share one source of truth; the Tauri commands are thin wrappers
(permission gate + resolve the corpus's on-disk index). The ops are
**backend-agnostic**: `load_graph` dispatches on what the index
carries — a deterministic `investigation/` graph (UAP) or an `atlas/`
enrichment (Enron), projecting both into one DTO contract
(`GraphNodeDto` / `EdgeDto` / `NodeDetailDto`). The atlas adapter maps Entity atoms → nodes and
Relation/Event atoms → cited edges, resolving each `sec_NNNNN` evidence id
to a numeric `chunks.lance` row via `chapters.json` so `read_chunk`
dereferences the source document unchanged; `reconciliation` surfaces the
cross-origin merge log (canonical + folded surface forms + the signal that
fired) as the identity glassbox. Four first-party apps ship on this
surface: SF-LVT (`public/meshapp/lvt/`, deterministic parcel compute), UAP
Blue Book (`public/meshapp/uap/`, investigation graph), Enron
(`public/meshapp/enron/`, a story-first atlas experience: scale banner +
description-led on-ramp + CSP-safe force-graph + collapse timeline +
reconciliation reveal + cited drill-down), and **Wrapped**
(`public/meshapp/wrapped/`, a Spotify-Wrapped-form story-card show over the
user's own `conversations-anthropic` corpus). Wrapped's op is different in
kind: `wrapped_artifact` serves a **precomputed artifact**, never live
inference — `sovereign-meshapp/src/wrapped.rs` folds every figure
deterministically (full chunk scan via `all_chunks_full` + the chunker's own
`parse_turns` header grammar for per-turn timestamps; GLiNER `chunk_entities`
rows from `~/.sovereign/sovereign.db` for the entity cards, filtered by a
**case-profile generics pass**: a surface form the assistant's own prose
frequently writes lowercase is a common noun, not a name — corpus evidence
instead of an enumerated stoplist, with one glassbox line per build naming
what was dropped and why), runs a
**verbatim-citation audit** (`verify_wrapped_artifact`: every cited chunk id
must resolve, every embedded quote must be a verbatim substring of its chunk
— a failing artifact is never served), and caches
`<index>/wrapped/all-time.json` keyed on `_corpus_meta.json`'s
`last_updated`/fingerprint (desktop-native build trigger: opening the app
rebuilds a stale artifact on demand). Cards are typed (`scale` / `rhythm` /
`obsessions` / `cast` / `door`); absent data ⇒ absent card, and the bundle
SKIPS unknown card types — the forward-compat seam future enriched cards
(unresolved questions, reversals) ship through. Bundles compose the
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
**Local dev loop:** `sovereign meshapp dev <id>` (sovereign-cli-llm) serves a
bundle + its `_sdk/` and injects a `window.meshApp` that proxies the explorer
ops over HTTP to the same `sovereign-meshapp` functions, reading a local corpus
index — so a bundle is iterable against real data without the desktop;
`sovereign meshapp new <id> --corpus <c>` scaffolds one. **Corpus as a managed
dependency:** a manifest's `corpus_data` (size + the recipe the bundle ships,
carrying a `[prebuilt]` HF block) makes the corpus first-class — `MeshAppsSection`
shows its presence and, when missing, a one-click **"Get data (N GB) & Open"** that
stages the recipe (`meshapp_stage_corpus_recipe` → `~/.sovereign/recipes/`) and runs
the existing prebuilt install with a progress bar. **Curated registry:** `sovereign
meshapp publish/install/list` (sovereign-cli-llm `meshapp_registry.rs`) distribute an
app as a self-contained `tar.zst` (bundle + a copy of `_sdk/`); install verifies the
artifact's sha256 (refuses tampering) and unpacks under `~/.sovereign/meshapps/<id>/`.
TRUST = integrity (sha256) + curation (membership in the reviewed
`sovereign-recipes/meshapp-registry.toml`); `meshapp dev` runs installed apps. The host
enumerates them via `meshapp_installed_apps()`; in-window opening of an installed app
(serving it from the install dir via a `meshapp://` scheme) is the remaining
integration. End-to-end runbooks: `docs/MESHAPP_CONSUMER.md` (replicate a demo) and
`docs/MESHAPP_AUTHORING.md` (recipe → corpus → app → publish). **Isolation caveat:**
Tauri v2 does not gate app
commands per-window (tauri#9227) — a webview with IPC can invoke any
registered command — so `capabilities/meshapp.json` only narrows the
core/plugin surface; true isolation for UNTRUSTED third-party apps needs a
no-IPC bridge (custom protocol / postMessage), a deferred platform
milestone. The bundles are verified headlessly by
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

Open polish: tray icon tint, HintCues nudge to Sharing tab, the W1
PR-3 default-flip removing in-process EmbeddedDaemon, graceful
SIGTERM-with-grace on daemon shutdown.

### Pinned worker pods as inference peers

Ephemeral worker pods (Vast L40S rented via `pipeline pod up`)
join the mesh scheduler's inference pool as one more peer, scored
by the same OICP load balancer. Pods aren't gossiped — owner-
private, TLS-pinned, authenticated by Ed25519 `WorkerToken`. See
[`docs/PINNED_WORKER_AS_INFERENCE_PEER.md`](./docs/PINNED_WORKER_AS_INFERENCE_PEER.md)
and [`docs/EPHEMERAL_WORKER_PODS.md`](./docs/EPHEMERAL_WORKER_PODS.md).

---

## 6. How the four projects fit together

**Sovereign standalone** — Tauri / CLI / server runs against
`EmbeddedLlamaCpp`. Knowledge via `MeshCorpusManager` (named for
the mesh case but works without one). `EmbedFn` wraps the local
Embed slot.

**Commonwealth standalone** — daemon serving `localhost:9741`.
Any OpenAI-compatible client points at it. Knowledge ingest uses
`embed_http::http_embed_fn` so a node without a local embed model
still indexes via the engine.

**Sovereign + Commonwealth (integrated)** —
`sovereign-mesh::EmbeddedDaemon` runs Commonwealth in-process.
Runtime inference is wrapped in `MeshInferenceProvider`, which
OICP-routes synthesis to peers when scoring favours them. Both
sides share `sovereign_mesh::oicp_select` so Joiner's selected
model and Founder's served slot can't drift.
`complete_stream_with_id` returns model attribution alongside the
stream so peer-served completions show in
`ResponseProvenance.inference_backend` as
`"Qwen3.5-9B.Q8_0 @ peer mac-peer"`. Skills with
`privacy = "local_only"` short-circuit to local.

**Desktop attach mode** — both the desktop app and `sovereign
daemon` want :9741. The desktop probes
`http://127.0.0.1:9741/v1/models` at startup (`bootstrap::detect`);
on success it enters Attach mode: inference flows through
`RemoteApiProvider`, mesh mutations go over HTTP via
`sovereign-mesh::mesh_http`, and `commands::save_config` POSTs
`/v1/admin/reload` so the daemon swaps its `InferenceProvider` in
place. Smoke test at `sovereign/scripts/smoke-attach-mode.sh`.

`/v1/admin/reload` rebuilds only what changed:

| Changed field                           | Reload action                              |
|-----------------------------------------|--------------------------------------------|
| `models.primary` / `.fast` / `.embed`   | Rebuild via `ProviderFactory`, atomic swap |
| `daemon.client_port` / `.internal_port` | `restart_required: true`                   |
| `data.dir`                              | `restart_required: true`                   |

When `restart_required: true`, `save_config` falls back to
`launchctl kickstart -k gui/$(id -u)/com.sovereign.daemon` (macOS)
or `systemctl --user restart sovereign` (Linux).

---

## 7. Build, test, run

### Prerequisites

- Rust toolchain (stable)
- `cmake` (llama.cpp)
- `protoc` (LanceDB → `lance-table`); macOS: `brew install
  protobuf`; Debian: `apt install protobuf-compiler`
- For Commonwealth: `llama-server` + `rpc-server` from
  `llama.cpp` on `PATH`
- For desktop: Node.js + Tauri 2
  (`cargo install tauri-cli --version "^2"`)

### Build / test

The repo is **one unified Cargo workspace** — 35 members under the root
`Cargo.toml` (`sovereign/`, `commonwealth/`, `corpus-engine` + its carve-outs
are directories of member crates, **not** separate workspaces). Use the
**sovereign watcher** (`lint_status` / `test_status` MCP tools) for
compilation feedback — running `cargo build` / `cargo test` directly via Bash
contends with the watcher for the file lock and idles.

**Watcher liveness is heartbeat-driven and self-healing.** The
`WatcherCoordinator` loop stamps a shared `WatcherHeartbeat`
(`corpus-engine/src/update/watcher_coordinator.rs`) every iteration;
the status tools read it through `code/watcher_health.rs`. Every
`lint_status`/`test_status`/`build` response carries a `watcher`
object — `{live, reason, configured, heartbeat_age_secs, hint}`. When
`live` is false the result is *orphaned* and `status` is reported as
`watcher_down` (never `fresh_*`), so a stale run can't masquerade as
current — the failure mode behind "the watcher silently goes stale."
A daemon-side `WatcherSupervisor`
(`sovereign-cli-daemon/src/watcher_supervisor.rs`) owns the coordinator
and restarts it (bounded backoff) when the loop task dies or its
heartbeat freezes; `sovereign doctor`'s `watcher_live` check probes the
same signal, catching configured-but-dead — which a config-presence
check cannot. If the runner sections are commented out in
`.sovereign/sovereign.toml`, restore from
`.sovereign/sovereign.toml.with-watchers`.

```sh
# One workspace — build / check / test everything from the repo root:
cargo build --release --workspace          # bundled assets copied via build.rs
cargo check  --workspace --all-targets      # what CI's `check` job runs
# The user-facing CLI execs into 4 sibling binaries — rebuild all of them
# (editing one + rebuilding only the dispatcher is a silent no-op):
cargo build --release -p sovereign-cli -p sovereign-cli-daemon \
            -p sovereign-cli-dev -p sovereign-cli-llm
```

```sh
cargo test --workspace                      # no GPU / network / model weights (§12.4)
```

No tests require GPU, models, or network. Sovereign uses
`DeterministicInference` + in-memory SQLite + real FTS5 for
functional tests. Commonwealth's harness runs simulated meshes
deterministically.

### Run

```sh
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

# Commonwealth daemon
cargo build --release -p commonwealth-daemon
target/release/commonwealth-daemon init --name "Co-op"
target/release/commonwealth-daemon daemon start
```

Default ports:

| Port  | Service                                                       |
|-------|---------------------------------------------------------------|
| 9741  | Commonwealth/Sovereign client API (OpenAI-compatible)         |
| 9742  | Commonwealth/Sovereign internal API (mTLS)                    |
| 9743+ | `llama-server` instances                                      |
| 50051+| `rpc-server` instances for layer shards                       |
| 8080  | Sovereign HTTP server (configurable)                          |

---

## 8. Where to look for what

| You want to                                      | Read                                                                |
|--------------------------------------------------|---------------------------------------------------------------------|
| Understand the agent runtime                     | `sovereign/crates/sovereign-core/src/runtime.rs` + `runtime/handlers/` |
| See how plans are executed                       | `sovereign-core/src/executor.rs`                                    |
| Add a tool                                       | `sovereign-core/src/traits.rs` then a new file under `sovereign-tools/src/` |
| Add a corpus extractor                           | `corpus-engine/src/extractors/` then register in `engine/ingest.rs` |
| Add a corpus filter                              | `corpus-engine/src/filters/` (impl `DocumentFilter`) + `recipe.rs::FilterConfig` + `filters/loader.rs` |
| Bundle a generated data file in corpus-engine    | Place in `sovereign-recipes/<corpus>/data/`, append filename to `corpus-engine/build.rs::BUNDLED_ASSETS`, `include_bytes!(concat!(env!("OUT_DIR"), …))` in `filters/assets.rs` |
| Write a recipe                                   | `sovereign-recipes/<id>/recipe.toml` then add to `registry.toml` |
| Author a recipe via the agent loop               | `sovereign-tools/src/recipe_author/` + skill at `sovereign/modes/recipe-author/skill.toml` |
| Add an `http_api` recipe (REST source)           | See `corpus-engine/src/recipe.rs` round-trip tests                  |
| Add an investigation recipe                      | `enrichment.type = "investigation"` + `[[entity_types]]` + `[[relationship_types]]` + `[[patterns]]`; run via `sovereign enrich investigation build <id>` |
| Write a skill                                    | `sovereign/modes/<id>/skill.toml`                                   |
| Tune model selection per hardware                | `sovereign/models.toml`                                             |
| Understand the SCIP call graph                   | `corpus-engine-scip/` (`scip_graph.rs`, `scip_export.rs`)           |
| See the code-intelligence MCP server             | `sovereign/crates/sovereign-cli-dev/src/project_cmd.rs` (`cmd_serve`); long-running variant at `sovereign-cli-daemon/src/daemon_cmd.rs::run_daemon` |
| See the Sovereign HTTP MCP route                 | `sovereign/crates/sovereign-server/src/routes_mcp.rs`               |
| Trace a `/v1/chat/completions` end-to-end        | `commonwealth/docs/routing-field-guide.md`                          |
| Understand OICP routing                          | `oicp-types/src/lib.rs` + `sovereign-mesh/src/oicp_select.rs` + `commonwealth-inference/src/scheduler/oicp_select.rs` + `sovereign-inference/src/selector.rs` and [`docs/inference.md`](./docs/inference.md) |
| Understand index storage on disk                 | `corpus-engine/src/index/mod.rs`                                    |
| Understand the v2 atlas pipeline                 | [`corpus-engine/ENRICHMENT_V2.md`](../corpus-engine/ENRICHMENT_V2.md) + `corpus-engine/src/enrichment/pipeline/mod.rs` |
| Drive v2 enrichment from the CLI                 | `sovereign-cli-llm/src/enrich_cmd/`                                 |
| Understand the recipe registry                   | `corpus-engine/src/registry.rs` (+ `recipe.rs::bundled_recipe_toml`) |
| Understand delta updates                         | `corpus-engine/src/update/delta.rs`                                 |
| Understand scope expansion (filter delta)        | `corpus-engine/src/engine/expand.rs`                                |
| Understand KnowledgeView digest assembly         | `sovereign-tools/src/knowledge_view/` and [`docs/knowledge-view.md`](./docs/knowledge-view.md) |
| See where KnowledgeView is injected              | `sovereign-core/src/runtime.rs::splice_landscape_digests` + `traits.rs::LandscapeDigestProvider` |
| Understand ATOS lifecycle                        | `sovereign-atos/src/local/orchestrator.rs`, `charter.rs`, `approval.rs`, and [`docs/ATOS.md`](./docs/ATOS.md) |
| See the ATOS CLI surface                         | `sovereign-cli-dev/src/atos_cmd/` + `project_cmd.rs` (`cmd_found`, `cmd_amend`, `cmd_phase`, `cmd_audit`) |
| Run the long-running Sovereign daemon            | `sovereign-cli-daemon/src/daemon_cmd.rs` + `contrib/launchd` + `contrib/systemd` |
| Rotate daemon logs                               | `sovereign-cli-shared/src/util/log_rotation.rs`                     |
| Understand the loopback guard                    | `sovereign-mesh/src/loopback_guard.rs` + `admin_http::tests::loopback_guard_works_under_production_listener_shape` |
| Understand local-corpus snapshot/rollback        | `sovereign-tools/src/local_corpus/writeback.rs` + `frontmatter.rs`  |
| Pick the next daemon test to write               | [`docs/TESTING_SURFACE.md`](./docs/TESTING_SURFACE.md)              |
| Add a binary-bearing corpus (email / .docx / .xlsx / future calendar / transactions) | `corpus-engine/src/extractors/described_asset.rs` — register an `AssetSubExtractor` via `CorpusEngine::set_asset_sub_extractors`; the in-tree defaults cover xlsx / docx / plaintext / opaque |
| Read or extend the multi-origin reconciliation primitive | `corpus-engine/src/enrichment/reconciliation/{mod,multi_origin,oplog,signals}.rs` — operates on `Vec<Entity>` with `Provenance` (AD-4); writes `atlas/reconciliation_oplog.jsonl` reversible op log |
| Score a clustering of mention-ids vs ground truth (B³ + pairwise-F1) | `sovereign-eval/src/entity_resolution_score.rs` (scorer) + `entity_resolution_bench.rs` (Split/peek-budget) |
| Run the Phase 5 Enron measurement loop | `sovereign bench enron run --corpus enron-sample-onemailbox --split train --policy {pre_reconciliation\|tuned}` → `sovereign-cli-llm/src/bench_cmd/enron.rs` |
| Add another typed Entity column-extractor for tabular asset kinds | `corpus-engine/src/extractors/column_aware.rs` — extend `ColumnHeaderMap` or write a per-asset-kind extractor reading the parquet parsed-form cache directly |
| Content-addressed asset store on disk | `corpus-engine/src/asset_store/{mod,fs,ledger}.rs` (AD-1; raw bytes + parsed-form caches + append-only ledger under `<corpus>/assets/`) |

---

## 9. Glossary

- **OICP** — Open Inference Capabilities Protocol (v0.3). Wire
  types in `oicp-types`. A model publishes one `CapabilityClaim`
  per kind-of-work it does well; schedulers score requests against
  claims with shared protocol-level + per-scheduler operational
  adjustments. See [`docs/inference.md`](./docs/inference.md).
- **CapabilityHint** — Validated tag identifying a kind of work.
  Standardized: `general`, `code`. Open vocabulary via `x:<tag>`.
- **Recipe** — A TOML file in `sovereign-recipes` describing how
  to ingest one corpus end-to-end.
- **Registry** — The recipe catalog at
  `sovereign-recipes/registry.toml` (single source of truth).
  `corpus-engine`'s `build.rs` vendors it into `OUT_DIR` as the
  compile-time bundled snapshot; can refresh from GitHub at runtime.
- **DocumentFilter** — Trait between extract and chunk that drops
  `ExtractedDoc`s by predicate. Composable via `[[filter]]`.
- **FilterPipeline / ScopeMeta** — A recipe's filter set + its
  hash. Stored in `_corpus_meta.json`; lets a corpus expand in
  place by relaxing filters and delta-ingesting.
- **Field Model (v1)** — Five-phase enrichment (skeleton → cluster
  → align → fault lines → open questions) that analyses a corpus
  holistically rather than per-chunk.
- **Domain (v1)** — Trait encoding the epistemic conventions of a
  knowledge field (philosophy, science, …). The single extension
  point for v1.
- **Atlas (v2)** — Typed atom graph + `Pipeline` trait + registry +
  `ExemplarBank` + `PhaseCache`. See `ENRICHMENT_V2.md`.
- **SCIP** — Source Code Intelligence Protocol. `scip_graph.rs`
  stores SCIP data in SQLite; `scip_export.rs` dispatches to
  language-specific analyzers.
- **CodeWatcher** — `notify`-crate filesystem watcher. Re-indexes
  modified files via `CorpusEngine::reindex_file` and marks them
  stale in the call graph (800 ms debounce).
- **Shard** — A `corpus-engine` index containing only a contiguous
  chunk-ID range. Structurally identical to a complete index.
- **Skill** — A TOML file configuring routing triggers, planner
  templates, prompt overrides, memory rules, and OICP requirements
  for a class of work.
- **Slot** — A model-loading position in `EmbeddedLlamaCpp`
  (Quick / Main / Code / Embed). See
  [`docs/inference.md`](./docs/inference.md).
- **Mesh** — A closed trust ring of Commonwealth nodes that share
  inference and knowledge. Joined via a `cwth-XXXX-XXXX-XXXX` key.
- **Peering** — A trust relationship between two distinct meshes
  that lets them exchange models or knowledge under a chosen
  `PeerTrustLevel`.
- **EmbedFn / InferenceFn** — Function types `corpus-engine`
  accepts from its caller for embedding text and (optionally)
  running an LLM during enrichment. Keeps the engine free of any
  specific runtime.
- **EmbedModelInfo** — `{ model_id, dimensions, pooling,
  normalization }`. Cross-peer interoperability contract.
- **KnowledgeView** — Three-map landscape-digest system (personal
  memories, 180-day conversation history, institutional notes)
  spliced into the system prompt before each turn. Strict
  local-scope privacy is structural, not policy.
- **ATOS** — Agent Task Orchestration System. Two-layer
  scaffolding (project charter + per-feature specs) injecting the
  relevant contract into every agent turn, detecting SHA-256
  drift, recording decisions/deviations/milestones under
  `.sovereign/`.
- **Charter** — ATOS specification document (`CHARTER.md` for the
  project, `spec.md` for a feature). Committing it is approval.
- **Drift** — ATOS term for "spec file changed since approval."
  Warns next turn; does not block. Either revert or
  `atos spec accept`.
- **Sovereign-coder pipeline** — Commonwealth middleware chain
  (`approval_gate → session_briefing → context_injector →
  tool_injector → artifact_surface`) that adapts a generic coder
  model into an ATOS-aware one.
- **Work atlas** — Cross-mesh peer awareness:
  `work_in_flight` / `declare_scope` / `release_scope` MCP tools.
  See [`docs/WORK_ATLAS.md`](./docs/WORK_ATLAS.md).

---

## 10. Architecture roadmap

Work intentionally deferred. Listed so the next engineer inherits a
todo list rather than a surprise (per
[`ARCH_PRINCIPLES.md`](./ARCH_PRINCIPLES.md) §14.3). A big file or
a documented gap without an entry is a bug; a big file or gap with
an entry is sequenced work.

### 10.1 Sovereign deferrals

| Item | Location | Why deferred |
|------|----------|--------------|
| `project_cmd.rs` split | `sovereign-cli-dev/src/project_cmd.rs` (~7000 lines) | **De-scoped from the launch-pristine §3 bar (2026-06-08):** `sovereign-cli-dev` is feature-gated out of the default/public build — the dispatcher gates its verbs behind `--features dev-tools` — so this developer-toolchain file is not part of the end-user product. Subcommand-per-file remains the eventual split shape; still gated on post-found project-lifecycle settling. |
| `model_slot.rs` residual (was the `embedded.rs` split) | `sovereign-inference/src/embedded/model_slot.rs` (~3,475 lines) | **The 9,669-line `embedded.rs` monolith was decomposed (PR5b + 2026-06-10):** one concern per submodule under `embedded/` (engine ~2,965 · model_slot ~3,475 · rpc_distribution ~1,168 · grammar ~1,146 · prompt_helpers ~786 · rpc_warm_cache ~668 · sampler ~567 · embed_slot ~548 · rerank_slot ~509), re-exported flat so `crate::embedded::<Item>` paths are unchanged. The residual `model_slot.rs` holds the slot state machine + decode loops + MTP — one tight, unsafe-heavy (44 blocks) FFI concern whose remaining seam is an alternate inference backend at the `InferenceProvider` boundary, not a file split. |
| `streaming.rs` refusal-retry duplication | `sovereign-core/src/runtime/streaming.rs` (~1,950 lines) | The 2026-06-10 runtime.rs decomposition moved the streaming dispatch here intact. Its KQ and Deep/Simple synthesis loops carry two NEAR-duplicate refusal-retry state machines that genuinely differ (error-frame + finish-reason handling) — unifying them is a measured behavior change, not a move. Same deferral class for the streaming-vs-non-streaming setup duplication (turn.rs). |
| `state.rs` decomposition (desktop) | `sovereign-desktop/src-tauri/src/state.rs` (~1430 lines, was 2347) | **In progress (2026-06-09):** config → `state/config.rs`, built-in skills → `state/builtin_skills.rs`, and four `bootstrap_with_progress` sub-phases → `state/builders/{health,store,inference,knowledge_view}.rs`. Each builder takes a **narrowed signature** (its own handles, not `&AppState` — §5.2) + a **mock-backed unit test** (a stub `InferenceProvider` + a temp `CorpusEngine`, plus the inference reuse-seam): **bootstrap phases ARE CI-testable via dependency injection** — only the literal model load isn't. 100 desktop tests green. **Extraction of the contiguous phases is complete.** The remaining bootstrap body — the `tools` registry and the `EmbeddedDaemon` wiring — stays inline *by necessity, not omission*: both are **interleaved** across the whole bootstrap (tools registered before AND after `corpus_engine`; `mesh.set_*` spread over four sites and order-bound to run before `try_resume`), so neither can be a pure-relocation builder without reordering a GGUF-gated startup path (§10.2). Keep `AppState` fields flat (~295 call sites borrow `state.<field>`). |
| `DesktopError` (desktop) | `sovereign-desktop/src-tauri/src/error.rs` + `src/lib/errors.ts` | **First PR landed (2026-06-09):** a structured `{code, message, suggested_action}` error replaces the `.map_err(\|e\| e.to_string())` → bare-`String` pattern (~295 handler sites). Rust `DesktopError` + snake_case `ErrorCode` (wire shape **pinned by a serialization test**) with `From<String>`/`From<&str>`, so a handler flips to `Result<_, DesktopError>` while its neighbours still return `String` and `?` keeps compiling across the seam. Frontend mirror: `DesktopError` type + pure, tested `isDesktopError`/`normalizeError` + `invokeChecked<T>()` + `toastError`. **Consumers so far:** `search_web` (via the additive `AppState::runtime()` accessor) + budget.rs's 4 daemon-HTTP commands (get/set_ingest_budget, get/set_mesh_quiesced — status/decode errors mapped to `upstream`). **Burn-down enabler (2026-06-09):** `invokeChecked` now throws the normalised error as an `Error` *instance* (structured fields attached via `Object.assign`), so the ~150 existing `e instanceof Error ? e.message : String(e)` catch blocks render the message unchanged — **migrating a command needs no per-caller edits**, just the Rust return-type flip + pointing its api.ts wrapper at `invokeChecked`. **Remaining (incremental, §10.2, ~140 command modules):** flip each handler's `-> Result<_, String>` → `DesktopError` (the `?`-sites auto-convert via `From<String>`; explicit `return Err` / tail `map_err` take `.into()` or a semantic `DesktopError::upstream`/`invalid_request`) + repoint its wrapper. The `store()`/`corpus_engine()` accessors + `require_runtime!` retirement land with the first chat-path module that needs them (deferred — chat is the live, higher-traffic path). |
| `atos_cmd/run.rs` split | `sovereign-cli-dev/src/atos_cmd/run.rs` (~4700 lines) | **De-scoped from the launch-pristine §3 bar (2026-06-08):** in the feature-gated `sovereign-cli-dev` developer toolchain (see `project_cmd.rs` row), not part of the public build. ATOS runner loop — subprocess fan-out, MCP-tool brokerage, milestone advancement, reviewer loop, run-record persistence cohere as one state machine today. One-file-per-stage split when boundaries stabilise. |
| `daemon_cmd.rs` split | `sovereign-cli-daemon/src/daemon_cmd/` (was 3803 → `mod.rs` 2378 + 5 submodules) | **Partial split done (2026-06-09):** the separable concerns moved to submodules following the `setup_cmd` recipe — `lifecycle` (start/stop/restart/reload/status + pidfile + port-probe + shutdown), `workspace` (auto-detect), `provider` (`LlamaCppFactory` hot-reload), `worker` (ephemeral-pod entry), `tool_registry` (MCP registry + merged SCIP graph). Cross-called fns are `pub(super)`; `home_dir_buf` stays in `mod.rs` as a shared ancestor-private; tests moved with their code (51 daemon tests green). **Then (also 2026-06-09)** the two **self-contained early phases** of the `run_daemon` bootstrap were extracted to `daemon_cmd/build/`: `preflight` (VRAM-capacity check — no outputs) and `inference` (`load_provider` — returns the provider + concrete engine handle + resolved embed family). Pure relocations, compile-verified (this startup path has no GGUF-free CI coverage); 51 daemon tests green. **Then (2026-06-15) the full bootstrap-TOC decomposition landed**, refuting the "interleaved → can't pure-relocate without reordering" call recorded here earlier: the remaining ~22 phases moved into `daemon_cmd/bootstrap.rs` (20 phase fns + a `WatcherAtlasSetup` bundle struct), taking `run_daemon` 1919→611 lines and `mod.rs` 2233→921. The enabling technique is **strict in-place extraction** — every call site stays in its exact position, so side-effect order is preserved *by construction* and any capture/borrow slip surfaces as a compile error, not a boot-time surprise — plus already-built handles passed as params, and for the one multi-output block (workspace watchers + work-atlas) a **return-bundle struct destructured at the call site back into the original local names**, leaving all ~7 downstream consumers byte-unchanged. `resolve_self_node_id` dedups the two byte-identical node-id resolutions. Verified: full-workspace `cargo check` + `cargo test` green. **Genuinely left inline** (a readability call, *not* interleaving): the config/stores preamble (flags → wizard → config → VRAM → stores) — already-readable guard-clauses whose only extraction blocker is early-`return <exit-code>` paths; threading those through `Result`/`ControlFlow` would add boot-path indirection for little gain. |
| `mesh_cmd.rs` / `corpus_cmd.rs` split | `sovereign-cli-llm/src/{mesh_cmd,corpus_cmd}.rs` (was 3868 → mesh_cmd 915 + corpus_cmd 2956) | **DONE (2026-06-09):** the `corpus` half (~2960 lines of `cmd_corpus_*` + helpers + `HELP_CORPUS`) split out of `mesh_cmd.rs` into `corpus_cmd.rs`, fixing the dispatch naming lie (the one file served both the `mesh` AND `corpus` verbs). `run_corpus` re-pointed at both callers (`main.rs` + `alignment_cmd.rs`); `mesh_data_dir` now imported from `sovereign_cli_shared::dirs` in both files; `hostname` stays private to `mesh_cmd` — corpus turned out to use neither, so there's **no cross-module coupling**. 498 llm tests green; `mesh_cmd.rs` is now ~915 (under the §3.1 ceiling). **Then (also 2026-06-09)** `corpus_cmd.rs` was further broken into `corpus_cmd/{mod,fmt,inventory,diagnostics,partitions}.rs`: `fmt` is the shared-formatter leaf, `inventory`/`partitions` use it, `diagnostics` borrows the partition-discovery helpers, `mod` is the dispatcher. **All five files are now under the §3.1 ceiling** (mod 116, fmt 52, inventory 624, diagnostics 1155, partitions 1050); cross-submodule fns are `pub(super)`. 498 llm tests green. (A stale duplicate `sovereign-cli-dev/src/mesh_cmd.rs` — never compiled — was deleted 2026-06-01.) |
| `setup_cmd.rs` split (CLI) | `sovereign-cli-daemon/src/setup_cmd/` (was 1609 lines → `mod.rs` 977 incl. tests + 6 submodules) | **DONE (2026-06-09):** behaviour-preserving §3.2 folder split into `args`/`catalog`/`byom`/`download`/`finish`/`opencode`. **The reusable recipe for the `daemon_cmd`/`mesh_cmd` splits above:** shared `Opts`/`ModelPaths`/`Pick` types stay in `mod.rs` (submodules read them as ancestor-privates → zero field-visibility churn); `run_setup`/`run_repair` orchestrate via `use` imports so their bodies stay byte-identical; cross-called fns are `pub(super)`; `download_with_progress` stays `pub(crate)` re-exported for `daemon_cmd`; test modules stay in `mod.rs` with explicit submodule `use`s. 51 daemon tests green. Related Phase 2 CLI infra (same period): the shared `sovereign_cli_shared::args` parser + collapse of the three `util.rs` re-export shims. |
| `daemon.rs` split | `sovereign-mesh/src/daemon.rs` (~2600 lines) | `EmbeddedDaemon` is the in-process commonwealth+sovereign entry. Pure helpers (`mesh_discovery.rs`) extracted; load-bearing splits (`app_state_builder.rs` + `background_tasks.rs`) unblocked but stay deferred until `MemberRecord.client_port` lands and a real two-daemon integration test against `start_daemon` itself can be built. |
| `inference_adapter.rs` split | `sovereign-mesh/src/inference_adapter.rs` (~2100 lines) | Pure helpers (`build_self_manifest`, `synthesize_slot_claims`) extracted to `oicp_synthesis.rs`. Wire-shape translation, tool-call envelope parsing, tool-profile policy stay until the tool-call envelope migration settles. |
| `peer_inference.rs` split | `sovereign-mesh/src/peer_inference.rs` (~2280 lines) | `MeshInferenceProvider` + throughput observation + manifest caching + quarantine. `ThroughputObservedStream` extracted to `throughput_tracking.rs`. `complete_stream_with_id_and_finish` and `complete_stream_with_id` deduplication blocked on `select_route` enum extraction. |
| `auto_ingest.rs` split | `sovereign-mesh/src/auto_ingest.rs` (~1200 lines) | Auto-collaborate orchestration — `Planning → Handoff → Active → Complete` state machine. Splitting before the cloud-peer flavour settles would re-merge. |
| `sqlite.rs` split | `sovereign-store/src/sqlite.rs` (~3678 lines) | `StateStore` trait-impl hotel — 14 sub-trait impls, one per store concern. Cleanly delineated by trait boundary; split into `stores/<concern>.rs` if it crosses ~4000 lines. |
| `document_asset.rs` split | `sovereign-tools/src/document_asset.rs` (~3617 lines) | DocumentAssetManager — tiered (T1/T2/T3) ingest orchestration + skeleton/RAPTOR persistence. Splits along the tier boundary once the tiered surface stops evolving. |
| `runtime/retrieval.rs` split | `sovereign-core/src/runtime/retrieval.rs` (~3385 lines) | Retrieval pipeline — chunk-fetch + atlas grounding + hybrid entity scorer + query expansion. Hot-iteration file (active query-expansion work); split when the retrieval algorithm settles. |
| `found.rs` split | `sovereign-cli-dev/src/found.rs` (~2750 lines) | `sovereign project found` four-stage founding conversation. Splits one-file-per-stage when the founding flow stabilises. |
| `MemberRecord.client_port` wire field | `commonwealth-core/src/mesh.rs` + `commonwealth-discovery/src/membership.rs` + `sovereign-mesh/src/daemon.rs::peer_inference_endpoints` + `sovereign-mesh/src/auto_ingest.rs` | Local-side port plumbing landed; **peer-uniformity assumption** remains: `peer_inference_endpoints` rewrites every peer URL with this daemon's client_port, and `auto_ingest` pins port `9742`. Mixed-port mesh deployments need a `client_port` field on `MemberRecord` and a matching slot in the join handshake. Until then, operators who set a non-default `client_port` should configure every peer the same. |
| Atlas inspector Phase 2 — curation overlay | `sovereign-tools/src/atlas_view/` | Phase 1 ships read-only inspection. Phase 2 adds an `atlas/overlay.sqlite` keyed by `StableAtomKey` (content-hash) so user edits and approval state survive re-extraction. Forward-compat fields (`curation_status`, `overlay_supports`) already on every DTO. |
| Imports tab — Gemini extractor | `corpus-engine/src/extractors/` + `sovereign-recipes/conversations-gemini/` | Settings → Imports ships **Anthropic + ChatGPT** (2026-06). Gemini (Google Takeout) remains: the plumbing is source-agnostic — a new `<source>_export` extractor + recipe + `ImportSource` arm + `<ConversationImportCard>` is all it takes. ChatGPT pattern (mapping-tree walk-up, PUA marker cleaning, source-aware `import_commands.rs`) is the template. |
| Imports tab — KQ chip label for conversation corpora | `sovereign-core/src/runtime.rs` `KnowledgeQueryPlan` | DeepQuery path threads `display_categories`; streaming KQ + metalingual locator pass `None`. Sub-page UX polish. |

### 10.1b corpus-engine deferrals

| Item | Location | Why deferred |
|------|----------|--------------|
| `recipe.rs` split | `corpus-engine/src/recipe.rs` (~3500 lines) | Recipe TOML schema + loader + recipe-authoring tools + parameter resolution + `bundled_recipe_toml(id: &str)` dispatch. The §2-style enumify of `bundled_recipe_toml` (RecipeId enum) is a prerequisite. |
| `notes.rs` split | `corpus-engine-notes/src/notes.rs` (~5634 lines) | NoteStore façade + persistence migrations + lifecycle + decision-log tools. **Carved out of `corpus-engine` into its own crate** (blast-radius control) — that isolation was the higher-priority move; the in-file split is still wanted. SQL schemas + migrations couple tightly. |
| `entity_extraction.rs` split | `corpus-engine/src/enrichment/entity_extraction.rs` (~2930 lines) | Phase-1b entity extraction for personal + conversational domains. Active surface (recent enrichment work); split along the per-domain extractor boundary once it settles. |
| `atlas/resolution.rs` split | `corpus-engine/src/enrichment/atlas/resolution.rs` (~4500 lines) | Atlas URI resolution + scoring. Hottest-iteration file; splitting churn-heavy code obscures git history while the algorithm is still settling. |
| `pipeline/runner.rs` split | `corpus-engine/src/enrichment/pipeline/runner.rs` (~3100 lines) | v2 atlas orchestrator. Phase dispatch + ExemplarBank + PhaseCache + step retry all touch the same state. |
| `engine/mod.rs` split | `corpus-engine/src/engine/mod.rs` (~3000 lines) | `CorpusEngine` façade. Plausible after watcher-driven recipes settle and `ingest_driver` enumify lands. |
| `pipelines/literary_atlas.rs` split | `corpus-engine/src/enrichment/pipeline/pipelines/literary_atlas.rs` (~2900 lines) | Splits naturally along phase boundaries (extract, cluster, name, resolve, synthesize). |

### 10.2 Commonwealth deferrals

| Item | Location | Why deferred |
|------|----------|--------------|
| `frontdoor.rs` split | `commonwealth-api/src/frontdoor.rs` (~5758 lines) | Harness-protocol → model-native normalizer — 9 concerns (harness detect, tool keeplist, heredoc diagnostics, distiller, path repair, nudges, allowlists, brief). Shares path-canon / tool-rewrite logic with `routes_responses.rs`; sequenced as the harness-unification PR (extract a shared reshaping core), not a bare size split. |
| `routes_responses.rs` split | `commonwealth-api/src/routes_responses.rs` (~3140 lines) | `/v1/responses` OpenAI-adapter — request/SSE translation + tool rewriting + path canon. The path-canon + tool-rewrite halves dedupe with `frontdoor.rs` into the shared reshaping core (same PR). |
| Multi-embed-model dispatch | `commonwealth-api/src/routes_inference.rs` | `/v1/embeddings` ignores the `model` field; gated on a second production embed model. |
| `embed_batch` | `commonwealth-api/src/routes_inference.rs` | Inputs fan out one at a time; gated on a backend that batches more efficiently. |
| Knowledge replica fanout | `commonwealth-api/src/routes_knowledge.rs` | Knowledge fan-out only hits non-hosted corpora today; gated on merge-dedupe hardening. |
| mesh_store gossip replication | `commonwealth-api/src/routes_internal.rs` | Gossip replicates the `Mesh` member list only. `all_entries_for_gossip` is defined but unused; sender half + `POST /internal/app/state` receiver missing. Workaround: explicit peer push at queue-handoff time. |
| Mesh Health attach-mode HTTP | `commonwealth-api/src/state.rs` + `sovereign-desktop/src-tauri/src/mesh_commands.rs` | Local-mode UI works; attach mode silently returns empty for `mesh_get_contributions` / `mesh_set_peer_preference` because the daemon doesn't expose these over HTTP yet. |
| ATOS middleware no-op fall-through | `commonwealth-api/src/routes_inference.rs` | When no session store is configured, the ATOS pipeline degrades to legacy routing. By design; operators should expect the silent fall-through. |
| `commonwealth` CLI is mostly placeholders | `commonwealth-daemon/src/main.rs` | `daemon start` and `balance` are real; many others print `(In production, this would …)` and exit 0. The HTTP API on :9741 is the actual control plane today. Decide per-command whether to implement against the HTTP surface or remove; don't bulk-fix in one PR. |

### 10.3 Doc posture

The two long-form commonwealth docs —
`commonwealth/ARCHITECTURE.md` and
`commonwealth/IMPLEMENTATION_PLAN.md` — are flagged at their top
as historical record. They preserve the original design rationale
(and the constitutional Design Philosophy section in
ARCHITECTURE.md still governs the project) but are not maintained
against current code shape. This file (§5 in particular) is the
source of truth for the running system.
