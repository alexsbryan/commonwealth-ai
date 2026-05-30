# Commonwealth AI — System Overview

A navigation primer. Read this on day one to know what exists, how
the pieces fit, and where to look for each subsystem. Read
[`sovereign/ARCH_PRINCIPLES.md`](./ARCH_PRINCIPLES.md) on day two for
the rules of engagement.

This file is a contract per `ARCH_PRINCIPLES.md §1.1`: every claim
must be verifiable against the code on the commit it appears in. If
you change a subsystem, update its entry in the same PR.

---

## 1. The four projects

```
commonwealth-ai/
├── oicp-types/          # OICP wire types — no other deps
├── corpus-engine/       # Knowledge layer (LanceDB + Tantivy)
├── corpus-engine-scip/  # SCIP call graph + per-language exporter dispatch
├── sovereign-recipes/   # Recipe TOMLs + generated data — pure data
├── sovereign/           # Local AI assistant (CLI / desktop / server)
└── commonwealth/        # Mesh coordination daemon
```

| Project              | Role                                          | Depends on                                            |
|----------------------|-----------------------------------------------|-------------------------------------------------------|
| `oicp-types`         | OICP v0.3 wire types + scoring helpers        | —                                                     |
| `corpus-engine`      | Acquire → extract → filter → chunk → embed → index | `oicp-types`, `corpus-engine-scip` (treesitter feature) |
| `corpus-engine-scip` | SCIP call graph store + exporter dispatch     | —                                                     |
| `sovereign-recipes`  | Pure data — recipe TOMLs + bundled assets     | —                                                     |
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
- `notes.rs`, `features.rs`, `plan_items.rs` — NoteStore + ATOS
  FeatureStore (SQLite + FTS5)
- `meta_atlas/` — cross-corpus articulation classifier + index
- `rough_edges.rs`, `git_archaeology.rs`, `pii.rs`,
  `alignment_projector.rs` — operator-facing scanners

### sovereign

```
crates/
├── sovereign-core           # Traits, runtime, planner, executor, router, memory
├── sovereign-inference      # llama.cpp slots, remote OpenAI-compat, hybrid w/ failover
├── sovereign-store          # SQLite + Postgres + in-memory StateStore
├── sovereign-tools          # Built-in tools (search, knowledge, docs, web, MCP, code-intel)
├── sovereign-atos           # ATOS lib (charter, approval, report, session, local orchestrator)
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

Recipe catalog (`registry.toml`) plus one directory per recipe.
Current set: `wikipedia`, `wikipedia-simple`, `wikipedia-newsworthy`,
`wikipedia-article`, `wikipedia-catalog`, `sep`, `stackexchange`,
`stackexchange-knowledge`, `openalex`, `gutenberg`, `gutenberg-work`,
`crs_reports`, `codebase`, `conversations-anthropic`, `routing`,
`scotus-opinions`, `olc-opinions`, `federal-register-presidential`,
`us-code`, `book-report`, `knowledge-gym`, `search-gym`,
`arch-principles`, `system-overview`. Underscore directories like
`_templates` carry scaffolding.

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
| Extractor  | `mediawiki_xml`, `stackexchange_xml`, `jsonl`, `wikipedia_jsonl`, `wikipedia_structured`, `html`, `html_sections`, `csv`, `parquet`, `plaintext`, `code`, `email` (RFC-5322 + MIME), `described_asset` (content-addressed binary dispatcher), `column_aware` (typed Entity atoms from parquet parsed-form caches) |
| Filter     | `pageview_rank`, `title_list`, `boilerplate` (email signature / quoted-reply / disclaimer stripping), composed via `[[filter]]` (`Any` / `All`) |
| Chunker    | `paragraph`, `sentence`, `fixed`, `semantic`, `passthrough`, `portal_event_bullet`, `threaded_turns` |
| Index      | `CorpusIndex` over LanceDB (IVF-PQ) + Tantivy FTS                  |

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

Default embedding model: `qwen3-embedding-0.6b` (768 dims).
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

Two coexisting systems; opt-in per recipe.

- **v1 — `enrichment/field_engine.rs`** — five-phase pipeline
  (skeleton → cluster → align → fault lines → open questions).
  `Domain` trait + `DomainRegistry`. Domains include `philosophy`
  (full), `multi` (Wikipedia), `personal` / `conversational` /
  `institutional` (KnowledgeView).
- **v2 atlas — `enrichment/pipeline/`** — typed atom graph
  (atom types include Entity, Claim, Event, Question, Position,
  Opposition, ArgumentReconstruction, Concept). `Pipeline` trait
  + registry + `ExemplarBank` + `PhaseCache`. Pipelines:
  `literary`, `literary_atlas`, `philosophy_atlas`,
  `referential_atlas`, `conversation_atlas`. State at
  `~/.sovereign/indexes/<corpus>/atlas/`.

See [`corpus-engine/ENRICHMENT_V2.md`](../corpus-engine/ENRICHMENT_V2.md)
for status table, landing-by-landing scope, and validation targets.

### Recipe registry

Six-plus recipes shipped in `sovereign-recipes`, consumed via
`RecipeRegistry`:

- **Bundled snapshot** — `registry_snapshot.toml` is `include_str!`'d
  so the engine works fully offline.
- **Bundled fallback** — `recipe.rs::bundled_recipe_toml(id)`
  returns the full recipe TOML for snapshot entries when the live
  URL is unreachable.
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
  (privacy = `local_only`).

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
| `sovereign-server`  | Axum REST + WebSocket on configurable port; multi-tenant via `tenant.rs`; SSE + WS streaming; server-side `ApprovalChannel` w/ `/v1/tasks/{id}/approve`. |
| `sovereign-desktop` | Tauri 2 + Svelte 5; setup wizard, chat w/ streaming + provenance, knowledge management (`KnowledgeStatus`, `CorpusProgressBanner`), skill manager, mesh UI, `sovereign://` deep-link handler, system tray. |

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
- **TLS** — `tls.rs` generates per-session certs with `rcgen`;
  pinned on the internal API.
- **Mesh peering** — `peering.rs`; two `PeerTrustLevel`s:
  `ModelAndKnowledgeSharing`, `Full`.

### Scheduling + orchestration

`commonwealth-inference/scheduler/` is a pure-functional layer
over gossiped state. A deterministic per-decision leader (lowest
`NodeId`) prevents thrash without consensus.

| Module                    | Algorithm                                                    |
|---------------------------|--------------------------------------------------------------|
| `layer_assignment.rs`     | Proportional VRAM, contiguous ranges per node, topology-aware, privacy-aware entry-node preference |
| `plan_builder.rs`         | `build_shard_plan`, `build_inference_plan`, `estimate_performance` (TPS / TTFT) |
| `knowledge_assignment.rs` | Greedy by free storage; whole-corpus if it fits, else `ChunkRange` split; respects per-corpus `mesh_sharing` |
| `oicp_select.rs`          | Shared OICP scoring (see [`docs/inference.md`](./docs/inference.md)) |
| `oicp_cache.rs`           | Hashes `CapabilityRequirements` to `(ModelId, score)` keyed by portfolio version |
| `portfolio.rs`            | `ModelPortfolio` w/ `ModelTransition` state machine; `SWAP_THRESHOLD = 0.3` |
| `usage_predictor.rs`      | `(weekday, hour, CapabilityCategory)` → preemptive loading   |
| `adaptive.rs`             | Adaptive scheduler hooks                                     |

`commonwealth-inference/orchestrator/`:

- `Orchestrator::apply_shard_plan` spawns `llama-server` on the
  entry node and `rpc-server` on remote nodes holding layer subsets.
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
| `/v1/mesh/*` `/v1/admin/*` `/mcp/*` | **Loopback-only** (router middleware + per-handler `enforce_localhost`) |

**Internal API — :9742, mTLS**

| Path                                | Purpose                          |
|-------------------------------------|----------------------------------|
| `POST /internal/gossip`             | Gossip exchange                  |
| `POST /internal/scheduling/intent`  | Scheduling decision notification |
| `POST /internal/scheduling/plan`    | New shard plan distribution      |
| `POST /internal/model/transfer`     | Model file transfer (peer-to-peer) |
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
  yielding to a recent local foreground request, or above the
  configured ceiling. `PeerInflightGuard` is RAII so the count
  stays accurate under panic unwind.
- **W3 — tray status chip + pause submenu**
  (`sovereign-desktop/src-tauri/src/tray.rs`).
- **W4 — first-mesh-join consent** —
  `DesktopConfig.first_mesh_consent`; ConsentGate renders when
  unset.
- **W6 — crash-bundle "send to Alex"**
  (`crash_bundle.rs`) — markdown file at
  `~/Desktop/sovereign-crash-<ts>.md`, prefilled `mailto:`. No
  auto-upload; v1 ships transparency.

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
`"Qwen3.5-9B.Q8_0 @ peer BeefyMac"`. Skills with
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

Each project is its own Cargo workspace. Use the **sovereign
watcher** (`lint_status` / `test_status` MCP tools) for compilation
feedback — running `cargo build` / `cargo test` directly via Bash
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
cd corpus-engine && cargo build --release   # bundled assets copied via build.rs
cd sovereign     && cargo build --release
cd commonwealth  && cargo build --release
```

```sh
cd corpus-engine && cargo test
cd sovereign     && cargo test --workspace
cd commonwealth  && cargo test --workspace
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
  `sovereign-recipes/registry.toml`. `corpus-engine` ships a
  compile-time bundled snapshot; can refresh from GitHub.
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
| `project_cmd.rs` split | `sovereign-cli-dev/src/project_cmd.rs` (~7000 lines) | Subcommand-per-file is the obvious shape; gated on post-found project lifecycle settling so we know which subcommands are sticky vs exploratory. |
| `embedded.rs` split | `sovereign-inference/src/embedded.rs` (~9500 lines) | Embedded daemon glue — slot management, lifecycle, HTTP handlers, MTP dispatch, sibling pool all cohere today; split when an alternate embedding mode forces the seam. |
| `commands.rs` (Tauri) split | `sovereign-desktop/src-tauri/src/commands.rs` (~5100 lines) | Tauri command-registration surface; splitting requires re-grouping by feature without breaking the IPC name registry. Coordination cost > current pain. |
| `atos_cmd/run.rs` split | `sovereign-cli-dev/src/atos_cmd/run.rs` (~4700 lines) | ATOS runner loop. Subprocess fan-out, MCP-tool brokerage, milestone advancement, reviewer loop, done-marker accept, run-record persistence all cohere as one state machine today. Split is one-file-per-stage when boundaries stabilise. |
| `daemon_cmd.rs` split | `sovereign-cli-daemon/src/daemon_cmd.rs` (~3300 lines) | Daemon Runtime construction + serve loop + watcher wiring. Cohesive while watcher subsystems keep settling. |
| `mesh_cmd.rs` split | `sovereign-cli-llm/src/mesh_cmd.rs` (~3000 lines) | Mesh CLI surface — peer ops, gossip introspection, partition tooling. Cohesive while peer-state semantics keep shifting under self-heal + cloud peering. |
| `daemon.rs` split | `sovereign-mesh/src/daemon.rs` (~2600 lines) | `EmbeddedDaemon` is the in-process commonwealth+sovereign entry. Pure helpers (`mesh_discovery.rs`) extracted; load-bearing splits (`app_state_builder.rs` + `background_tasks.rs`) unblocked but stay deferred until `MemberRecord.client_port` lands and a real two-daemon integration test against `start_daemon` itself can be built. |
| `inference_adapter.rs` split | `sovereign-mesh/src/inference_adapter.rs` (~2100 lines) | Pure helpers (`build_self_manifest`, `synthesize_slot_claims`) extracted to `oicp_synthesis.rs`. Wire-shape translation, tool-call envelope parsing, tool-profile policy stay until the tool-call envelope migration settles. |
| `peer_inference.rs` split | `sovereign-mesh/src/peer_inference.rs` (~2280 lines) | `MeshInferenceProvider` + throughput observation + manifest caching + quarantine. `ThroughputObservedStream` extracted to `throughput_tracking.rs`. `complete_stream_with_id_and_finish` and `complete_stream_with_id` deduplication blocked on `select_route` enum extraction. |
| `auto_ingest.rs` split | `sovereign-mesh/src/auto_ingest.rs` (~1200 lines) | Auto-collaborate orchestration — `Planning → Handoff → Active → Complete` state machine. Splitting before the cloud-peer flavour settles would re-merge. |
| `MemberRecord.client_port` wire field | `commonwealth-core/src/mesh.rs` + `commonwealth-discovery/src/membership.rs` + `sovereign-mesh/src/daemon.rs::peer_inference_endpoints` + `sovereign-mesh/src/auto_ingest.rs` | Local-side port plumbing landed; **peer-uniformity assumption** remains: `peer_inference_endpoints` rewrites every peer URL with this daemon's client_port, and `auto_ingest` pins port `9742`. Mixed-port mesh deployments need a `client_port` field on `MemberRecord` and a matching slot in the join handshake. Until then, operators who set a non-default `client_port` should configure every peer the same. |
| Atlas inspector Phase 2 — curation overlay | `sovereign-tools/src/atlas_view/` | Phase 1 ships read-only inspection. Phase 2 adds an `atlas/overlay.sqlite` keyed by `StableAtomKey` (content-hash) so user edits and approval state survive re-extraction. Forward-compat fields (`curation_status`, `overlay_supports`) already on every DTO. |
| Imports tab — ChatGPT + Gemini extractors | `corpus-engine/src/extractors/` + `sovereign-recipes/conversations-{chatgpt,gemini}/` | v1 of Settings → Imports ships Anthropic only. Plumbing is source-agnostic; lights up once a new `<source>_export` extractor + recipe register. |
| Imports tab — KQ chip label for conversation corpora | `sovereign-core/src/runtime.rs` `KnowledgeQueryPlan` | DeepQuery path threads `display_categories`; streaming KQ + metalingual locator pass `None`. Sub-page UX polish. |

### 10.1b corpus-engine deferrals

| Item | Location | Why deferred |
|------|----------|--------------|
| `recipe.rs` split | `corpus-engine/src/recipe.rs` (~3500 lines) | Recipe TOML schema + loader + recipe-authoring tools + parameter resolution + `bundled_recipe_toml(id: &str)` dispatch. The §2-style enumify of `bundled_recipe_toml` (RecipeId enum) is a prerequisite. |
| `notes.rs` split | `corpus-engine/src/notes.rs` (~3200 lines) | NoteStore façade + FeatureStore schema + persistence migrations + lifecycle + decision-log tools. SQL schemas + migrations couple tightly. |
| `atlas/resolution.rs` split | `corpus-engine/src/enrichment/atlas/resolution.rs` (~4500 lines) | Atlas URI resolution + scoring. Hottest-iteration file; splitting churn-heavy code obscures git history while the algorithm is still settling. |
| `pipeline/runner.rs` split | `corpus-engine/src/enrichment/pipeline/runner.rs` (~3100 lines) | v2 atlas orchestrator. Phase dispatch + ExemplarBank + PhaseCache + step retry all touch the same state. |
| `engine/mod.rs` split | `corpus-engine/src/engine/mod.rs` (~3000 lines) | `CorpusEngine` façade. Plausible after watcher-driven recipes settle and `ingest_driver` enumify lands. |
| `pipelines/literary_atlas.rs` split | `corpus-engine/src/enrichment/pipeline/pipelines/literary_atlas.rs` (~2900 lines) | Splits naturally along phase boundaries (extract, cluster, name, resolve, synthesize). |

### 10.2 Commonwealth deferrals

| Item | Location | Why deferred |
|------|----------|--------------|
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
