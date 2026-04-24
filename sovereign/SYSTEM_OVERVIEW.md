# Commonwealth AI — System Overview

A technical record of the as-built system, intended as a single entry point for
developers joining the project. This document describes what currently exists in
the workspace, not the original design documents from which it grew. For
historical design rationale see each repo's `ARCHITECTURE.md` and
`IMPLEMENTATION_PLAN.md`.

---

## 1. The Four Projects

The workspace contains four projects that compose into one coherent system:

```
commonwealth-ai/
├── oicp-types/          # Shared OICP type definitions (no other deps)
├── corpus-engine/       # Shared knowledge layer (no dependencies on the others)
├── sovereign-recipes/   # Corpus recipe definitions + registry catalog
├── sovereign/            # Sovereign — single-machine local AI assistant
└── commonwealth/        # Commonwealth — multi-machine inference + knowledge mesh
```

| Project             | Role                                     | Dependents              |
|---------------------|------------------------------------------|-------------------------|
| `oicp-types`        | OICP v0.2 type definitions + helpers     | Sovereign + Commonwealth|
| `corpus-engine`     | Ingest, index, search, and shard corpora | Sovereign + Commonwealth|
| `sovereign-recipes` | Declarative corpus recipe TOML files     | corpus-engine (compile-time snapshot) |
| `sovereign`          | Local agent runtime                      | Standalone or + mesh    |
| `commonwealth`      | Mesh coordination daemon                 | Standalone or + Sovereign|

The dependency direction is one-way: `oicp-types` and `corpus-engine` know
nothing about the other projects; both Sovereign and Commonwealth consume them
as path dependencies. `sovereign-recipes` is a pure-data repository consumed
by `corpus-engine` via a bundled snapshot at compile time. Sovereign optionally
embeds Commonwealth in-process via the `sovereign-mesh` crate, which is the
only point where the two upstream projects directly meet.

```
       ┌──────────────────┐          ┌────────────────────┐
       │   oicp-types     │          │ sovereign-recipes  │
       │ (OICP v0.2 types)│          │ (recipe TOML files)│
       └──────┬───────────┘          └────────┬───────────┘
              │                               │ registry_snapshot.toml
              │                    ┌──────────▼──────────┐
              │                    │   corpus-engine     │  (LanceDB + Tantivy)
              │                    └──────────┬──────────┘
              │                               │ EmbedFn / InferenceFn
              │                               │ + 3-op shard contract
              ├───────────┬───────────────────┼─────────────┐
              │           │                   │             │
       ┌──────▼──────┐   │    ┌──────────────▼──────┐  ┌──▼───────────┐
       │  Sovereign  │   │    │     Both call       │  │ Commonwealth │
       │  (sovereign/)│◄──┤    │  identical public   ├─►│              │
       │             │   │    │  API                 │  │              │
       └──────┬──────┘   │    └─────────────────────┘  └──────┬───────┘
              │          │                                    │
              │          └────────────────────────────────────┘
              │   sovereign-mesh embeds                       │
              └───────────────► commonwealth ◄────────────────┘
                          (in-process daemon)
```

Sovereign and Commonwealth share two protocols via the **`oicp-types`** crate:
**OICP** (Open Inference Capabilities Protocol — capability-aware model routing)
and the **`EmbedFn`/`InferenceFn` injection contract** for the corpus engine.
OICP has a canonical specification at `commonwealth/docs/oicp-v0.2.md`; the
shared types live in `oicp-types/src/lib.rs` and are re-exported as
`sovereign_core::oicp` and `commonwealth_core::oicp` so both projects share
the same wire format. The `EmbedFn` contract is defined once in `corpus-engine`
and each project implements its own closure over the same type.

---

## 2. Repository Map

### corpus-engine/

```
src/
├── lib.rs                    # Public API re-exports
├── engine/
│   ├── mod.rs                # CorpusEngine facade
│   ├── ingest.rs             # Ingestion pipeline (acquire → extract → chunk → embed → index)
│   └── reindex.rs            # Per-file incremental re-index (called by CodeWatcher)
├── recipe.rs                 # Recipe TOML schema + builtin recipes
├── registry.rs               # RecipeRegistry: bundled snapshot + remote fetch
├── types.rs                  # EmbedFn, InferenceFn, ChunkRange, IndexInfo, ScoredChunk
├── error.rs
├── progress.rs               # IngestProgress + ProgressCallback
├── safety.rs                 # robots.txt, rate limiting, scope enforcement
├── sharding.rs               # extract_shard / merge_shards
├── index/
│   ├── mod.rs                # CorpusIndex (LanceDB + Tantivy)
│   ├── create.rs             # Index creation + resume
│   ├── search.rs             # Hybrid search (vector + FTS5)
│   ├── write.rs              # Batch insert + delete
│   └── enrichment.rs         # Enrichment data storage
├── testing.rs                # Recipe test harness (acquisition, extraction, chunking, search)
├── acquirers/
│   ├── bulk_download.rs      # Resumable HTTP download
│   ├── huggingface.rs        # HF dataset acquirer
│   └── local_file.rs
├── extractors/
│   ├── xml.rs                # MediaWiki, Stack Exchange
│   ├── wikipedia_structured.rs
│   ├── wikipedia_jsonl.rs    # Wikipedia JSONL (section-level with metadata)
│   ├── wikipedia_types.rs    # WikipediaChunkMetadata, WikiLink
│   ├── json.rs               # JSONL, OpenAlex inverted index reconstruction
│   ├── html.rs
│   ├── csv.rs
│   ├── parquet.rs            # Parquet columnar (+ OpenAlex inverted-index transform)
│   ├── plaintext.rs
│   └── code/mod.rs           # Tree-sitter symbol extraction (Rust/TS/JS/Go/Python)
├── chunkers/
│   ├── paragraph.rs          # Hierarchical fallback splitter
│   ├── sentence.rs
│   ├── fixed.rs
│   ├── passthrough.rs        # Identity chunker (one chunk per input record)
│   └── semantic.rs           # Heading-aware
├── enrichment/
│   ├── field_engine.rs       # FieldModelEngine: 5-phase enrichment coordinator
│   ├── clustering.rs         # HDBSCAN-based chunk clustering
│   ├── skeleton.rs           # FieldSkeleton, CanonicalQuestion, PartialSkeleton
│   ├── alignment.rs          # Cluster-to-skeleton alignment
│   ├── fault_lines.rs        # Fault line detection between positions
│   ├── open_questions.rs     # Open question detection
│   ├── checkpoint.rs         # Resumable enrichment checkpointing
│   ├── domain.rs             # Domain trait — single extension point
│   ├── domain_registry.rs    # DomainRegistry: data-driven domain dispatch (replaces match)
│   ├── filter.rs             # Chunk eligibility filtering
│   └── domains/
│       ├── philosophy.rs     # Fully implemented (423 lines)
│       ├── multi.rs          # Wikipedia multi-domain
│       ├── personal.rs       # Personal-knowledge map (powers KnowledgeView)
│       ├── conversational.rs # Conversation-history map (powers KnowledgeView)
│       ├── institutional.rs  # Institutional-notes map (powers KnowledgeView)
│       ├── science.rs        # Stub
│       ├── policy.rs         # Stub
│       ├── legal.rs          # Stub
│       └── community.rs      # Stub
├── notes.rs                  # NoteStore: working notes + session reflections (SQLite, FTS5)
│                             #   NoteRow, ToolCallLogRow, write_note, write_reflection,
│                             #   read_notes (retired-aware), retire_by_tool/id,
│                             #   log_tool_call (10k ring buffer), read_reflections
│                             #   + ATOS kinds (deviation, redteam_finding, postmortem_pointer)
│                             #   + NoteScope dimension (Global / Feature / Session)
├── features.rs               # FeatureStore: ATOS feature rows, milestones, runs, tool events
├── sovereign_config.rs       # SovereignConfig loader (.sovereign/sovereign.toml)
├── project_docs.rs           # ProjectDocsStore: indexed SOVEREIGN.md + docs for project_context
├── lint_results.rs           # LintStore: persisted LintStatus output for agents
├── test_results.rs           # TestStore: persisted TestStatus output for agents
├── scip_graph.rs             # ScipGraph: SCIP call graph (SQLite, staleness tracking)
├── scip_export.rs            # Language-agnostic SCIP exporter dispatch
├── scip_proto.rs             # Minimal SCIP protobuf types (prost)
└── update/
    ├── mod.rs
    ├── watch.rs              # CodeWatcher: filesystem watcher → reindex + staleness
    ├── delta.rs              # Incremental index updates (version manifests, resumable)
    ├── watcher_coordinator.rs # WatcherCoordinator: debounced multi-watcher lifecycle
    ├── lint_watcher.rs       # LintWatcher: background cargo check → LintStatus
    ├── test_watcher.rs       # TestWatcher: background cargo test → TestStatus
    └── project_index_watcher.rs # ProjectIndexWatcher: SOVEREIGN.md + docs re-index

registry_snapshot.toml        # Bundled recipe catalog (compiled via include_str!)
xtask/                        # cargo xtask update-registry-snapshot

tests/
├── ingest_failure_modes.rs
├── parquet_ingest_e2e.rs
└── watcher_e2e.rs            # Filesystem watcher E2E tests
```

### sovereign/

```
crates/
├── sovereign-core/           # Traits, types, runtime, planner, executor,
│   src/                      #   router, memory, skills, OICP, model families
│   ├── traits.rs             # 5 trait boundaries
│   ├── types.rs              # ~700 lines of domain types
│   ├── runtime.rs            # Runtime orchestrator
│   ├── router.rs             # LlmRouter (two-pass classification)
│   ├── planner.rs            # LlmPlanner (DAG generation)
│   ├── executor.rs           # Step DAG executor with sampling/eval/tool loop
│   ├── memory.rs             # Working-memory compression, decay
│   ├── context.rs            # Context assembly
│   ├── skills.rs             # Skill loader + registry
│   ├── model_family.rs       # Per-family quirks (Qwen3, Gemma3, Llama3, ...)
│   ├── oicp.rs               # Capability profiles, requirements, manifest
│   ├── registry.rs           # ToolRegistry
│   └── stubs.rs
│
├── sovereign-inference/      # Inference providers
│   ├── embedded.rs           # llama.cpp via llama-cpp-2 FFI, dual-slot loader
│   ├── remote.rs             # OpenAI-compatible HTTP client
│   ├── hybrid.rs             # Multi-backend with health + failover
│   ├── selector.rs           # CapabilityAwareSelector / PrioritySelector
│   ├── health.rs             # EWMA latency, 3-strike availability
│   └── hardware.rs           # HardwareProfile detection (CPU/Low/Default/High/VeryHigh)
│
├── sovereign-store/          # Persistence
│   ├── sqlite.rs             # rusqlite + tokio Mutex, full StateStore impl
│   ├── postgres.rs           # tokio-postgres + deadpool, full StateStore impl
│   ├── memory.rs             # In-memory store for tests
│   └── migrations.rs         # Schema, FTS5 indices, soft-delete, sync columns
│
├── sovereign-tools/          # Built-in tools
│   ├── search.rs             # SearchTool: local + coverage assessment + web fallback
│   ├── web/{search.rs,extract.rs,mod.rs}   # DuckDuckGo / Brave / Tavily
│   ├── knowledge.rs          # Direct corpus query tool
│   ├── document.rs           # Map-reduce document summarizer
│   ├── epistemic.rs          # ClaimSearchTool, EpistemicLandscapeTool
│   ├── knowledge_view/       # KnowledgeView landscape digest assembly (§4.12)
│   │   ├── manager.rs        #   KnowledgeViewManager: lifecycle, observer, splice
│   │   ├── cross_view.rs     #   Cross-view resonance (0.75 cosine, ≤5 matches)
│   │   ├── recipes.rs        #   Three view recipes + SQL privacy filter
│   │   └── acquirers/sqlite.rs # SqliteAcquirer for memory/message/note sources
│   ├── code/                 # Code Intelligence + ATOS lifecycle tools
│   │   ├── symbol_lookup.rs  #   Exact symbol-name lookup (LanceDB filter)
│   │   ├── code_search.rs    #   Semantic code search (vector + FTS fallback)
│   │   ├── recent_changes.rs #   Symbols modified in last N hours (mtime)
│   │   ├── callees.rs        #   SCIP call graph: what does this function call?
│   │   ├── callers.rs        #   SCIP call graph: what calls this function?
│   │   ├── provision_feature.rs / archive_feature.rs    # ATOS FeatureStore writes
│   │   ├── record_atos_event.rs / promote_note.rs       # ATOS lifecycle events
│   │   ├── read_note_by_id.rs / read_note_digest.rs     # ATOS note surfaces
│   │   └── write_redteam_finding.rs                     # red-team finding persistence
│   ├── corpus/               # Corpus install + parsers (Wiki, OpenAlex, SEP,
│   │                         #   StackExchange, Gutenberg, Parquet, HTML, CRS)
│   ├── rag/{ingest.rs,parse.rs,chunk.rs}   # User-document RAG
│   ├── mcp/                  # Model Context Protocol client (stdio + HTTP+SSE)
│   ├── shell.rs file.rs email.rs calendar.rs compute.rs
│
├── sovereign-atos/           # ATOS library (§4.13)
│   ├── lib.rs                # AtosOrchestrator trait, RunMode, PreparedBrief
│   ├── local.rs              # LocalAtosOrchestrator — provision, milestones, reports
│   ├── charter.rs            # Charter markdown parser, auto-redteam detection
│   ├── approval.rs           # SHA-256 drift detection; git + MeshStore approval sources
│   ├── report.rs             # Milestone / red-team / epistemic report rendering
│   └── session.rs            # ATOS session state
│
├── sovereign-cli/            # Terminal REPL + named subcommands + ATOS CLI surface
├── sovereign-server/         # Axum REST + WebSocket, multi-tenant, approvals
├── sovereign-desktop/        # Tauri 2 + Svelte 5 native app
└── sovereign-mesh/           # In-process Commonwealth daemon embed
    ├── capabilities.rs       # build_local_capabilities — hosted_corpora + hardware for gossip
    ├── daemon.rs             # EmbeddedDaemon lifecycle (starts mDNS + Axum on 9742, client API on 9741)
    ├── deep_link.rs          # sovereign:// URL parser
    ├── gossip.rs             # Push-pull member-state gossip; re-publishes live capabilities per round
    ├── inference_adapter.rs  # SovereignInferenceAdapter: peers fetch /oicp/v1/capabilities from here; serves /v1/chat/completions
    ├── join.rs               # Same-LAN join handshake client (POSTs /internal/join)
    ├── knowledge_client.rs   # MeshKnowledgeClient — Runtime calls this to federate search
    ├── oicp_select.rs        # Shared OICP scoring + (score, size_gb) tie-break; pick_slot_for_oicp
    ├── peer_inference.rs     # MeshInferenceProvider — wraps local InferenceProvider, OICP-routes to peers
    ├── persist.rs            # mesh.json on-disk persistence
    ├── state.rs              # MeshState wrapper
    └── types.rs              # UI-friendly mesh types

skills/
├── research-analyst/skill.toml
├── epistemic-research/skill.toml
├── codebase-navigator/skill.toml  # 5 code tools, call graph tracing, security review
├── code-review/skill.toml
├── personal-assistant/skill.toml
└── inner-work/skill.toml          # privacy = "local_only"

data/corpora.toml                  # Compiled-in corpus + tier registry
models.toml                        # Per-hardware-profile model manifest
```

### commonwealth/

```
crates/
├── commonwealth-core/        # Shared types (re-exports oicp-types as oicp)
│   ├── ids.rs                # MeshId, NodeId, ModelId, ProcessId (16-byte)
│   ├── mesh.rs               # Mesh, MemberRecord, NodeStatus, MeshPeering
│   ├── capabilities.rs       # NodeCapabilities, HardwareProfile, GpuInfo
│   ├── model.rs              # ModelInfo, ModelArchitecture, ModelAvailability
│   ├── scheduler.rs          # ShardPlan, ShardAssignment, LayerRange
│   ├── knowledge.rs          # KnowledgeShardPlan, ChunkRange
│   ├── ledger.rs ledger_store.rs    # Append-only contribution ledger
│   ├── oicp_registry.rs      # OICP capability profile registry
│   ├── latency.rs            # LatencyMatrix
│   ├── config.rs             # DaemonConfig (TOML)
│   ├── model_aliases.rs      # Glob pattern → OICP synthesis
│   └── glob.rs default_aliases.toml
│
├── commonwealth-discovery/   # Membership and topology
│   ├── membership.rs         # generate_join_key (cwth-XXXX-...), BLAKE3 hash
│   ├── mdns.rs               # _commonwealth._tcp.local
│   ├── gossip.rs gossip_service.rs   # 10s epidemic gossip, LWW
│   ├── latency_probe.rs      # UDP RTT, EWMA α=0.3
│   ├── hardware.rs           # nvidia-smi → rocm-smi → Metal
│   ├── monitor.rs threshold.rs       # Resource polling + change detection
│   ├── tls.rs                # rcgen per-session certs + pinning
│   └── peering.rs            # Mesh-to-mesh trust establishment
│
├── commonwealth-inference/   # Scheduling, orchestration, inference plans
│   ├── inference_plan.rs     # InferencePlan, ShardPlan, ShardAssignment, LayerRange
│   ├── model.rs              # ModelInfo, ModelArchitecture, ModelAvailability
│   ├── model_aliases.rs      # Glob pattern → OICP synthesis
│   ├── ledger.rs ledger_store.rs     # Append-only contribution ledger
│   ├── oicp_registry.rs      # OICP profile registry
│   ├── store_adapter.rs      # InferenceStateStore persistence adapter
│   ├── scheduler/
│   │   ├── layer_assignment.rs   # Proportional, contiguous, topology-aware
│   │   ├── plan_builder.rs       # build_shard_plan, build_inference_plan
│   │   ├── knowledge_assignment.rs   # Greedy by free storage + replicas
│   │   ├── leader.rs             # Deterministic per-decision (lowest NodeId)
│   │   ├── oicp_cache.rs         # Hashed-requirement → ModelId cache
│   │   ├── portfolio.rs          # ModelPortfolio + transition state machine
│   │   └── usage_predictor.rs    # Weekday/hour capability distribution
│   └── orchestrator/
│       ├── orchestrator.rs       # apply_shard_plan, event emission
│       ├── process.rs            # ManagedProcess, ProcessState, spawn helpers
│       ├── health.rs             # HealthTracker, latency window, status enum
│       ├── fault.rs              # FaultDetector, FaultEvent
│       └── departure.rs          # Graceful 30s countdown state machine
│
├── commonwealth-api/         # HTTP servers
│   ├── server.rs             # Dual listeners: client (9741) + internal (9742, mTLS)
│   ├── routes_inference.rs   # /v1/chat/completions, /v1/models (OICP-aware + pipeline-alias routing)
│   ├── routes_knowledge.rs   # /v1/knowledge/search (fan-out + merge)
│   ├── routes_status.rs      # /status
│   ├── routes_oicp.rs        # /oicp/v1/capabilities provider manifest
│   ├── routes_internal.rs    # /internal/{gossip,scheduling,model,index,knowledge,latency}
│   ├── routes_apps.rs        # /v1/apps/* mesh-app lifecycle (register/list/start/stop)
│   ├── routes_app_internal.rs # /internal/app/* app-to-mesh bridge (store, knowledge, inference)
│   ├── middleware/           # Pipeline-alias middleware stack (ATOS sovereign-coder pipeline)
│   │   ├── mod.rs            # Middleware trait + MiddlewareRegistry
│   │   ├── approval_gate.rs  # Rejects writes until feature spec is committed
│   │   ├── session_briefing.rs # Prepends session brief to first system message
│   │   ├── context_injector.rs # Injects charter + spec + notes digest every turn
│   │   ├── tool_injector.rs  # Merges ATOS tool defs (write_note, record_atos_event, ...)
│   │   └── artifact_surface.rs # Surfaces post-turn artifacts + pending deviation acks
│   ├── openai_types.rs state.rs
│
├── commonwealth-knowledge/   # corpus-engine integration
│   ├── mesh_corpus.rs        # MeshCorpusManager (install/list/remove)
│   ├── shard_manager.rs      # prepare_shards / install_received_shard / consolidate_shards
│   ├── embed_http.rs         # http_embed_fn → /v1/embeddings client
│   ├── grounding.rs          # System-prompt knowledge injection
│   └── store_adapter.rs      # KnowledgeStateStore persistence adapter
│
├── commonwealth-app/         # Mesh application platform
│   ├── manifest.rs           # MeshAppManifest, AppPermissions, RequiredCapabilities
│   ├── lifecycle.rs          # AppProcess, AppStatus state machine
│   ├── registry.rs           # AppRegistry (in-memory)
│   └── proxy.rs              # AppPortMap, HTTP reverse-proxy helpers
│
├── commonwealth-state/       # Distributed KV store
│   ├── store.rs              # MeshStore (SQLite + LWW conflict resolution)
│   ├── backend.rs            # SqliteBackend (WAL mode)
│   └── gc.rs                 # RetentionGc (TTL-based garbage collection)
│
├── commonwealth-daemon/      # CLI entry point + signal handling
└── commonwealth-test-harness/        # SimulatedMesh, SimulatedNode, MockLlamaServer

contrib/
├── install.sh                # Curl installer
├── systemd/commonwealth.service      # systemd unit file
└── launchd/com.commonwealth.daemon.plist  # macOS launchd plist
```

### sovereign-recipes/

```
registry.toml                 # Recipe catalog (schema_version 1, 6 entries)
├── wikipedia/recipe.toml     # Wikipedia English (HF dataset, 6.7M articles)
├── sep/recipe.toml           # Stanford Encyclopedia of Philosophy
├── stackexchange/recipe.toml # Stack Exchange Q&A
├── openalex/recipe.toml      # OpenAlex scholarly metadata
├── gutenberg/recipe.toml     # Project Gutenberg books
└── crs_reports/recipe.toml   # Congressional Research Service reports
```

---

## 3. corpus-engine — The Shared Knowledge Layer

`corpus-engine` is a self-contained Rust library that owns everything between
"raw source data on the internet" and "ranked search hits with provenance."
Both upstream projects use it through the same public API and the same on-disk
index format. Neither knows the other exists.

### 3.1 Pipeline

```
Acquirer  →  Extractor  →  Chunker  →  Embedder  →  Index
                                          (caller-supplied)
```

Each stage is a trait implementation that the engine dispatches to based on a
**Recipe** — a TOML file describing one corpus's pipeline end-to-end.

On top of the ingest pipeline sits the v2 **enrichment atlas** — a typed-graph
layer (seven atom types, seven edge types, seed-threaded map-reduce extraction)
that supports trajectory / relational / event-sequence queries beyond basic
claim+question retrieval. See `corpus-engine/ENRICHMENT_V2.md` for the live
plan of record: status table, landing-by-landing scope, verification targets.

| Stage      | Implementations                                                |
|------------|----------------------------------------------------------------|
| Acquirer   | `bulk_download` (resumable HTTP), `huggingface`, `local_file`  |
| Extractor  | `mediawiki_xml`, `stackexchange_xml`, `jsonl`, `html`, `csv`, `parquet`, `plaintext`, `wikipedia_structured` |
| Chunker    | `paragraph`, `sentence`, `fixed`, `semantic` (heading-aware)   |
| Index      | `CorpusIndex` over LanceDB (IVF-PQ) + Tantivy FTS              |

### 3.2 Storage

- **LanceDB** for vectors (IVF-PQ, memory-mapped from SSD).
- **Tantivy** for keyword full-text search, native to Lance.
- One on-disk directory per corpus, structurally identical whether full or
  shard. `_corpus_meta.json` is the authoritative metadata file.

```
~/.sovereign/indexes/
├── wikipedia/
│   ├── _corpus_meta.json
│   └── chunks.lance/{_versions, data, _indices, _latest.manifest}
└── stackexchange-shard-0-6200000/   # shard — same schema as a full index
```

### 3.3 The injection contract

`corpus-engine` never embeds or generates text itself. Two function types are
injected by the caller:

```rust
pub type EmbedFn = Arc<
    dyn Fn(&str) -> Pin<Box<dyn Future<Output = Result<Vec<f32>>> + Send>>
        + Send + Sync,
>;

pub type InferenceFn = Arc<
    dyn Fn(&str) -> Pin<Box<dyn Future<Output = Result<String>> + Send>>
        + Send + Sync,
>;
```

| Caller        | `EmbedFn`                              | `InferenceFn` (optional, for enrichment) |
|---------------|----------------------------------------|------------------------------------------|
| Sovereign     | Wraps local Embed slot (Qwen3-Embedding via llama.cpp) | Wraps Primary slot (Slow speed)          |
| Commonwealth  | `http_embed_fn()` → POST `/v1/embeddings` | Mesh inference endpoint                  |
| Tests         | Mock returning zero vectors            | Mock returning canned JSON               |

The default expected embedding model is `qwen3-embedding-0.6b` (768 dims).
Indexes record their embedding model in `_corpus_meta.json`; opening an index
with a different model fails with `Error::IncompatibleEmbedding`.

### 3.4 The three-operation sharding contract

Everything Commonwealth needs from `corpus-engine` to distribute knowledge
across nodes fits in three operations:

| Operation                      | Effect                                      |
|--------------------------------|---------------------------------------------|
| `index_stats(corpus_id)`       | Returns total chunks, ID range, size on disk|
| `extract_shard(corpus_id, range, dir)` | Builds a new index containing only chunks in `range` |
| `merge_shards(dirs, dir)`      | Reconstitutes a complete index from N shards |

Because a shard is structurally identical to a complete index,
`CorpusIndex::search()` does not know or care which kind it is operating on.
That Liskov property is the reason the contract stays at three operations.

### 3.5 Enrichment (optional)

`enrichment/` adds an LLM-driven post-indexing pass called the **field model
enrichment system**. Instead of extracting claims from individual chunks, it
analyzes the corpus as a whole in five phases:

1. **Skeleton extraction** — Identifies canonical questions and positions from overview chunks using domain-specific LLM prompts
2. **HDBSCAN clustering** — Clusters chunk embeddings (no inference required), then labels clusters via LLM
3. **Alignment** — Maps clusters to skeleton positions using embedding similarity + LLM verification
4. **Fault line detection** — Identifies substantive disagreements between aligned positions
5. **Open question detection** — Surfaces questions where the corpus has gaps

The `Domain` trait (`enrichment/domain.rs`) is the single extension point for
generalizing across knowledge fields. It defines epistemic vocabulary, overview
document filters, all LLM prompts, and clustering/alignment configuration.
Nine domain implementations exist today: `philosophy` (fully implemented, 423
lines), `multi` (Wikipedia multi-domain), and three domains that power
Sovereign's **KnowledgeView** (see §4.12): `personal` (memory maps),
`conversational` (180-day conversation history), `institutional` (architectural
decisions + invariants from the NoteStore). The remaining four — `science`,
`policy`, `legal`, `community` — are stubs.

`FieldModelEngine` orchestrates all five phases with checkpoint-based
resumability (`checkpoint.rs`). Domain construction goes through
`enrichment/domain_registry.rs` — a data-driven `DomainRegistry` that resolves
domain IDs to `Arc<dyn Domain>`. Adding a domain is a single `register` call,
not a new match arm in `field_engine.rs`.

This pass is opt-in per recipe (`[enrichment] enabled = true, domain =
"philosophy"`). Without an `InferenceFn`, the engine logs a warning and skips
enrichment without failing ingestion.

#### 3.5.1 Enrichment v2 (in iteration)

A replacement pipeline lives alongside v1 at
`corpus-engine/src/enrichment/pipeline/`. It splits the monolithic 5-phase
`FieldModelEngine` into 7 per-phase steps (per-chapter question extraction →
question clustering → canonical concern naming → chunk clustering → grounded
position extraction → pairwise tension detection → gap detection) that an
admin CLI can iterate on one at a time. Prompts are no longer embedded as
Rust string constants — each phase loads a markdown system preamble via
`include_str!` and injects top-K relevant exemplars from a per-phase
`ExemplarBank` JSON file that the developer edits between runs. Dispatch
happens through `PipelineRegistry` (mirrors `DomainRegistry` per
ARCH_PRINCIPLES §4). `LiteraryPipeline` is the first implementation; once
v2 is proven, philosophy / personal / conversational / institutional migrate
and `FieldModelEngine` + the `Domain` trait are deleted (§12 Roadmap).

Supporting infrastructure added in Landing 1:

- `chunkers::sectioned::SectionedChunker` + `SectionDetector` trait —
  section-aware chunking with pluggable detectors (default
  `ChapterRegexDetector` for plaintext books).
- `pipeline::ChapterManifest` — stable per-corpus manifest at
  `~/.sovereign/indexes/<corpus>/chapters.json`.
- `pipeline::PhaseCache` — atomic per-phase JSON cache with mtime-based
  staleness detection across upstream dependencies.
- `pipeline::RunOutputWriter` — monotonic per-run output files under
  `runs/` so the developer can `diff` and `promote` from any prior run.

The CLI admin harness lives in `sovereign-cli/src/enrich_cmd/`.
Landings 2 + 3 ship all seven phases end-to-end: `init` (scaffold + pin
config), `extract` (phase 1 subset/full), `cluster-questions`,
`name-concerns`, `cluster-chunks`, `extract-positions`,
`detect-tensions`, `detect-gaps`, plus `cascade --from <phase>` to
rerun a phase + every downstream dependent. `show <target>` renders
every phase's cached output; `exemplars` reports per-phase bank counts
+ lint; `status` shows fresh/stale/never-run per phase. Pure-vector
HDBSCAN goes through `pipeline::cluster_vectors` (no CorpusIndex
dependency — simpler than v1's wrapper since admin corpora stay in
the low thousands of chunks). Landing 4 adds the validation battery
and dev-UX helpers: `query` (atlas traversal with LOCATE/TRAVERSE/
GROUNDING print), `validate` (runs a `QueryBattery` against the atlas
and prints a score table with pass-rate at a chosen threshold),
`promote` (lifts a run finding into the per-phase exemplar bank by
primary key), `diff` (side-by-side compare of two phase-1 run
outputs: added/removed questions, reveals + carriers changed), and
`reset` (clear caches + runs to re-iterate — default keeps phase 1 +
exemplars; `--full` wipes the whole tree including the chapter
manifest; always prompts unless `--yes`; `--dry-run` previews). The
CLI harness is now complete for the v2-iteration workflow.

### 3.6 Safety

Hardcoded, not configurable from recipes:

- robots.txt compliance on web crawls
- 1-second minimum delay per domain
- User-Agent: `CorpusEngine/0.1 (+https://sovereign.dev/corpus-engine)`
- Crawl scope enforced against seed URL domain
- Download size warnings at 1.5× estimate

### 3.7 Recipes and the recipe registry

Six corpora ship as built-in recipes: Wikipedia, Stanford Encyclopedia of
Philosophy, OpenAlex, Stack Exchange, Project Gutenberg, and CRS Reports.
Recipe TOML files live in the `sovereign-recipes` repository and are consumed
by `corpus-engine` via `RecipeRegistry` (`src/registry.rs`):

- **Bundled snapshot** — `registry_snapshot.toml` is compiled into the crate
  via `include_str!` so the engine works fully offline.
- **Live refresh** — `RecipeRegistry::refresh()` fetches the latest
  `registry.toml` from GitHub. Each entry has a `toml_url` field pointing to
  the raw recipe on GitHub.
- **Resolution order** — local override on disk → fetch from `toml_url` → error.
- **SHA-256 verification** — when the registry entry's `sha256` field is
  non-empty, the fetched recipe is verified before use.
- **`cargo xtask update-registry-snapshot`** refreshes the bundled snapshot
  from the live registry.

User recipes can be dropped into the local recipes directory and discovered
via `engine.discover_recipes()`.

### 3.8 Delta updates

`update/delta.rs` supports incremental index updates via version manifests:

- `VersionManifest` tracks per-document revision IDs.
- `ManifestDiff` computes additions, updates, and deletions between two manifests.
- Updates apply in three phases: deletions → updates (delete-then-re-add) → additions.
- Resumability via `_update_progress.json` so interrupted updates can continue.

---

## 4. Sovereign — The Local Agent

A single-machine local AI assistant. Runs as a desktop app, CLI, or HTTP server
against the same `Runtime`. No data leaves the machine unless the user opts in
to web search or to a Commonwealth mesh.

### 4.1 Trait architecture

`sovereign-core/src/traits.rs` defines the async trait boundaries the entire
runtime is built against. Every component is swappable for tests or alternate
implementations.

| Trait                  | Surface                                                        |
|------------------------|----------------------------------------------------------------|
| `InferenceProvider`    | `complete`, `complete_stream`, `complete_stream_with_id`, `embed`, `embed_query`, `capabilities` |
| `Router`               | `classify(message, ctx, tools) → RouterClassification` — primary intent + confidence + (PR2) alternatives + rationale + legacy coarse/self-assessment diagnostics |
| `Planner`              | `plan(goal, context, tools) → Plan`, `replan(...)`             |
| `Tool`                 | `descriptor`, `execute`, `validate`, `retry_config`, `required_permissions` |
| `LandscapeDigestProvider` | `splice_landscape_digests(ctx, active_skill)` — KnowledgeView hook (§4.12) |
| `ApprovalChannel`      | human-in-the-loop tool approval; impls: `CliApprovalChannel`, `TauriApprovalChannel`, `ServerApprovalChannel`, `AutoApprovalChannel` (tests) |
| `MeshKnowledgeSource`  | fan-out knowledge search to mesh peers                         |
| `InsightStore` / `InsightSink` | long-term insight extraction + persistence                |

`StateStore` itself is decomposed per ISP into focused sub-traits that a
single blanket impl re-aggregates: `ConversationStore`, `TaskStore`,
`MemoryStore`, `RoutingStore`, `DocumentStore`, `CorpusStateStore`,
`BudgetStore`, `PermissionStore`, `HealthStore`, `DocumentSessionStore`,
`DocumentAssetStore`. Callers can narrow bounds to the exact capability they
need; implementors still get a single aggregate trait to plug in.

### 4.2 Runtime data flow

```
User message
  → Router.classify                    (Fast slot, two-pass coarse-then-refine)
       → RouterClassification { primary: {intent, confidence}, alternatives, rationale, ... }
  → decide_policy(classification, ConfidenceThresholds)   (pure fn, sovereign-core/types.rs)
       → RoutingPolicy { tier: High|Moderate|Low, move_kind: Commit|Propose|Ask, thresholds_used }
  → SessionStore.begin(...)            (in-memory scratch; ARCH §4.5 — not StateStore-worthy)
       → QuerySession { id, conv, skill, input, classification, policy, cancel_token, ... }
       → sweep_expired()               (drops sessions > 30s old)
  → Dispatch by Intent (PR1: MoveKind::Commit only; Propose/Ask land in PR2):
       → SimpleQuery / DeepQuery / KnowledgeQuery → search → synthesize
       → ComplexTask  → Planner.plan (Primary slot)
                      → Executor (topological batches)
                          ├─ ReasonWithTools loop (iterative tool use)
                          ├─ Best-of-N sampling (LlmJudge / Random / Best)
                          ├─ Evaluation passes (eval prompt → retry on fail)
                          └─ Tool steps with permission checks + approval
                      → synthesize from step outputs
  → Provenance recorded into Message.metadata (includes routing policy tier)
  → Memory extraction on conversation end
```

**Antifragile-routing split (PR1 foundation, PR2 UX).** The router emits
*facts* (`RouterClassification`); the runtime applies *policy* (`decide_policy`).
The split keeps classification pure and testable without a model, and lets
future threshold calibration mutate policy without touching the `Router` trait.
Each turn registers a `QuerySession` in an in-memory `Arc<DashMap>` carrying the
classification, the policy decision, and a `tokio_util::sync::CancellationToken`
that PR2's `redirect_turn` command will fire to cancel an in-flight sampler.
PR1 only reaches `MoveKind::Commit` in the dispatcher — Propose (interpretation
banner + cheap redirect) and Ask (clarification card) land in PR2 once the
desktop XState machines and Tauri events are wired.

`Plan` is a flat JSON DAG: `steps` (each with `kind`, `inputs`, optional
`sampling`, optional `evaluation`) plus `edges` for the dependency graph.
`StepKind` variants: `Reason`, `Tool`, `UserInput`, `Branch`, `ReasonWithTools`.

The planner emits `[sample:N:method]` and `[eval:name]` annotations directly in
its skill template DSL; the executor parses them into `SamplingConfig` and
`EvaluationConfig`.

### 4.3 Inference

`sovereign-inference/embedded.rs` wraps `llama-cpp-2` with a dual-slot loader:

| Slot       | Purpose                          | Typical model              |
|------------|----------------------------------|----------------------------|
| Fast       | Routing, working-memory compression, query reformulation | Qwen3-0.6B–1.7B |
| Primary    | Planning, synthesis, evaluation  | Qwen3.5-4B/9B/27B          |
| Embed      | Vector embeddings                | Qwen3-Embedding-0.6B/4B    |

`models.toml` is the source of truth: five hardware profiles
(`cpu_only`, `low_mem`, `default`, `high`, `very_high`) each declare three
slots with `repo`, `file`, `family`, `quant`, `size_gb`, `thinking`. Per-slot
`quirks_override` lets profiles tune family defaults without redeclaring them.

`model_family.rs` encodes per-family quirks (`Qwen3`, `Qwen35`, `Qwen3Embedding`,
`Gemma3`, `Llama3`, `Phi4`, `Phi4Reasoning`, `SmolLM3`, `Unknown`):

- `ThinkingControl` — `SystemPromptToken { enable, disable }`, `AlwaysOn`, `None`
- Sampling defaults — temperature, top-k, top-p, presence penalty
- `EmbedQuirks` — `PoolingStrategy`, `NormalizationStrategy`, `query_instruction` prefix (asymmetric retrieval)

`hybrid.rs` is a multi-backend `InferenceProvider`:

- `BackendEntry { name, health_tracker, priority, cost_per_token, is_local, oicp_manifest }`
- `BackendSelector` trait → `CapabilityAwareSelector` (OICP scoring) →
  fallback `PrioritySelector` (highest-priority healthy backend)
- Per-backend `HealthTracker`: EWMA latency (α=0.3), 3-strike availability mark
- Background health loop refreshes OICP manifests for remote backends
- Up to 2 retries on failure with the next-best backend

`remote.rs` is an OpenAI-compatible client that speaks to anything wearing the
schema (vLLM, Ollama, llama.cpp server, TGI, **Commonwealth**).

### 4.4 Tools

| Tool                 | What it does                                                   |
|----------------------|---------------------------------------------------------------|
| `SearchTool`         | Embeds query, vector + FTS5 over local store, scores against thresholds (`SUFFICIENT=0.85`, `LOW=0.3`), falls back to web if configured |
| `WebSearchTool`      | NL → keywords (Fast slot), search backend, fetch top 3, extract, synthesize with citations |
| `WebFetchTool`       | Single-URL fetch + HTML→text                                  |
| `KnowledgeTool`      | Direct corpus query against installed `corpus-engine` indexes |
| `ClaimSearchTool`    | Search enriched corpora by epistemic status                   |
| `EpistemicLandscapeTool` | Map positions and disagreements on a topic               |
| `SymbolLookupTool`   | Exact symbol-name lookup against tree-sitter-indexed code (LanceDB filter pushdown) |
| `CodeSearchTool`     | Semantic code search (vector + FTS fallback); results always labelled approximate |
| `RecentChangesTool`  | Symbols modified within last N hours, grouped by file (mtime) |
| `FindCalleesTool`    | What does this function call? SCIP call graph, compiler-resolved. Staleness-aware |
| `FindCallersTool`    | What calls this function? Depth 1–2 traversal. Staleness-aware |
| `BlastRadiusTool`    | BFS over call graph to all transitive callers; appends `macro_hints` text scan for macro-invoked symbols not in SCIP |
| `LintStatusTool`     | Current status of the background `cargo check` watcher (`fresh_passing`, `fresh_failing`, `stale`, `running`, `never_run`) |
| `GetLintOutputTool`  | Full lint output when `LintStatus.output_truncated = true` |
| `TestStatusTool`     | Current status of the background `cargo test` watcher |
| `RunTestsTool`       | Trigger a test run (returns immediately; poll `test_status`) |
| `GetRunOutputTool`   | Full test output when `TestStatus.output_truncated = true` |
| `WriteNoteTool`      | Persist a working note (decision, attempt, invariant, todo) to `NoteStore` |
| `ReadNotesTool`      | FTS + recency search over active notes; hides retired reflections by default |
| `DeleteNoteTool`     | Remove a note by ID |
| `ProjectContextTool` | Search `SOVEREIGN.md` and project documentation |
| `SessionReflectionTool` | Record post-task feedback keyed by tool name; surfaced by `sovereign reflect` |
| `CheckDocPathsTool`  | Scan a markdown file for backtick path references and verify each exists on disk |
| `DocumentTool`       | Map-reduce summarize/analyze (4 chunks per batch, 8K reduce window) |
| `ShellTool`          | `sh -c` with 30s timeout, `Shell` permission + per-call approval |
| `FileTool`           | Read/write/list, sandboxed to data dir                        |
| `EmailTool`          | SMTP via `lettre` (feature-gated)                            |
| `CalendarTool`       | Read/create events                                            |
| `ComputeTool`        | Cost estimation for mesh contribution accounting              |
| `McpClient` + `McpToolAdapter` | stdio JSON-RPC client wraps remote MCP servers as native tools |

**Code Intelligence** is a 24-tool MCP server started via `sovereign project serve`
(for ad-hoc use) and `sovereign daemon` (for the long-running service owned by
launchd/systemd). Tools fall into eight groups:

| Group | Tools |
|-------|-------|
| Code index (LanceDB)    | `symbol_lookup`, `code_search`, `recent_changes` |
| SCIP call graph         | `find_callers`, `find_callees`, `blast_radius` |
| Lint watcher            | `lint_status`, `get_lint_output` |
| Test watcher            | `test_status`, `run_tests`, `get_run_output` |
| Working notes           | `write_note`, `read_notes`, `delete_note` |
| ATOS feature lifecycle  | `provision_feature`, `archive_feature`, `read_note_by_id`, `promote_note`, `read_note_digest`, `record_atos_event`, `write_redteam_finding` |
| Project context         | `project_context` |
| Session reflection      | `session_reflection` |
| Doc health              | `check_doc_paths` |

The ATOS group powers the feature lifecycle (see §4.13). `provision_feature` and
`archive_feature` write to `FeatureStore`; `read_note_by_id`, `promote_note`, and
`read_note_digest` extend the NoteStore surface with feature-scoped operations;
`record_atos_event` writes to `atos_tool_events`; `write_redteam_finding` persists
a `redteam_finding`-kind note.

The first three groups query either LanceDB indexes (built by tree-sitter via
`sovereign code index <path>`) or a SQLite `ScipGraph` populated by SCIP exports
from language-specific analyzers (`rust-analyzer`, `scip-typescript`, etc.).

`blast_radius` performs BFS over the call graph up to `max_depth` and appends a
supplementary text scan (`macro_hints`) for symbol references inside macro
invocations and attributes that SCIP does not capture.

`lint_status` / `test_status` expose the output of background `cargo check` /
`cargo test` watchers so agents can check build state without contending for the
Cargo file lock.

`write_note` / `read_notes` / `delete_note` persist structured working notes
(decisions, attempts, invariants, todos) across sessions in `NoteStore` (SQLite,
FTS5). `project_context` searches `SOVEREIGN.md` and project docs.

`session_reflection` records structured post-task feedback keyed by tool name.
The developer reads accumulated reflections via `sovereign reflect` and retires
them once the underlying issue is fixed. Every 10 tool calls in a session the
server appends a brief reminder to the tool response nudging the agent to call
`session_reflection`.

A filesystem watcher (`CodeWatcher`) re-indexes modified files and marks them
stale in the call graph so query results carry calibrated confidence:

| Staleness level | Trigger | User sees |
|-----------------|---------|-----------|
| None            | Graph < 1 hour old, no modified files | Nothing — no noise for fresh results |
| SomeCallSitesMayBeStale | File modified since last SCIP export | Quiet note naming the specific files |
| GraphIsAging    | 1–24 hours since export | Quiet note with age |
| GraphIsStale    | > 24 hours since export | Prominent warning with `sovereign corpus scip` refresh command |
| LanguageNotIndexed | SCIP exporter not installed for this language | Note with install hint |

The `codebase-navigator` skill configures planner templates for call graph
tracing and security review workflows.

`SearchTool` is the unified pipeline: local FTS5 + vector → coverage assessment
→ optional web fallback → cited synthesis. Web backends: DuckDuckGo (free
HTML), Brave (API), Tavily (API). Budget tracking gates web usage so the
system stays usable offline.

### 4.5 State

`sovereign-store` provides three `StateStore` implementations against the
same trait: `SqliteStateStore` (default, `rusqlite` bundled), `PostgresStateStore`
(deadpool + tokio-postgres), `MemoryStateStore` (tests).

Schema (defined in `migrations.rs`):

- `conversations`, `messages` (+ FTS5 on content)
- `tasks` (plan JSON, status, completed_steps)
- `memories` (content, source, confidence, embedding BLOB, FTS5)
- `documents` (chunk_index, embedding BLOB, source_type, FTS5)
- `corpus_states` (corpus_id, tier, installed_at, indexed_at, sync_ready)
- `routing_log` (message_hash, classified_as, was_correct, latency_ms)
- `search_budget` (per backend, queries_used, reset_at)
- `permissions` (tool_id, scope, granted)

Every record carries a Lamport `version` and soft-deletable rows have
`deleted_at`. Reads filter `WHERE deleted_at IS NULL`. This is sync-ready: two
StateStores can merge by union + timestamp resolution without schema migration.

### 4.6 Memory

- **Working memory** — compressed every message via `memory::compress_working_memory` (Fast slot, max 200 tokens) into `{ current_goal, facts, active_documents }`.
- **Long-term memory** — extracted at conversation end. Each `Memory` has `confidence` (0..1), `created_at`, `last_used`. Retrieved by FTS5 keyword search on content.
- **Decay** — exponential monthly decay (default 10%, overridable per skill via `confidence_decay_per_month`). Pruned below `prune_threshold`.
- **Routing-correction memory** — `RoutingCorrection { message_hash, classified_as, was_correct }` is fed into the router's Pass-1 prompt as "avoid these mistakes."

### 4.7 Skills

Skills are TOML files. `SkillRegistry` loads them from `skills/` and merges
their routing hints, planner templates, prompt overrides, memory rules, and
OICP requirements into the runtime. Bundled:

| Skill              | Highlights                                                 |
|--------------------|-----------------------------------------------------------|
| `research-analyst` | Multi-source research with citations, Slow synthesis, 5%/month decay |
| `epistemic-research` | Debate mapping; uses `ClaimSearchTool` + `EpistemicLandscapeTool`; requires `analysis≥2` |
| `codebase-navigator` | Call graph tracing, security review workflows; wires all 16 code intelligence tools |
| `code-review`      | Structured code analysis; `privacy = local_only`         |
| `collaborative-research` | Multi-turn research with shared context |
| `document-analyst` | Long-document analysis via map-reduce `DocumentTool` |
| `personal-assistant`| General-purpose                                          |
| `inner-work`       | Reflective/Socratic; `privacy = local_only` enforced regardless of available remote backends |

Skill TOML structure:

```toml
[skill]                       id, name, version, description
[routing]                     trigger_phrases, default_intent, min_confidence
[[planner.templates]]         name, trigger, steps (with [sample:N:method] / [eval:name])
[tools]                       required, optional, tool_settings
[prompts]                     synthesis override
[memory]                      extract_prompt_addendum, confidence_decay_per_month, prune_threshold
[inference]                   privacy, min_context_tokens, [inference.preferred_capabilities]
```

Skills carry `signature` and `signed_by` fields and a derived
`TrustLevel { CommunityReviewed, AuthorSigned, Unsigned }` so the UI can
distinguish them.

### 4.8 OICP

Sovereign's `oicp.rs` is the canonical OICP v0.3.0 implementation per
`commonwealth/docs/oicp-v0.3.md`. All types live in the workspace-root
`oicp-types` crate and are re-exported by `commonwealth-core::oicp` and
`sovereign-core::oicp`.

**Wire types (v0.3):**

- `CapabilityHint` — validated string tag. Standardized constants: `general` and `code`. Open-vocabulary specializations use the `x:<tag>` extension form. Parse is permissive: any non-empty whitespace-free string is accepted so future-standardized hints don't break older clients.
- `LatencyClass` — `Fast, Normal` (default), `Extended`.
- `CapabilityClaim { hint, latency_class, max_context, max_output, affinity }` — one claim per kind-of-work a node serves well. The unit of scheduling: a node with N claims contributes N candidate matches.
- `InferenceRequirements { oicp_version, capability_hint, latency_class, context_tokens, max_output_tokens, privacy, request_id }` — `effective_hint()` defaults to `general`; `effective_latency_class()` defaults to `Normal`.
- `ProviderManifest` — what a backend advertises, with `knowledge` + `federation` sections.
- `ProviderModel { id, base_model, quantization, context_tokens, status, size_gb, claims }` — each model publishes its claims directly; no separate capability profile on the wire.
- `ShardingPrivacy` — `LocalOnly` (default), `MeshAllowed`.
- `OicpResponseMeta { quantization, match_quality, request_id }` — response metadata.
- `KnowledgeSearchRequest`/`KnowledgeSearchResponse`/`KnowledgeResult` — knowledge search API.

**Internal model-metadata vocabulary (not on the wire):**

- `Capability`, `CapabilityProfile`, `ProficiencyLevel`, `proficiency()` — shared vocabulary the runtime uses to describe what a model is good at (sourced from `models.toml`, skill TOML, internal registries). Translated to a `CapabilityHint` at advertisement time via `infer_hint_from_profile` (strict: requires `Code == Exceptional` and `Code > General` for the `code` hint).

**Scheduler (three implementations, one shared scoring pipeline):**

All three schedulers — `commonwealth-inference::scheduler::oicp_select`,
`sovereign-inference::selector::CapabilityAwareSelector`,
`sovereign-mesh::oicp_select` — call `score_claim_for_request(&CapabilityClaim, &InferenceRequirements)` for the protocol-level score, then fold in the v0.3 §7 operational adjustments.

Protocol-level (shared):

- Hint match: exact = `1.0`; specific request + general claim = `0.5` fallback; `general` request + specific claim = `0.0` (specialization obligation is on the advertiser).
- Context / output: **hard gate**; a claim that can't fit is eliminated.
- Latency class: `1.0` exact / `0.8` adjacent / `0.5` two-class gap.
- Affinity: clamped to `[0.0, 1.0]`, final multiplier.

Operational (per-scheduler, shared helpers in `oicp-types`):

- **Observed health** — `effective_affinity(claimed, &NodeObservations)` blends the self-reported affinity with the scheduler's own rolling failure rate. Trusts the claim at zero samples, fully applies observation past `CONFIDENCE_SAMPLES` (50).
- **Load penalty** — hyperbolic taper `1 / (1 + 0.05 * in_flight)`. 10 in-flight ≈ 0.67×, 20 ≈ 0.50×. Protects popular specialists from thundering-herd collapse.
- **Locality bonus** — `Local` 1.15× / `Near` 1.05× / `Far` 1.0×. Lets a local 0.70 node out-rank a remote 0.80 node per spec §6 example. Peer locality is measured from the manifest-fetch RTT: `<5ms` → `Local`, `<25ms` → `Near`, else `Far` (`classify_rtt_ms` in `sovereign-mesh::oicp_select`). Piggy-backing on the manifest fetch means real LAN deployments get their bonus without a separate probe round-trip.
- **Cold-start ramp** — new nodes start at `0.7×` weight and ramp to `1.0×` over `COLD_START_SAMPLES` (20) observations. "Modest fraction, not full load," not "never route to new peers."
- **Inference availability** — gossiped `NodeCapabilities.inference_availability` (clamped `[0.2, 1.0]`), captured before per-turn observations converge.

Observations are **local to each scheduler** per spec §7 — never advertised between nodes. Commonwealth's `BackendCandidate` carries `Option<&NodeObservations>` + `NodeLocality`; Sovereign's `BackendEntry` holds them as `Arc<RwLock<NodeObservations>>` so the surrounding provider wrapper can record outcomes as requests complete. Sovereign-mesh's `MeshInferenceProvider` exposes `record_dispatch` / `record_success` / `record_failure` for the streaming path to call.

**Advertisers emit claims from loaded slots:**

- `commonwealth-api/routes_oicp.rs::synthesize_default_claim` — one claim per `ModelInfo`. Code hint by name heuristic (`coder|code-llama|deepseek-coder`); affinity derived from the stored v0.2 proficiency profile.
- `sovereign-mesh/inference_adapter.rs::synthesize_slot_claims` — one claim per Speed slot. Fast slot → `LatencyClass::Fast` + reduced context envelope (8K/1K); Slow slot → `LatencyClass::Normal` + full envelope (32K/4K).
- `sovereign-mesh/inference_adapter.rs::synthesize_code_slot_claims` — one `code`-hinted claim for the optional Code specialist (PR-E2). Always `LatencyClass::Normal` (reflects hot-swap TTFT, not held-warm TTFT — the code slot shares the primary's lazy chat mutex). Affinity floors at 0.5 for filename-signalled coders so BYOM code GGUFs not in `models.toml` remain discoverable.

**Embed slot — a cross-peer interoperability contract:**

Unlike the Quick / Main responder slots (which are per-peer
performance choices — each node picks the best model its hardware
can run), the **Knowledge embedder** is architecturally distinct:
every node participating in a shared corpus MUST produce
bit-compatible vectors. `EmbedModelInfo { model_id, dimensions,
pooling, normalization }` captures the identity; two nodes are
eligible collaborators iff their `EmbedModelInfo` values are equal.
Cosine similarity across different embedding spaces is meaningless,
so a mismatch silently corrupts retrieval — the collaborative-
ingestion planner filters peers on this field up-front and logs
the rejection rather than letting partitions land in broken state.

Implications for any future refactor: the embed slot is **not**
one role of many. It remains explicitly named, explicitly probed
at daemon startup (`Arc<EmbedSlot>` in `EmbeddedLlamaCpp`), and
explicitly advertised via `KnowledgeManifest.embed_model`. Any
generalisation that folds embed into a generic role vector must
preserve the "one active embed model per node; must match
collaborators" invariant. The family-driven pooling + normalisation
defaults come from the bundled manifest via
`ModelsManifest::embed_family_for_file` — adding a new embed model
requires declaring its `family = "..."` line so both the desktop
startup path and the CLI daemon advertise the same
`EmbedModelInfo`.

**PR-E2 honours this split in its design**: the Code specialist
shares the primary's lazy chat mutex (hot-swap model), while the
embed slot stays on its own `Arc<EmbedSlot>` with zero shared
state. The `code` claim in `build_self_manifest` advertises
`loaded: false` to prevent schedulers from double-counting the
two chat roles against available compute — at most one of
{Main responder, Code specialist} is resident at a time.

**Extension governance (v0.3 §4.3):**

`sovereign-mesh::MeshInferenceProvider` carries an
`ExtensionRegistry` (from `oicp-types`) that passively records every
`x:*` hint it sees — either on an outgoing request (consumer
demand) or in a peer's advertised claim (provider adoption). The
registry is **not** consulted by the scheduler: it's a governance
input for the separate promotion process that decides when an
extension has demonstrated durable-enough use to merit
standardization (spec §4.3). Each entry tracks `requests_seen`,
`advertisements_seen`, `first_seen_unix`, `last_seen_unix` so a
governance review can distinguish a one-off typo from a
multi-mesh multi-month signal. Standardized hints (`general`,
`code`) and unknown-bare strings are ignored — neither is a
governance-track signal. Read the snapshot via
`MeshInferenceProvider::extension_stats().await`.

**Runtime request builder:**

`runtime::default_oicp_for_intent` attaches `capability_hint` + `latency_class` to every OICP-bearing turn — DeepQuery → Extended, ComplexTask + KnowledgeQuery → Normal, SimpleQuery / Continuation / SimpleAction stay local (no envelope). `build_oicp` merges skill declarations with intent defaults (skill-declared wins; intent fills in gaps). `sovereign-mesh::oicp_select::pick_slot_for_oicp` honours the v0.3 latency class by picking the matching Speed slot directly, falling back to the other slot when the primary cannot serve the requested hint.

**Skills declare in `[inference]`:**

```toml
[inference]
privacy            = "local_only" | "mesh_allowed"
capability_hint    = "general" | "code" | "x:<extension>"
latency_class      = "fast" | "normal" | "extended"
min_context_tokens = 8192
max_output_tokens  = 2000
```

The spec default for `privacy.sharding` is `LocalOnly`, so any request
that reaches a mesh provider without an explicit `mesh_allowed` opt-in
is rejected with HTTP 400 by Commonwealth.

### 4.9 Frontends

| Frontend     | Purpose                                                       |
|--------------|--------------------------------------------------------------|
| `sovereign-cli`     | Interactive REPL (default) plus named subcommands: `setup` (first-run wizard), `project` (per-project code intelligence — `init`/`design`/`plan`/`charter`/`found`/`amend [design|charter]`/`phase`/`audit`/`serve`/`refresh`/`install-hooks`), `atos` (feature-layer orchestration — `provision`/`start-milestone`/`end-milestone`/`spec diff`/`spec accept`/`teardown`/`doctor`/`install-plugin`), `daemon` (long-running service started by launchd/systemd; owns :9741), `doctor` (diagnose setup + daemon health), `mesh` (create/join/rotate/status/members/leave), `corpus` (install/remove/update/list), `code` (index), `mcp` (proxy + `mcp list-tools`), `recipe` (run), `reflect` (review session reflections, retire fixed ones). Flags: `--model`, `--primary-model`, `--data-dir`, `--skills-dir`, `--router`, `--ingest`, `--brave-api-key`, `--tavily-api-key`, `--no-knowledge-view` (disable KnowledgeView landscape digests; see §4.12). `project init` prompts interactively for AI assistant harness selection (Claude Code / opencode / all / skip) and writes `.opencode/config.json` + `AGENTS.md` for opencode; if `.sovereign/sovereign.toml` has a `[commonwealth]` section it also configures a Commonwealth OICP inference provider in the opencode config. `project init` also installs the ATOS opencode plugin at `.opencode/plugins/sovereign-atos.ts` (see §4.13). |
| `sovereign-server`  | Axum REST + WebSocket on configurable port. Multi-tenant via `tenant.rs`. SSE streaming via `/v1/conversations/{id}/messages/stream`. WS streaming via `/v1/ws/{conversation_id}`. Server-side `ApprovalChannel` stores requests in DB and exposes `/v1/tasks/{id}/approve`. |
| `sovereign-desktop` | Tauri 2 + Svelte 5. Setup wizard (persona, hardware-driven model selection, knowledge tier, optional web search keys). Chat with streaming + source attribution. Knowledge base management (`KnowledgeStatus`, `CorpusProgressBanner`). Skill manager. Mesh status/settings UI. Deep-link handler for `sovereign://` URLs. System tray. |

### 4.10 Deep links

`sovereign-mesh/deep_link.rs` parses `sovereign://create?name=<name>` and
`sovereign://join?key=<key>` URLs, including relay hints for NAT traversal.
The desktop app registers as the system handler for the scheme so a join key
can be sent as a clickable link.

### 4.11 Knowledge integration

Sovereign hands `corpus-engine` an `EmbedFn` that wraps its local Embed slot,
and (optionally) an `InferenceFn` wrapping the Primary slot in Fast mode with a
768-token budget for claim/relationship extraction. `data/corpora.toml` is the
manifest the desktop app uses for tier-driven install:

| Tier      | Size  | Corpora                                       |
|-----------|-------|-----------------------------------------------|
| Essential | 55 GB | Wikipedia                                     |
| Research  | 105 GB| Wikipedia + SEP + OpenAlex + CRS              |
| Technical | 95 GB | Wikipedia + Stack Exchange                    |
| Full      | 170 GB| All six                                       |

### 4.12 KnowledgeView — Landscape digest assembly

Sovereign ships with an opt-in "map of the user's terrain" system that splices
short, structured summaries of the user's own world into the system prompt
before each turn. It composes three views, each sourced from an existing
`StateStore` surface and enriched via the `corpus-engine` field-model pipeline:

| View                   | Source                                   | Enrichment domain |
|------------------------|------------------------------------------|-------------------|
| `personal-knowledge`   | `memories` (confidence > 0.2, not deleted) | `personal`      |
| `conversation-history` | `conversations + messages` (180-day window, `privacy = "local_only"` skills excluded) | `conversational` |
| `institutional-notes`  | `notes` (decisions, invariants, todos, uncertainties, redteam findings — not reflections) | `institutional`  |

Each view runs the usual corpus-engine pipeline (`SqliteAcquirer` → JSONL →
ingest → enrichment) and writes a `field_skeleton.json` to
`~/.sovereign/indexes/<view>/`. Before each message, the Runtime calls
`LandscapeDigestProvider::splice_landscape_digests` (single method,
`sovereign-core/src/traits.rs`). The only implementor in production is
`sovereign-tools::knowledge_view::KnowledgeViewManager`, which:

1. Formats each view's skeleton into a short markdown block bounded by a
   per-view token budget (300 / 200 / 100 tokens by default).
2. Computes **cross-view resonance** — embeds every canonical question and
   open question across views, keeps matches above cosine similarity 0.75
   (≤5 per digest), and phrases them tentatively: *"theme X (personal) may
   resonate with theme Y (conversations)"*, never asserting identity.
3. Splices the resulting `Vec<LandscapeDigest>` into `ConversationContext`,
   which `build_system_message` then concatenates into the final system
   prompt.

**Structural privacy invariants** (enforced in code, not policy):

- All three recipes hardcode `scope = "local"`, `mesh_sharing = false`,
  `query_sharing = false` — these fields are not parameterized and cannot be
  flipped by user configuration.
- The conversational view's acquirer builds a SQL `WHERE skill_id NOT IN
  (<local_only_ids>)` clause **at ingest time**, so messages tagged with a
  `privacy = "local_only"` skill never enter the shared conversational map.
- When the **active** skill is `local_only`, the splice call suppresses the
  conversational + institutional + cross-view digests entirely, leaving only
  the personal map.

**Configuration.** KnowledgeView is on by default. Three disable paths:
`--no-knowledge-view` (CLI), `[knowledge_view] enabled = false`
(`sovereign-server.toml`), and the desktop Settings toggle. When disabled, the
manager is never instantiated and the splice is a no-op — no digest ever
reaches the prompt, no background enrichment runs.

### 4.13 ATOS — Agent Task Orchestration System

ATOS is Sovereign's scaffolding for driving coding agents against a specified
contract. It operates at two orthogonal layers that share a single artifact
directory, `.sovereign/`:

**Project layer** — a single charter governs the whole repo:

| Command                         | Effect                                                         |
|---------------------------------|----------------------------------------------------------------|
| `sovereign project init`        | Observe repo (languages, deps, git); auto-with-confirm `git init` when absent (skippable with `--no-git`); write `project.toml`; install ATOS opencode plugin at `.opencode/plugins/sovereign-atos.ts`. Soft-paths (not hard-bails) empty repos when `DESIGN.md` is present |
| `sovereign project design`      | Agent-collaborative DESIGN.md session against the Commonwealth daemon. opencode is the blessed path (`--via opencode`, default); `--solo` drives CLI prompts from the `DesignSignals` structural parser and writes `OPEN_QUESTIONS.md`; `--stopgap` is a provisional in-terminal chat placeholder. `--import <path>` copies an existing doc into `<repo>/DESIGN.md` with diff-confirm. Session artifacts land in `.sovereign/.atos/design/<id>/` (`brief.md`, `state.json`, `transcript.jsonl`) |
| `sovereign project plan`        | Compose `IMPLEMENTATION_PLAN.md` at repo root from `DESIGN.md` + `OPEN_QUESTIONS.md`. Phase 0 is language-specific skeleton; phases 1..N derive from H2 sections (skipping Anchors / Open questions) in document order. Unanswered OQs block the plan unless `--allow-open`; answered OQs surface as `Resolved (for the record)` on the matching phase. Plan items upserted into `.sovereign/plan.db` (`plan_items` table); stale rows from prior DESIGN.md generations are marked `deferred` rather than deleted |
| `sovereign project charter`     | Create or edit the free-form team `CHARTER.md` (governance, culture, onboarding). Low-ceremony — distinct from `DESIGN.md`. First invocation writes a minimal skeleton at `.sovereign/CHARTER.md` and opens `$EDITOR`; subsequent invocations open the existing file. Re-hashes and indexes post-save |
| `sovereign project found`       | Four-stage founding conversation → `CHARTER.md` + `PHASES.md`, records answers as `decision` notes, sets `charter_hash = SHA-256(CHARTER.md)`. As of the ATOS onboarding redesign, Stage-1/Stage-2 predicates are signal-gated — questions fire only when the observation, prior answers, OR `DesignSignals` extracted from `DESIGN.md` indicate the question is material (e.g., `fault.time-representation` fires only when the design mentions time). `--orchestrate` switches to the orchestrator path: require `DESIGN.md` + answered `OPEN_QUESTIONS.md` + `IMPLEMENTATION_PLAN.md` + `CHARTER.md` at repo root (all produced by `project design` / `project plan` / `project charter`); skip the questionnaire; elicit only the Phase-1 stop condition; compose `PHASES.md` via the existing `compose_phases`; flip the lifecycle |
| `sovereign project amend [target]` | `amend charter` (default, for back-compat) opens `CHARTER.md` in `$EDITOR`, diffs section-by-section, runs adversarial Q&A, writes new hash + amendment-log entry. `amend design` opens `DESIGN.md` at repo root, detects edits to the curated sections (`Anchors`, `Data & interfaces`, `Open questions`), asks one adversarial question per changed curated section, appends the Q&A to `DESIGN.md`'s `## Amendment log` (newest on top). `amend design` does NOT bump `charter_version` — DESIGN.md is expected to iterate; provenance is the inline log + git history |
| `sovereign project phase pass N` | Runs phase N's stop condition from `PHASES.md`; on green, writes `phase-N.md` and bumps `current_phase` |
| `sovereign project audit`       | One-page reviewer rollup (founding state, phases passed, notes by kind, open questions, deviations, drift status) |

**Feature layer** — one charter per feature, nested under the project charter
when present:

| Command                                   | Effect                                                       |
|-------------------------------------------|--------------------------------------------------------------|
| `sovereign atos provision <id> --charter <path>` | Parse charter, seed `FeatureRow` + `MilestoneRow`s, detect `**Red team:** auto` opt-in |
| `sovereign atos start-milestone <id> --brief <path>` | Export `ATOS_RUN_ID`/`ATOS_FEATURE_ID`/`ATOS_MODE` and hand control to the driver |
| `sovereign atos end-milestone <id>`       | Run `stop_condition` (shell command), capture output, record verdict, write `milestone-N.md` |
| `sovereign atos spec diff <id>`           | Line-level diff of `spec.md` vs. approved hash |
| `sovereign atos spec accept <id> --reason` | Re-baseline to current content, log `deviation` note |
| `sovereign atos teardown <id>`            | Wrap feature; render `epistemic-report.md`; promote feature-scope notes that generalize into global scope |
| `sovereign atos doctor` / `install-plugin` | Compare installed plugin version to CLI binary; reinstall if stale |

**Crate layout.** `sovereign-atos` (library) defines the `AtosOrchestrator`
trait and the `LocalAtosOrchestrator` impl. Storage lives in two SQLite tables
that share a connection with the existing NoteStore:

- `FeatureStore` — `features`, `feature_milestones`, `atos_runs`, `atos_tool_events`
- `NoteStore` — extended with kinds `deviation`, `redteam_finding`,
  `postmortem_pointer` and a `NoteScope` dimension (`Global | Feature | Session`)

**Drift detection.** Every agent turn recomputes SHA-256 of the feature
`spec.md` and compares against the recorded approval hash. Mismatches **warn,
not block** — the next turn's preamble carries a drift note pointing at the
deviation; the agent either reverts or calls `atos spec accept --reason`.
Approval is sourced from either git history (walking commits that touch the
spec file) or a Commonwealth `atos-approvals` MeshStore app, so force-push
doesn't silently erase it.

**The `commonwealth/sovereign-coder` pipeline.** Defined in
`commonwealth-core/src/default_pipelines.toml` and resolved via
`PipelineAliasTable` on `/v1/chat/completions`. Its middleware chain runs in
order: `approval_gate → session_briefing → context_injector → tool_injector →
artifact_surface`. The `context_injector` prepends a fixed
`<atos-instructions>` preamble, the project charter frame (invariants +
current phase), a scoped notes digest (Global + Feature), and the spec body.
A paired `sovereign-red-team` pipeline uses `read_only_enforcer +
context_injector (invariants-only)` and is auto-spawned after the final
milestone passes when the charter carries `**Red team:** auto`.

**Plugin integration.** The opencode plugin source lives at
`sovereign-cli/assets/sovereign-atos.ts`, embedded into the CLI binary via
`include_str!` with a `// sovereign-atos-version: X.Y.Z` header. It injects
`X-Feature-Id` (from the current git branch's feature dir) and `X-Session-Id`
on every request so the daemon knows which feature's spec to splice.

### 4.14 Local Corpora — Folder Drop + Obsidian Vault

Two user-visible flows in **Settings → Local Knowledge** — "Drop or
browse a folder" (PDFs + TXT) and "Connect Obsidian vault" (markdown) —
are instances of the same operation: the user points Sovereign at a
local directory; Sovereign pre-scans it, ingests it through the shared
corpus-engine pipeline, and maintains the relationship over time. The
two flows differ only in configuration and extension points, not in
architecture.

**Crate layout** — `sovereign-tools/src/local_corpus/`:

| Module          | Responsibility                                                                                              |
|-----------------|-------------------------------------------------------------------------------------------------------------|
| `config.rs`     | `LocalCorpusConfig`, `document_folder` + `obsidian_vault` factories, `recipe_toml` builder                  |
| `pre_scanner.rs`| Directory walker + PDF classifier (`Readable` / `ScannedNoText` / `PasswordProtected` / `Corrupt`)          |
| `humanise.rs`   | Filename → display-name rules (date normalisation, ordering-prefix strip, title-case, acronym allow-list)   |
| `extract_stage.rs` | PDF/TXT/MD → JSONL staging; `safe_extract_pdf_text` catches pdf-extract panics so one bad file can't nuke the ingest |
| `progress.rs`   | `LocalCorpusProgress` — one enum covering Scanning / Staging / Ingesting / Clustering / Snapshotting / Writing / RollingBack / Complete / Error |
| `excerpt.rs`    | Post-ingest excerpt scorer (length + diversity) for the completion screen                                   |
| `clusterer.rs`  | Obsidian-only: wraps `cluster_embeddings` (HDBSCAN) + LLM labelling pass using spec §6.3 `domain/subtopic` prompt |
| `preview.rs`    | Pure builder for `VaultPreview` — cluster summaries, outlier classification (`LowConfidence`, `TooShort`, `AmbiguousCluster`) |
| `frontmatter.rs`| YAML frontmatter merge + strip — value-perfect preservation of user keys; only `<namespace>/*` tags and `<namespace>_*` keys are ever touched |
| `writeback.rs`  | `take_snapshot` → `write_file_tags` → `write_cluster_index` → `rollback` / `clean`; atomic per-file rename; 3-snapshot retention pruning |
| `git.rs`        | `check_git_repo` + `git_commit_before_write` via `std::process::Command` (no `libgit2` dep)                 |
| `manager.rs`    | `LocalCorpusManager` — owns `CorpusEngine` handle, registered-corpora map, in-memory cluster-result cache   |

**Architectural invariants** (enforced by tests in-crate):

- **Snapshot before any write.** `WriteBack::execute` takes an atomic
  JSON snapshot under `~/.sovereign/vault-snapshots/{corpus_id}/`
  **before** the first file is touched. Crash mid-write → snapshot
  exists → rollback restores. Snapshot directory lives **outside** the
  vault (inside would self-ingest on the next scan).
- **`<namespace>/*` namespace is inviolable.** Only
  `<namespace>/` tags and `<namespace>_*` frontmatter keys are added,
  modified, or removed. Every other key and tag in every note
  round-trips at the value level.
- **Rollback is idempotent.** Running `rollback` twice on the same
  snapshot produces the same result; deleted-since-snapshot files
  are re-created from the snapshot payload, not reported as errors.
- **pdf-extract panics are caught.** `safe_extract_pdf_text` runs
  `pdf_extract::extract_text` inside `catch_unwind` so a DeviceN
  colour-space panic (known failure mode in `pdf-extract 0.7.12`)
  is classified as `Corrupt` rather than taking down the
  `spawn_blocking` task.

**Tauri command surface** (in `sovereign-desktop/src-tauri/src/local_corpus_commands.rs`):

`lc_validate_path`, `lc_pre_scan`, `lc_ingest`, `lc_list`, `lc_remove`,
`lc_incomplete_jobs`, `lc_search`, `lc_cluster`, `lc_get_preview`,
`lc_check_git`, `lc_write_tags`, `lc_list_snapshots`, `lc_rollback`,
`lc_clean`, `lc_cancel`. Progress events stream on
`local-corpus://progress/{job_id}`.

**Frontend component tree** lives under
`sovereign-desktop/src/lib/components/local-knowledge/`; `folder/` and
`obsidian/` subdirectories hold flow-specific components. Mounted as
the "Local Knowledge" tab of `SettingsPanel.svelte`.

**Resume-on-relaunch** is free: `CorpusEngine::ingest` checkpoints to
`_source_manifest.json` on every flush; `lc_incomplete_jobs` surfaces
any corpus with non-`Complete` entries; the ResumePrompt's "Continue"
button just re-invokes `lc_ingest`, which picks up at the last
completed shard.

---

## 5. Commonwealth — The Coordination Daemon

A symmetric daemon. Every machine runs the same binary. There is no master.
Members find each other via mDNS on the LAN or transitively over a VPN
(Tailscale/WireGuard) and converge on a shared view of the mesh through gossip.

### 5.1 What it does in one sentence

Translates a request like "complete this chat with model X" into a concrete
plan that spawns `llama-server` on one node and `rpc-server` on the others,
holds the resulting OpenAI-compatible HTTP endpoint open for clients, and
keeps the plan healthy as nodes come and go.

### 5.2 Discovery and membership

- **Join keys** — `cwth-XXXX-XXXX-XXXX`. `membership::generate_join_key` produces a key, stores its BLAKE3 hash, and discards the plaintext. `verify_join_key` is constant-time. The first node calls `init_mesh`; subsequent nodes call `accept_join` to add themselves to the `Mesh`.
- **mDNS** — Each daemon advertises `_commonwealth._tcp.local` with `node_id`, `mesh_id`, `name`. `MdnsDiscovery::browse` populates a `DiscoveredPeer` map.
- **Gossip** — `gossip_service.rs` runs a 10-second epidemic loop, picking 2–3 random peers per round. `GossipMessage` is a three-phase digest/delta/response exchange. Conflicts resolved by timestamp (last-write-wins). Payload kinds: `MemberState`, `InferencePlan`, `KnowledgePlan`, `LedgerEntry`, `MeshConfig`.
- **Latency probing** — UDP RTT every 30s with the magic bytes `CWLP` and EWMA smoothing (α=0.3). The resulting `LatencyMatrix` is shared via gossip and consumed by the scheduler for topology-aware layer ordering.
- **Hardware detection** — `discovery/hardware.rs` tries `nvidia-smi`, then `rocm-smi`, then Metal. Outputs `HardwareProfile { gpus, ram_gb, storage_gb, cpus }` with `GpuInfo { name, vram_gb, compute_type, tflops }`.
- **TLS** — `tls.rs` generates per-session certificates with `rcgen` and pins them on the internal API.
- **Mesh peering** — `peering.rs` lets two meshes establish federation via out-of-band key exchange. Two `PeerTrustLevel`s: `ModelAndKnowledgeSharing`, `Full`.

### 5.3 Scheduling

`commonwealth-inference/scheduler` is a pure-functional layer over the gossiped state. A
deterministic per-decision leader (lowest `NodeId`) prevents thrash without
needing consensus.

| Module                   | Algorithm                                                |
|--------------------------|---------------------------------------------------------|
| `layer_assignment.rs`    | Proportional VRAM allocation, contiguous ranges per node, topology-aware ordering using `LatencyMatrix`, privacy-aware entry-node preference |
| `plan_builder.rs`        | `build_shard_plan` (single model) and `build_inference_plan` (multiple models), with `estimate_performance` for TPS / TTFT estimates |
| `knowledge_assignment.rs`| Greedy by free storage; whole-corpus if it fits, otherwise `ChunkRange` split; replicas placed on different nodes; respects per-corpus `mesh_sharing` flag |
| `oicp_cache.rs`          | Hashes `CapabilityRequirements` to a `(ModelId, score)` cache keyed by portfolio version, invalidated on portfolio change |
| `portfolio.rs`           | `ModelPortfolio` with `ModelTransition` state machine (`Loading → Ready → Complete`); `SWAP_THRESHOLD = 0.3` decides whether to evict |
| `usage_predictor.rs`     | Counts requests by `(weekday, hour, CapabilityCategory)` to predict dominant capability for preemptive loading |

### 5.4 Orchestration

`commonwealth-inference/orchestrator` is the side-effect layer that turns scheduler
output into running processes.

- `Orchestrator::apply_shard_plan` spawns `llama-server` on the entry node
  (allocating sequentially from `next_llama_port`) and `rpc-server` on remote
  nodes that hold layer subsets.
- `ManagedProcess` tracks `id`, `kind` (`LlamaServer | RpcServer`), `state`
  (`Starting | Running | Unhealthy | Failed | Stopped`), `pid`, `child`,
  `listen_address`, `spawned_at`. `stop()` does graceful SIGTERM with a
  configurable timeout, then SIGKILL.
- `HealthTracker` polls every 5s by default (HTTP for llama-server, TCP for
  rpc-server), keeps a 20-sample latency history, marks `Unresponsive` after 3
  consecutive failures. Statuses: `Healthy, Degraded { reason }, Unresponsive,
  Dead, Unknown`.
- `GracefulDeparture` is a 30-second countdown state machine
  (`Announced → Rebalancing → Draining → Complete`) so a node can leave
  without dropping in-flight requests.
- `FaultDetector` collapses health changes into `FaultEvent`s that the daemon
  can act on.

### 5.5 HTTP API

Two listeners, two trust domains.

**Client API — port 9741, no mTLS**

| Method | Path                              | Notes                                         |
|--------|----------------------------------|-----------------------------------------------|
| POST   | `/v1/chat/completions`           | OpenAI-compatible. Routing priority: OICP requirements → exact model name → glob alias (via `model_aliases.rs`) → default. `LocalOnly` privacy is rejected (400). |
| GET    | `/v1/models`                     | Loaded models with capabilities and performance estimates |
| POST   | `/v1/knowledge/search`           | Determines target corpora, fans out to shard nodes, merges, reranks. Wired to `corpus-engine` (search call site landed; cross-node fan-out is the active integration point). |
| GET    | `/status`                        | Comprehensive node/mesh/inference/knowledge/contribution summary |
| GET    | `/oicp/v1/capabilities`          | Provider manifest + federation info           |

**Internal API — port 9742, mTLS**

| Path                                 | Purpose                          |
|--------------------------------------|---------------------------------|
| `POST /internal/gossip`              | Gossip exchange                 |
| `POST /internal/scheduling/intent`   | Scheduling decision notification|
| `POST /internal/scheduling/plan`     | New shard plan distribution     |
| `POST /internal/model/transfer`      | Model file transfer (peer-to-peer) |
| `POST /internal/index/transfer`      | Corpus shard transfer           |
| `POST /internal/knowledge/search`    | Inter-node shard query (fan-out target) |
| `GET  /internal/latency/probe`       | Latency probe response          |

### 5.6 Knowledge

`commonwealth-knowledge` wraps `corpus-engine`:

- `MeshCorpusManager` — install / list / remove corpora.
- `ShardManager` — `prepare_shards` extracts per-node shards for distribution; `install_received_shard` takes a transferred shard and integrates it locally; `consolidate_shards` merges all local shards into a complete index.
- `embed_http::http_embed_fn` — builds an `EmbedFn` that POSTs to a remote `/v1/embeddings` endpoint (default model `qwen3-embedding-0.6b`). This is how a node without a local embed model still ingests via the engine.
- `grounding.rs` — `GroundingConfig { enabled, corpora, max_chunks, max_context_tokens, min_relevance, citation_instructions }` and `search_for_grounding` / `format_knowledge_context` for system-prompt injection with citation markers.

### 5.7 Ledger and fairness

- `LedgerStore` is append-only. Entries are `LedgerEntryKind { Contributed { served_request_from }, Consumed { served_by } }` with units `GpuSeconds | StorageGbDays | BandwidthGb`.
- `NodeBalance` rolls a 30-day window into `compute_hours, storage_gb_days, bandwidth_gb, balance`.
- `FairnessPolicy` is a per-mesh choice: `Transparent` (everyone sees the ledger, social pressure), `SoftThrottle { threshold_hours, priority_reduction }`, or `HardCap { threshold_hours }`. Decisions emerge as `FairnessDecision { Allow, Throttle, Deny }`.

### 5.8 Test harness

`commonwealth-test-harness` provides the integration story:

- `SimulatedMesh` — orchestrates many `SimulatedNode`s in-process, each with its own `AppState` and HTTP listeners on random ports.
- `SimulatedNodeBuilder` — fluent builder for hardware profiles.
- `MockLlamaServer` — Axum app responding to `/v1/chat/completions` and `/health` with canned responses, request counting via `Arc<AtomicU64>`.
- `fixtures.rs` — reusable hardware profiles, models, capability profiles.

`tests/integration.rs` covers mesh formation, gossip convergence, layer
assignment correctness, inference E2E through the mock llama-server, fault
recovery, graceful pause/resume, OICP routing, multi-model portfolio,
knowledge query fan-out, ledger accuracy, fairness throttling. All tests run
in deterministic time with no real 10-second gossip waits.

### 5.9 CLI

`commonwealth-daemon` defines the CLI structure with `clap`:

```
commonwealth init --name "..."          Create a mesh, get a join key
commonwealth join <key>                 Join an existing mesh
commonwealth status                     Mesh state, members, models, capacity
commonwealth balance                    Contribution ledger
commonwealth models                     Available and loaded models
commonwealth corpora                    Hosted knowledge bases
commonwealth corpus install/remove/update/list/consolidate
commonwealth pause / resume             Graceful departure and return
commonwealth leave                      Permanent departure
commonwealth logs [--follow]            Daemon logs
commonwealth mesh members               List members with status
commonwealth mesh set <key> <value>     Propose config change
commonwealth mesh revoke <node>         Propose removing a member
commonwealth mesh peer <key>            Establish peering with another mesh
commonwealth daemon start/stop/status   Daemon lifecycle
```

`init` and `join` are wired through to `discovery::membership`. The remaining
commands are scaffolded; the daemon orchestration loop that ties config
loading, signal handling, and component startup into a single long-running
process is the active surface.

### 5.10 Applications (`commonwealth-app`)

The mesh application platform enables third-party apps to run on the mesh:

- `MeshAppManifest` — static app description gossiped across nodes: `app_id`, `version`, `entrypoint`, `permissions`, `required_capabilities`
- `AppPermissions` — declares `mesh_store_read`, `mesh_store_write`, `inference_access`, `knowledge_access`
- `AppRegistry` — in-memory registry of known apps
- `AppProcess` — lifecycle state machine (`Stopped → Starting → Running → Failed`)
- `AppPortMap` + `forward()` — HTTP reverse-proxy helpers for routing traffic to mesh-hosted app processes

### 5.11 Distributed State (`commonwealth-state`)

`MeshStore` is a gossip-replicated distributed key-value store backed by SQLite (WAL mode):

- Entries are `StoreEntry { app_id, key, value: Bytes, timestamp, origin: NodeId }`
- Scoped by `app_id` so different mesh apps have isolated key namespaces
- Last-write-wins (LWW) conflict resolution using Unix-second timestamps
- `merge_entry()` accepts entries from gossip and resolves by timestamp
- `all_entries_for_gossip()` exports the full store for broadcast
- `RetentionGc` provides TTL-based garbage collection

### 5.12 Deployment

`contrib/` contains platform service files:

- `install.sh` — curl installer script
- `systemd/commonwealth.service` — systemd unit file for Linux
- `launchd/com.commonwealth.daemon.plist` — macOS launchd plist

---

## 6. How the Four Projects Fit Together

### 6.1 Sovereign standalone

Sovereign runs by itself with no mesh. Inference is local via
`EmbeddedLlamaCpp`. Knowledge bases are installed via `MeshCorpusManager` (the
crate is named for the mesh case but works fine without one) and indexed by
`corpus-engine`. The `EmbedFn` injected into the engine wraps Sovereign's
local Embed slot directly. This is the default configuration of the desktop
app and CLI.

### 6.2 Commonwealth standalone

Commonwealth runs as a daemon serving `localhost:9741`. Any OpenAI-compatible
client (Open WebUI, LiteLLM, `curl`) points at it and gets distributed
inference for free. Knowledge ingestion uses `embed_http::http_embed_fn` to
call a local or remote embeddings endpoint, so the daemon can index without
shipping its own embedding model.

### 6.3 Sovereign + Commonwealth (the integrated case)

Sovereign embeds Commonwealth in-process via `sovereign-mesh::EmbeddedDaemon`.
When the user clicks "Create mesh" or accepts a `sovereign://join` deep link,
the daemon spins up inside the Sovereign process (internal port 9742, client
API 9741) with no separate binary. The Runtime's `inference` field is wrapped
in `sovereign_mesh::peer_inference::MeshInferenceProvider`, which routes
synthesis calls to peers when OICP scoring favours them:

```
User → Sovereign Runtime → Router → Planner → Executor
                                       │
                                       └─► MeshInferenceProvider
                                              │
                                              ├─ pick_better(local_manifest,
                                              │              peer_manifests...)  ← OICP score
                                              │                                    then size_gb
                                              ├─ if local wins → EmbeddedLlamaCpp
                                              └─ if peer wins  → POST /v1/chat/completions
                                                                 to that peer's :9741
                                                                 (their SovereignInferenceAdapter
                                                                  re-applies OICP to pick Fast vs Slow slot)
```

Both sides consult the same scoring primitives in `sovereign_mesh::oicp_select`
so the Joiner's selected model and the Founder's served slot can't drift.
Streaming calls use `complete_stream_with_id` — a companion to `complete_stream`
that returns the model attribution alongside the stream, so peer-served
completions show up in `ResponseProvenance.inference_backend` as
`"Qwen3.5-9B.Q8_0 @ peer BeefyMac"` rather than the local model name.

A skill that declares `privacy = "local_only"` (e.g. `inner-work`) sets
`ShardingPrivacy::LocalOnly` on the outgoing OICP envelope; the wrapper
honours this by short-circuiting to the local provider regardless of score.

Knowledge bases follow the same pattern: Sovereign owns the local index,
Commonwealth distributes shards across nodes via `ShardManager`, and both use
the same `corpus-engine` schema so a shard transferred between nodes is
immediately searchable through `CorpusIndex::search` with no migration step.

### 6.3.1 Desktop attach mode (CLI-started daemon + hot reload)

The desktop app and the CLI (`sovereign daemon run`, installed as a
launchd/systemd service by `sovereign setup`) both want to own `:9741`.
Rather than colliding, the desktop probes `http://127.0.0.1:9741/v1/models`
at startup via `sovereign-desktop::bootstrap::detect` (see
`src-tauri/src/bootstrap.rs`). If the probe succeeds, the desktop
enters **Attach** mode:

- inference flows through `RemoteApiProvider` pointing at the running
  daemon instead of starting an in-process `EmbeddedDaemon`,
- mesh mutations (`create/join/rotate/leave`) go over HTTP via
  `sovereign-mesh::mesh_http` (`/v1/mesh/*` — all localhost-only),
- the setup wizard's `detecting` state skips the model and knowledge
  screens when `SetupConfig` on disk already covers those fields,
- `commands::save_config` mirrors shared fields (model paths, data
  dir) back into `~/.config/sovereign/config.toml` and POSTs
  `/v1/admin/reload` so the running daemon swaps its
  `InferenceProvider` in place — no service restart, no visible
  inference gap.

The admin surface (`sovereign-mesh::admin_http::admin_router`) merges
alongside `mcp_router` and `mesh_router` on `:9741`. The reload handler
diffs the incoming `SetupConfig` against the daemon's baseline and
rebuilds only what changed:

| Changed field                       | Reload action                                      |
|-------------------------------------|----------------------------------------------------|
| `models.primary` / `.fast` / `.embed` | Rebuild provider via `ProviderFactory`, swap atomically |
| `daemon.client_port` / `.internal_port` | `restart_required: true` (handler refuses to rebind) |
| `data.dir`                          | `restart_required: true` (SQLite handles mid-flight) |

`sovereign-cli::daemon_cmd::LlamaCppFactory` is the concrete factory
the CLI daemon installs; it calls `EmbeddedLlamaCpp::load_full_with_families`
with the same parameters as cold start. When `restart_required: true`
surfaces, `save_config` falls back to `launchctl kickstart -k
gui/$(id -u)/com.sovereign.daemon` on macOS or `systemctl --user
restart sovereign` on Linux. Smoke test at
`sovereign/scripts/smoke-attach-mode.sh`.

**Security posture on `:9741`.** The client listener binds `0.0.0.0`
(federated inference needs peers to reach `/v1/chat/completions`), so
the admin surfaces sharing that port — `/v1/mesh/*`, `/v1/admin/*`,
`/mcp/*` — are defended in two layers:

1. **Router-level middleware** ([`sovereign_mesh::loopback_guard::loopback_only`](crates/sovereign-mesh/src/loopback_guard.rs))
   applied via `.layer(axum::middleware::from_fn(..))` on each of
   `mesh_router`, `admin_router`, and `mcp_router`. Rejects non-
   loopback peers with 403 before any handler runs, and **fails
   closed** with 500 when `ConnectInfo` is missing (a wiring bug
   surface — see point 3).
2. **Per-handler `enforce_localhost` extraction** of
   `ConnectInfo<SocketAddr>`. Belt + suspenders: if the middleware
   layer is ever accidentally stripped, the handler still denies.
3. **The listener must use
   `.into_make_service_with_connect_info::<SocketAddr>()`** in
   `daemon::start_daemon` — bare `axum::serve(listener, router)`
   leaves `ConnectInfo` absent and breaks the guards for *every*
   caller (including localhost). Pinned by
   `admin_http::tests::loopback_guard_works_under_production_listener_shape`.

`/v1/chat/completions` is intentionally left unauthenticated today —
the Commonwealth "closed trust ring" model assumes network-level ACLs
(Tailscale, LAN firewall) bound reachability to mesh members. A
future revision should add per-request auth against
`Mesh.join_key_hash` so a reachable-but-non-member attacker can't
burn inference budget.

### 6.4 Shared protocols

Two protocols cross the Sovereign/Commonwealth boundary:

- **OICP** — The canonical specification lives at
  `commonwealth/docs/oicp-v0.2.md`. The Rust implementation lives in
  `oicp-types/src/lib.rs` — a standalone crate at the workspace root that
  both projects consume via path dependency. It is re-exported as
  `sovereign_core::oicp` and `commonwealth_core::oicp` so downstream code
  uses familiar module paths. A request produced by one project serializes
  through `serde` round-trips into the other without translation.
- **`EmbedFn` injection** — `corpus-engine::EmbedFn` is the only contract
  between the corpus layer and any embedding backend. Sovereign and
  Commonwealth implement different concrete closures over the same type.

---

## 7. Build, Test, Run

### 7.1 Prerequisites

- Rust toolchain (stable)
- `cmake` (llama.cpp build)
- `protoc` (LanceDB pulls in `lance-table` which needs protobuf)
  - macOS: `brew install protobuf`
  - Debian: `apt install protobuf-compiler`
- For Commonwealth: `llama-server` and `rpc-server` binaries from the
  `llama.cpp` project on `PATH`
- For desktop: Node.js + Tauri 2 prerequisites (`cargo install tauri-cli --version "^2"`)

### 7.2 Build

Each project is its own Cargo workspace.

```sh
# corpus-engine
cd corpus-engine && cargo build --release

# Sovereign — all crates
cd sovereign && cargo build --release

# Commonwealth — all crates
cd commonwealth && cargo build --release
```

### 7.3 Test

```sh
cd corpus-engine && cargo test                # ~95 tests
cd sovereign     && cargo test --workspace     # ~289 tests
cd commonwealth && cargo test --workspace     # ~222 unit + integration tests
```

No tests require a GPU, model file, or network access. Sovereign uses
`DeterministicInference` + in-memory SQLite + real FTS5 for functional tests.
Commonwealth's `commonwealth-test-harness` runs simulated meshes with mock
llama-servers under deterministic timing.

### 7.4 Run

```sh
# Sovereign desktop
cd sovereign/crates/sovereign-desktop && npm install && cargo tauri dev

# Sovereign CLI
cd sovereign && cargo run --release -p sovereign-cli -- \
  --model models/qwen3-1.7b.gguf --primary-model models/qwen3.5-9b.gguf

# Sovereign HTTP server
cd sovereign && cargo run --release -p sovereign-server -- --config sovereign-server.toml

# Commonwealth daemon
cd commonwealth && cargo run --release -p commonwealth-daemon -- init --name "Co-op"
cd commonwealth && cargo run --release -p commonwealth-daemon -- daemon start
```

Default ports:

| Port  | Service                                     |
|-------|---------------------------------------------|
| 9741  | Commonwealth client API (OpenAI-compatible) |
| 9742  | Commonwealth internal API (mTLS)            |
| 9743+ | `llama-server` instances                    |
| 50051+| `rpc-server` instances for layer shards     |
| 8080  | Sovereign HTTP server (configurable)        |

---

## 8. Deployment Topologies

| Topology                 | What it looks like                                                                                                                                                          |
|--------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Sovereign desktop, alone | Tauri app, embedded llama.cpp, SQLite, local corpus indexes. No network required after model download.                                                                      |
| Sovereign CLI, alone     | Same as desktop minus the GUI.                                                                                                                                              |
| Sovereign server         | Axum REST + WebSocket, multi-tenant via `tenant.rs`, SQLite or PostgreSQL store. Suitable as an internal team endpoint.                                                     |
| Commonwealth daemon, alone | Standalone OpenAI-compatible endpoint backed by a mesh. Any compliant client works.                                                                                       |
| Sovereign + embedded mesh | Sovereign desktop with `sovereign-mesh::EmbeddedDaemon` running in-process. The user creates or joins a mesh through the desktop UI; deep links handle invitation flow.    |
| Mesh of meshes           | Two Commonwealth meshes establish peering via `peering.rs` with a chosen `PeerTrustLevel`. Internal model and index transfer routes carry the federation traffic.           |

All topologies share the same on-disk index format, the same OICP capability
matching, and the same skill manifests. A skill written for desktop Sovereign
runs unmodified against a Commonwealth-backed remote.

---

## 9. Project Status (as built)

| Subsystem                                 | State                                              |
|-------------------------------------------|---------------------------------------------------|
| corpus-engine pipeline (acquire/extract/chunk/index) | Production                                |
| corpus-engine sharding (3-op contract)    | Production                                        |
| corpus-engine enrichment (field model, domain-extensible) | Production, opt-in per recipe    |
| corpus-engine recipe registry + snapshot  | Production                                        |
| corpus-engine delta updates               | Production                                        |
| sovereign-recipes registry catalog        | Production                                        |
| Sovereign trait architecture + Runtime    | Production                                        |
| Sovereign embedded llama.cpp (dual-slot)  | Production                                        |
| Sovereign hybrid provider + OICP selector | Production                                        |
| Sovereign SQLite/Postgres/in-mem stores   | Production                                        |
| Sovereign tools (search, web, RAG, document, MCP, shell, file) | Production                  |
| Sovereign code intelligence (24-tool MCP server: symbol index, SCIP call graph, lint/test watchers, notes, ATOS feature lifecycle, project context, session reflection, doc health) | Production |
| Sovereign KnowledgeView (3-view landscape digest assembly, cross-view resonance) | Production |
| Sovereign ATOS project layer (init/found/amend/phase/audit) + feature layer (provision/milestones/teardown) | Production |
| Commonwealth sovereign-coder pipeline + middleware stack (approval_gate / session_briefing / context_injector / tool_injector / artifact_surface) | Production |
| Sovereign epistemic tools                 | Production (against enriched corpora)             |
| Sovereign skills + planner templates      | Production                                        |
| Sovereign desktop (Tauri 2 + Svelte 5)    | Production for single-user                         |
| Sovereign HTTP server (multi-tenant)      | Production                                        |
| Sovereign mesh embed (`sovereign-mesh`)   | Functional                                        |
| Commonwealth core types + ledger          | Production                                        |
| Commonwealth discovery (mDNS, gossip, latency, hardware, TLS, peering) | Production           |
| Commonwealth inference (scheduler + orchestrator) | Production                                |
| Commonwealth API (client + internal)      | Production                                        |
| Commonwealth ledger + fairness policies   | Production                                        |
| Commonwealth knowledge fan-out            | Active integration — types and shard pipeline complete; cross-node fan-out + merge is the current edge |
| Commonwealth mesh peering federation      | Trust establishment + record types complete; cross-mesh model/index transfer routes are stubs |
| Commonwealth app platform (`commonwealth-app`) | Functional                                  |
| Commonwealth distributed state (`commonwealth-state`) | Production                           |
| Commonwealth daemon CLI (lifecycle wiring)| Init/join wired; long-running daemon orchestration loop is the active edge |
| OICP shared types (`oicp-types`)          | Production                                        |

The pieces marked "active integration" or "active edge" are where development
is currently happening. Everything above them is stable and covered by tests.

---

## 10. Where to Look for What

| You want to                           | Read                                                 |
|---------------------------------------|------------------------------------------------------|
| Understand the agent runtime          | `sovereign/crates/sovereign-core/src/runtime.rs`     |
| See how plans are executed            | `sovereign/crates/sovereign-core/src/executor.rs`    |
| Add a tool                            | `sovereign/crates/sovereign-core/src/traits.rs` then a new file under `sovereign-tools/src/` |
| Add a corpus parser                   | `corpus-engine/src/extractors/` then register in `engine/ingest.rs` |
| Write a recipe                        | `sovereign-recipes/<id>/recipe.toml` then add entry to `registry.toml` |
| Write a skill                         | `sovereign/skills/<id>/skill.toml`                    |
| Understand code intelligence tools    | `sovereign/crates/sovereign-tools/src/code/mod.rs`   |
| Understand the SCIP call graph        | `corpus-engine/src/scip_graph.rs` (schema, staleness, queries) |
| Add a SCIP language exporter          | `corpus-engine/src/scip_export.rs` → `all_exporters()` |
| See the MCP server (code intelligence)| `sovereign/crates/sovereign-cli/src/project_cmd.rs` (`cmd_serve`, inline `mcp_server` module) |
| See the MCP server (Sovereign HTTP)   | `sovereign/crates/sovereign-server/src/routes_mcp.rs` |
| Understand session reflections        | `corpus-engine/src/notes.rs` (NoteStore, write_reflection, retire_by_tool) and `sovereign/crates/sovereign-cli/src/reflect_cmd.rs` |
| Tune model selection per hardware     | `sovereign/models.toml`                               |
| Trace a Commonwealth scheduling decision | `commonwealth/crates/commonwealth-inference/src/scheduler/plan_builder.rs` |
| Trace a Commonwealth shard plan       | `commonwealth/crates/commonwealth-inference/src/scheduler/layer_assignment.rs` |
| Trace process spawning                | `commonwealth/crates/commonwealth-inference/src/orchestrator/process.rs` |
| Add an internal mesh route            | `commonwealth/crates/commonwealth-api/src/routes_internal.rs` |
| Stand up a multi-node test            | `commonwealth/crates/commonwealth-test-harness/`     |
| See OICP routing logic                | `sovereign/crates/sovereign-inference/src/selector.rs` and `commonwealth/crates/commonwealth-api/src/routes_inference.rs` |
| See OICP type definitions             | `oicp-types/src/lib.rs`                              |
| Understand index storage on disk      | `corpus-engine/src/index/mod.rs`                     |
| Understand the embedding injection    | `corpus-engine/src/types.rs` (`EmbedFn`) and `commonwealth/crates/commonwealth-knowledge/src/embed_http.rs` |
| Understand enrichment domains         | `corpus-engine/src/enrichment/domain.rs` and `enrichment/domains/` |
| Understand the v2 enrichment pipeline | `corpus-engine/src/enrichment/pipeline/mod.rs` (`Pipeline` trait, `PipelineRegistry`, `ExemplarBank`, `PhaseCache`) |
| Add a new v2 pipeline                 | `corpus-engine/src/enrichment/pipeline/pipelines/` + `PipelineRegistry::builtin` |
| Drive v2 enrichment from the CLI      | `sovereign-cli/src/enrich_cmd/` (lands in Landing 2) |
| Understand the recipe registry        | `corpus-engine/src/registry.rs`                      |
| Understand delta updates              | `corpus-engine/src/update/delta.rs`                  |
| Understand KnowledgeView digest assembly | `sovereign/crates/sovereign-tools/src/knowledge_view/manager.rs` (lifecycle), `cross_view.rs` (resonance), `recipes.rs` (three-view + privacy) |
| See where KnowledgeView is injected into the prompt | `sovereign/crates/sovereign-core/src/runtime.rs` (`splice_landscape_digests` + `build_system_message`) and `traits.rs` (`LandscapeDigestProvider`) |
| Understand ATOS charter + spec lifecycle | `sovereign/crates/sovereign-atos/src/local.rs` (orchestrator), `charter.rs` (parsing), `approval.rs` (SHA-256 drift detection) |
| See the ATOS CLI surface              | `sovereign/crates/sovereign-cli/src/atos_cmd.rs` and `project_cmd.rs` (`cmd_found`, `cmd_amend`, `cmd_phase`, `cmd_audit`) |
| Trace a sovereign-coder pipeline turn | `commonwealth/crates/commonwealth-api/src/middleware/` (`mod.rs`, `approval_gate.rs`, `context_injector.rs`, `tool_injector.rs`, `artifact_surface.rs`) + `commonwealth-core/src/default_pipelines.toml` (alias table) |
| Install / upgrade the ATOS opencode plugin | `sovereign/crates/sovereign-cli/assets/sovereign-atos.ts` (source) + `sovereign-cli/src/atos_plugin.rs` (`include_str!` + version header + installer) |
| Run the long-running Sovereign daemon | `sovereign/crates/sovereign-cli/src/daemon_cmd.rs` (`run`) + `contrib/launchd` + `contrib/systemd` |

---

## 11. Glossary

- **OICP** — Open Inference Capabilities Protocol. A schema for declaring what
  a model is good at (`Code`, `Analysis`, ...) at a 0–4 proficiency level, plus
  required vs preferred constraints, latency preferences, and privacy
  restrictions. Defined in `oicp-types`, re-exported by both Sovereign and
  Commonwealth.
- **Recipe** — A TOML file in `sovereign-recipes` that describes how to ingest
  one corpus end-to-end (acquire, extract, chunk, index, optionally enrich).
- **Registry** — The recipe catalog in `sovereign-recipes/registry.toml`.
  `corpus-engine` ships a compile-time bundled snapshot and can refresh from
  GitHub for the latest recipes.
- **Field Model** — The enrichment system's approach: five phases (skeleton
  extraction, HDBSCAN clustering, alignment, fault lines, open questions)
  that analyze a corpus holistically rather than per-chunk.
- **Domain** — A `corpus-engine` trait (`enrichment/domain.rs`) encoding the
  epistemic conventions of a field of knowledge (philosophy, science, etc.).
  The single extension point for the field model enrichment system.
- **SCIP** — Source Code Intelligence Protocol, a language-neutral format for
  symbol definitions and references. `scip_graph.rs` stores SCIP data in SQLite;
  `scip_export.rs` dispatches to language-specific analyzers (`rust-analyzer`,
  `scip-typescript`, etc.) to produce the data.
- **ScipGraph** — The SQLite database storing symbol definitions, call-site
  references, and staleness metadata. Queried by `find_callees` and
  `find_callers`. Staleness is tracked per-file via `CodeWatcher` integration.
- **Shard** — A `corpus-engine` index containing only a contiguous chunk-ID
  range. Structurally identical to a complete index.
- **Skill** — A TOML file in Sovereign that configures routing triggers,
  planner templates, prompt overrides, memory rules, and OICP requirements
  for a class of work. No code required.
- **Slot** — One of the three model loading positions in Sovereign's embedded
  inference: Fast (router/compression), Primary (planning/synthesis), Embed
  (vector embeddings).
- **Mesh** — A closed trust ring of Commonwealth nodes that share inference
  and knowledge. Joined via a `cwth-XXXX-XXXX-XXXX` key.
- **Peering** — A trust relationship between two distinct meshes that lets
  them exchange models or knowledge under a chosen `PeerTrustLevel`.
- **CodeWatcher** — Filesystem watcher (`notify` crate) that re-indexes
  modified source files via `CorpusEngine::reindex_file()` and marks them
  stale in the `ScipGraph`. 800ms debounce window collapses editor saves.
- **EmbedFn / InferenceFn** — The two function types `corpus-engine` accepts
  from its caller for embedding text and (optionally) running an LLM during
  enrichment. Keeps the engine free of any specific runtime.
- **KnowledgeView** — Sovereign's three-map landscape-digest system (personal
  memories, 180-day conversation history, institutional notes) spliced into
  the system prompt before each turn via `LandscapeDigestProvider`. Strict
  local-scope privacy is structural, not policy. See §4.12.
- **Landscape digest** — A compact markdown block summarizing one
  KnowledgeView map within a per-view token budget. Cross-view resonance is
  surfaced tentatively ("may resonate with"), never as an assertion.
- **ATOS** — Agent Task Orchestration System. Two-layer repo scaffolding
  (project charter + per-feature specs) that injects the relevant contract
  into every agent turn, detects SHA-256 drift against the approved version,
  and records every decision / deviation / milestone outcome under
  `.sovereign/`. See §4.13.
- **Charter** — ATOS specification document (`CHARTER.md` for the project,
  `spec.md` for a feature). Committing it is approval.
- **Drift** — ATOS term for "spec file changed since approval." Warns in the
  next turn's preamble; does not block. Either revert or `atos spec accept`.
- **Sovereign-coder pipeline** — Commonwealth middleware chain
  (`approval_gate → session_briefing → context_injector → tool_injector →
  artifact_surface`) that adapts a generic coder model into an ATOS-aware
  one by splicing charter + spec + scoped notes into every request.

---

## 12. Architecture Roadmap

Future improvement candidates identified through SOLID analysis. These are
proposals, not active work.

### Enrichment v1 → v2 migration

v1 (`FieldModelEngine` + `Domain` trait + 9 impls) and v2 (`Pipeline` trait +
`LiteraryPipeline` + admin CLI harness) currently coexist (see §3.5.1). Once
v2 has been proven on ≥2 text corpora under the `sovereign enrich` admin
loop, the remaining domains (`philosophy`, `personal`, `conversational`,
`institutional`; four stubs can stay deleted) migrate onto `Pipeline`, the
`sovereign enrich` CLI retires or demotes to a diagnostic helper, and
`enrichment/field_engine.rs` + `domain.rs` + `domains/` are deleted. The
retirement PR is sequenced after the admin harness ships; migrating
KnowledgeView's three domains is the largest single subtask (shared code
paths with `sovereign-tools/src/knowledge_view/`).

### SRP: Large-file decomposition

~~**`corpus-engine/src/engine.rs`**~~ — Done. Split into `engine/mod.rs`
(facade) + `engine/ingest.rs` (pipeline) + `engine/reindex.rs` (per-file).

~~**`corpus-engine/src/index.rs`**~~ — Done. Split into `index/mod.rs`
(struct + metadata) + `index/create.rs` + `index/search.rs` + `index/write.rs`
+ `index/enrichment.rs`.

**`corpus-engine/src/enrichment/field_engine.rs`** (1,337 lines) — lighter
touch: move `get_overview_chunks()` to `filter.rs`, move skeleton parsing to
`skeleton.rs`, keep the orchestrator method in place.

**`sovereign-tools/src/knowledge_view/manager.rs`** (1,332 lines) — five
concerns in one module: lifecycle, `StateStoreObserver`, debouncer,
digest assembly, token estimation. Proposed split: `debouncer.rs`
(PendingView + channel), `tokens.rs` (pure estimators), `digest.rs`
(`landscape_digest` + `format_landscape`), with `manager.rs` remaining as the
public façade and trait implementor. Introduce a `ViewKind` enum so the
`VIEW_*` string constants and their match-like checks become type-safe.

~~**`sovereign-cli/src/atos_cmd.rs`** (2,673 lines)~~ — Done. Split into
`sovereign-cli/src/atos_cmd/` directory with twelve files per subcommand
family (`mod.rs` dispatcher + `args`, `stores`, `provision`, `milestone`,
`feature`, `spec`, `status`, `teardown`, `doctor`, `plugin`, `ab`). Largest
resulting file is `milestone.rs` at ~700 lines; all others under 450.

~~**`sovereign-atos/src/local.rs`** (1,183 lines)~~ — Done. Split into
`sovereign-atos/src/local/` directory: `orchestrator.rs` (struct + trait impl
+ inherent helpers + orchestrator-level tests) and `helpers.rs` (pure
charter-text helpers — `extract_id_and_title`, `compose_milestone_brief`,
`extract_milestone_stop_condition`, stop-condition marker stripping,
prior-digest / global-invariants preamble assembly) with its own unit tests.
`mod.rs` preserves the public surface (`pub use LocalAtosOrchestrator`).

### OCP: Registry patterns for pluggable dispatch

~~**`FieldModelEngine` domain dispatch**~~ — Done. Replaced with
`enrichment/domain_registry.rs` (`DomainRegistry`); `from_recipe` is now a
lookup, not a match cascade.

**`commonwealth-api/src/middleware/mod.rs`** — middleware discovery is a
stringly-typed match. Proposed: `MiddlewareRegistry { factories:
HashMap<&'static str, MiddlewareFactory> }` with a single `build_chain(ids:
&[&str])` entry point. Adding middleware becomes one `register` call.

**`sovereign-atos/src/report.rs`** — three sibling `render_milestone /
render_red_team / render_full` functions with near-parallel heading,
note-grouping, and markdown formatting. Propose a `ReportRenderer` trait with
one shared driver and three concrete impls; the existing free functions
become thin wrappers for back-compat.

### ISP: trait boundaries to preserve

`StateStore` is already decomposed (see §4.1); the sub-traits are live and
callers should prefer narrow bounds (`impl ConversationStore + MemoryStore`
over `impl StateStore`) in new code. No further splits proposed.

`LandscapeDigestProvider` and `StateStoreObserver` are both single-method /
tri-method traits; do not widen.

### DIP / SICP: data-driven templates

ATOS keeps the agent-facing instructions preamble, charter adversarial Q&A
catalog, and report heading strings baked into the Rust source. Proposed:
move to `sovereign-atos/assets/{atos_instructions.md,amend_questions.toml,
report_templates.toml}` (embedded with `include_str!`) so the data and the
code that walks it are separate concerns, and an operator can swap to a
`read_to_string` debug-build variant without recompiling to experiment.

### Glassbox observability

`KnowledgeView` cross-view match selection, ATOS charter edits, drift
detection line-level diffs, and red-team auto-spawn are all currently opaque
to the operator. Proposed: low-cost `tracing::debug!`/`tracing::info!` events
at decision points that let the operator answer "why did X happen?" from log
output without a debugger.

### Antifragile-routing — deferred follow-ups

PR1–PR4 shipped the full Commit / Propose / Ask dispatcher, desktop UX
(InterpretationBanner, ClarificationCard, NarrationChip, NextStepButtons),
real redirect-with-re-dispatch, and structural-signal capture into
`routing_log.was_redirected` + `routing_log.redirect_to`. Four follow-ups
remain tracked, none load-bearing for correctness:

1. **Retrieval caching across redirect.** `prepare_knowledge_query_plan`
   bundles retrieval + prompt-building. Splitting it into a cache-able
   retrieve half + an intent-specific build-request half would save ~200ms
   per redirect by reusing chunks instead of re-searching the corpus.
   Revisit when redirect-rate telemetry shows users redirect often enough
   for the delay to matter.
2. **Confidence-threshold calibration.** The `was_redirected` + `redirect_to`
   columns now accumulate from day one. A periodic job keyed on
   (coarse_intent, confidence_bucket) → redirect_rate can tune
   `ConfidenceThresholds` per-user when the signal volume warrants it.
   Signal capture lives in `Runtime::redirect_turn_stream`; the calibration
   job itself is future work.
3. **Clarification + implicit-acceptance signals.** Today only explicit
   redirects from the Propose banner produce a signal. Extending capture to
   cover (a) Ask-move resolutions via ClarificationCard clicks and (b)
   30s-no-redirect implicit acceptance on Propose turns would fill in the
   positive-signal side of the calibration input.
4. **Client-started structural telemetry.** Long-form trace of every
   `router:policy_applied` / `routing:redirected` / `routing:ask` event at
   `debug` level is sufficient today. A future structured-export sink
   (CSV/JSON to a log directory) would let users audit routing behavior
   without running `tracing_subscriber` by hand.

### OICP v0.3 — Specialization-aware routing rollout

**PR-A/B/C/D/E/F/G (shipped)** landed v0.3 end-to-end with
observation-adjusted routing, role-based user-facing vocabulary,
RTT-derived peer locality, and extension-hint governance
telemetry. PR-A introduced the protocol types in `oicp-types`;
PR-B wired claim-based ranking into all three schedulers + the two
advertisers + the runtime request builder, keeping the v0.2 surface
as a transitional fallback; PR-C deleted the v0.2 routing surface
(`Capability{Requirements}`, `ContextRequirements`,
`PerformanceRequirements`, `LatencyPreference`, `satisfies_required` /
`score_preferred`, `InferenceRequirements.{capabilities,context,performance}`
fields, `ProviderModel.capabilities`,
`OicpResponseMeta.{model_capabilities,degraded_capabilities}`,
`DegradedDetail`, the v0.2 fallback branches in all three schedulers,
the `OicpModelCache` module) and migrated all 8 skill TOMLs to the
v0.3 `capability_hint`/`latency_class` schema. PR-D added the
operational adjustment layer: `NodeObservations` +
`effective_affinity` / `load_penalty` / `locality_bonus` /
`cold_start_weight` in `oicp-types`; `BackendCandidate.observations`
+ `locality` in commonwealth-inference; `BackendEntry.observations`
+ `locality` in sovereign-inference; per-peer observation tracker +
`record_dispatch` / `record_success` / `record_failure` on
`MeshInferenceProvider`. Two new scenario test files
(`oicp_v03_observations.rs`) exercise thundering-herd, failing-node,
cold-start, and locality preferences.

The `Capability` enum + `CapabilityProfile` + `proficiency` +
`infer_hint_from_profile` remain as internal model-metadata
vocabulary (not on the wire).

PR-E shipped the user-facing vocabulary pass across both the
desktop (`SettingsPanel`, `DeveloperSetup`) and the CLI (`sovereign
setup` prompts, `sovereign` help text + runtime messages). Models
are now described by role, not by internal slot: **Quick responder**
(Fast slot — short fast-turnaround replies + routing), **Main
responder** (Primary slot — substantive work, lazy-loaded),
**Knowledge embedder** (Embed slot — retrieval vectorization).

PR-F piggy-backed a locality probe on the existing manifest fetch.
`sovereign-mesh::peer_inference::get_peer_manifest` now records
the RTT of its single HTTP round-trip alongside the manifest, and
`classify_rtt_ms` buckets the result into `Local` / `Near` / `Far`.
No extra probe traffic; LAN peers pick up their 1.05× locality
bonus as soon as the manifest cache warms.

PR-G added the extension-hint governance registry. `oicp-types`
exposes `ExtensionRegistry` + `ExtensionStats`; `MeshInferenceProvider`
owns one and taps it at two points — `select_peer` (every outgoing
request's `x:*` hint) and `get_peer_manifest` (every claim in a
fetched peer manifest). Operators can read a snapshot via
`extension_stats()`; a separate governance tool (future work) reads
the first/last-seen timestamps + request/advert counts to decide
which extensions merit promotion per spec §4.3.

PR-E2 shipped the **Code specialist** — an optional fourth role
that threads hint-aware dispatch *inside* `EmbeddedLlamaCpp`
instead of widening the `Speed` enum (which would have cascaded
through 33 files and 10+ `InferenceProvider` impls). The user-
facing surface is a fourth card in Settings → Models + an optional
prompt in `sovereign setup`. When configured, requests whose OICP
envelope carries `capability_hint = "code"` (as emitted today by
`research-analyst` and the `codebase-navigator` skill paths) are
dispatched to the Code specialist GGUF; everything else continues
to flow to the Main responder.

Mechanics: the Code specialist **shares the primary's lazy chat
mutex** — one of {Main responder, Code specialist} is resident at
a time, with a hot-swap on hint-switch. This trades ~5–30s reload
latency for a dramatically smaller memory footprint on mid-range
hardware (the alternative — two held-warm specialists — would
double VRAM pressure on Mac unified memory). A collective peer
that wants zero-swap code routing dedicates one node to code and
lets the mesh scheduler surface that peer via the new third
`ProviderModel` claim in `build_self_manifest`. The code claim
advertises `LatencyClass::Normal` (reflecting hot-swap TTFT, not
held-warm TTFT) so scoring lines up with reality.

**Knowledge embedder stays single-slot and explicitly named** —
it carries a cross-peer `EmbedModelInfo` contract that must not
be flattened into the chat-roles vector. The PR-E2 change
deliberately does not touch the embed plumbing; only the two
chat slots (Main + Code) share the lazy-load mutex. See §(cross-
peer interoperability) for why this asymmetry is load-bearing.

Trait surface: `InferenceProvider::code_model_id() -> Option<String>`
defaults to `None`, so the dozens of test stubs, remote providers,
and hybrid wrappers continue to compile unmodified. Only
`EmbeddedLlamaCpp` (and `MeshInferenceProvider` which forwards)
return `Some(...)`. The dispatch rules themselves live in a free
function `pick_slot(request, has_primary, has_code) -> SlotTarget`
so they can be table-tested without loading real GGUF weights
(`embedded::pick_slot_tests`, six rules locked).

Remaining step (hypothetical PR-E2.1, not scheduled):

1. **N-warm chat slots.** Extend the single lazy chat mutex into a
   role-keyed vector so a big-box operator with VRAM to spare can
   hold Main + Code + (future) Math all warm simultaneously. Touches
   `EmbeddedLlamaCpp` internals only — user-facing vocabulary and
   OICP claim shape are already in place. Not on the critical
   path; current users trade a one-time ~20s swap per hint switch
   for half the RAM pressure.
