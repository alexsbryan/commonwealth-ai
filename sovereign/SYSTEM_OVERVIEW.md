# Commonwealth AI — System Overview

A field guide for navigating the codebase. Describes what currently exists in
the workspace and where to find each piece. For design rationale see each
project's `ARCHITECTURE.md` / `IMPLEMENTATION_PLAN.md` / topical docs
(e.g. `corpus-engine/ENRICHMENT_V2.md`, `commonwealth/docs/oicp-v0.3.md`).

---

## 1. The Four Projects

```
commonwealth-ai/
├── oicp-types/          # OICP wire types — no other deps
├── corpus-engine/       # Knowledge layer (LanceDB + Tantivy) — no other deps
├── sovereign-recipes/   # Corpus recipe TOMLs + generated data (Wikipedia, SEP, …)
├── sovereign/           # Local AI assistant (CLI / desktop / server)
└── commonwealth/        # Mesh coordination daemon
```

| Project             | Role                                              | Depends on              |
|---------------------|---------------------------------------------------|-------------------------|
| `oicp-types`        | OICP v0.3 wire types + scoring helpers            | —                       |
| `corpus-engine`     | Acquire → extract → filter → chunk → embed → index | `oicp-types`            |
| `sovereign-recipes` | Pure data — recipe TOMLs + bundled assets         | —                       |
| `sovereign`         | Local agent runtime                               | `corpus-engine`, `oicp-types` |
| `commonwealth`      | Symmetric mesh daemon                             | `corpus-engine`, `oicp-types` |

Dependency direction is one-way. Sovereign optionally embeds Commonwealth
in-process via `sovereign-mesh` — the only place the two upper projects meet.

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

- **OICP** — declared in `commonwealth/docs/oicp-v0.3.md`; types in
  `oicp-types/src/lib.rs`; re-exported as `sovereign_core::oicp` and
  `commonwealth_core::oicp`. Same `serde` round-trip both sides.
- **`EmbedFn` / `InferenceFn`** — `corpus-engine` accepts these closures
  from any caller; each project provides its own implementation.

---

## 2. Workspace Map

### corpus-engine/

```
src/
├── lib.rs                    # Public API re-exports
├── engine/                   # CorpusEngine facade
│   ├── mod.rs
│   ├── ingest.rs             # acquire → extract → filter → chunk → embed → index
│   ├── expand.rs             # Filter-scope expansion + delta path + IVF-PQ rebuild
│   └── reindex.rs            # Per-file incremental re-index
├── recipe.rs                 # Recipe TOML schema (+ bundled fallback for offline registry)
├── registry.rs               # RecipeRegistry: bundled snapshot + remote fetch
├── types.rs progress.rs error.rs
├── safety.rs                 # robots.txt, rate limit, scope, UA
├── sharding.rs               # extract_shard / merge_shards
├── acquirers/                # bulk_download, huggingface, local_file
├── extractors/               # mediawiki/jsonl/html/csv/parquet/plaintext + code (tree-sitter)
├── chunkers/                 # paragraph, sentence, fixed, semantic, passthrough
├── filters/                  # DocumentFilter trait — pageview_rank, title_list, ComposedFilter
│   ├── mod.rs                #   FilterPipeline {Any|All}, normalize_title, doc_title_for_filter
│   ├── loader.rs             #   build_filter_pipeline + @bundled: key resolution
│   └── assets.rs             #   include_bytes! constants for bundled lists
├── index/                    # CorpusIndex (LanceDB + Tantivy)
│   ├── mod.rs                #   IndexMeta with ScopeMeta + FilterOverride
│   ├── create.rs search.rs write.rs enrichment.rs
├── enrichment/               # See ENRICHMENT_V2.md
│   ├── field_engine.rs       #   v1 — 5-phase Domain trait pipeline (legacy, still in use)
│   ├── domain_registry.rs    #   v1 dispatch
│   ├── domains/              #   philosophy, multi, personal, conversational, institutional + 4 stubs
│   ├── pipeline/             #   v2 atlas — Pipeline trait + ExemplarBank + PhaseCache
│   │   └── pipelines/        #     literary, literary_atlas, philosophy_atlas
│   └── atlas/                #   atom-graph storage (atoms.json, edges.json, …)
├── atlas_traversal/          # Query layer over atlas graphs
├── update/                   # Code/file watchers, delta updates, lint/test watchers
├── notes.rs features.rs      # NoteStore + ATOS FeatureStore (SQLite + FTS5)
├── plan_items.rs             # ATOS implementation-plan rows
├── project_docs.rs           # SOVEREIGN.md indexer for project_context tool
├── lint_results.rs test_results.rs   # Persisted watcher output
├── scip_graph.rs scip_export.rs scip_proto.rs  # SCIP call graph
├── design_signals.rs         # Structural parser over DESIGN.md
└── sovereign_config.rs       # .sovereign/sovereign.toml loader

assets/                       # (none; see build.rs)
build.rs                      # Copies sovereign-recipes/wikipedia/data/* into OUT_DIR
registry_snapshot.toml        # Bundled recipe catalog (include_str!)
xtask/                        # cargo xtask update-registry-snapshot
ENRICHMENT_V2.md              # Atlas pipeline plan of record
tests/
```

### sovereign/

```
crates/
├── sovereign-core/           # Traits, runtime, planner, executor, router, memory, OICP, model families
│   src/
│   ├── traits.rs             #   Trait boundaries (see §4.1)
│   ├── runtime.rs            #   Top-level orchestrator + landscape splice
│   ├── router.rs planner.rs executor.rs
│   ├── memory.rs context.rs query_session.rs
│   ├── skills.rs registry.rs # Skill loader + ToolRegistry
│   ├── model_family.rs models_manifest.rs
│   ├── observer.rs           #   StateStoreObserver (KnowledgeView triggers)
│   ├── health.rs health_monitor.rs
│   └── insight.rs gap.rs title.rs
│
├── sovereign-inference/      # llama.cpp (embedded), OpenAI-compatible (remote), hybrid w/ failover
│   ├── embedded.rs           #   Slot management (Quick / Main / Code / Embed)
│   ├── remote.rs hybrid.rs selector.rs
│   ├── health.rs router_circuit.rs
│   ├── hardware.rs gguf_validator.rs
│   └── json_grammar.rs       #   LLGuidance integration
│
├── sovereign-store/          # SQLite + Postgres + in-memory StateStore impls
│   └── migrations.rs         #   Schema, FTS5, soft-delete, sync columns
│
├── sovereign-tools/          # Built-in tools (per §4.4)
│   ├── search.rs knowledge.rs document.rs epistemic.rs enrich.rs
│   ├── web/ rag/ mcp/        #   Web (DDG/Brave/Tavily), user-doc RAG, MCP client
│   ├── code/                 #   24 code-intelligence tools (see §4.4)
│   ├── corpus/               #   Per-recipe install + parsers
│   ├── knowledge_view/       #   Landscape-digest assembly (manager/digest/cross_view/recipes)
│   ├── local_corpus/         #   Folder/Obsidian-vault flow
│   ├── index_validator.rs enrichment_checker.rs manifest.rs
│   └── document_*.rs
│
├── sovereign-atos/           # ATOS library (per §4.13)
│   ├── lib.rs charter.rs approval.rs report.rs session.rs
│   └── local/                #   LocalAtosOrchestrator + helpers
│
├── sovereign-cli/            # REPL + named subcommands (see §4.9)
│   src/
│   ├── main.rs setup_cmd.rs daemon_cmd.rs doctor_cmd.rs mesh_cmd.rs
│   ├── project_cmd.rs design_session.rs design_onboarding.rs
│   ├── plan_composer.rs phases.rs found.rs amend.rs
│   ├── atos_cmd/             #   provision/milestone/spec/feature/teardown/doctor/plugin/ab/status
│   ├── enrich_cmd/           #   build/extract/cluster/seed/atlas-* + sep_ingest/cascade
│   ├── tools_cmd/            #   `sovereign tools list/describe/call`
│   ├── chat_cmd/ bench_cmd/  #   REPL + benchmarking
│   ├── service_install.rs    #   launchd/systemd installation
│   ├── code_cmd.rs mcp_cmd.rs recipe_cmd.rs reflect_cmd.rs
│   ├── atos_plugin.rs        #   include_str! sovereign-atos.ts
│   └── util/                 #   dirs, prompts, status, urls, log_rotation, tracing_init
│   assets/sovereign-atos.ts  #   opencode plugin source (versioned)
│
├── sovereign-server/         # Axum REST + WebSocket, multi-tenant + approvals
│   └── src/{routes, ws, tenant, auth, approval, activity}.rs
│
├── sovereign-desktop/        # Tauri 2 + Svelte 5
│   src-tauri/                #   commands.rs, local_corpus_commands.rs, enrich_commands.rs, bootstrap.rs
│   src/lib/                  #   Svelte 5 components, stores (runes), routes
│
└── sovereign-mesh/           # In-process Commonwealth daemon embed
    ├── daemon.rs             #   EmbeddedDaemon (binds 9741 client + 9742 internal)
    ├── inference_adapter.rs  #   SovereignInferenceAdapter — peers fetch /oicp/v1/capabilities
    ├── peer_inference.rs     #   MeshInferenceProvider — wraps local inference, OICP-routes to peers
    ├── oicp_select.rs        #   Shared OICP scoring + RTT-derived locality
    ├── knowledge_client.rs   #   MeshKnowledgeClient — federate knowledge search
    ├── join.rs deep_link.rs persist.rs state.rs gossip.rs capabilities.rs
    ├── admin_http.rs mesh_http.rs mcp_router.rs project_http.rs
    ├── loopback_guard.rs     #   Loopback-only middleware on admin / mcp / mesh routers
    ├── auto_ingest.rs reindexer.rs supervised_task.rs
    └── projects.rs types.rs

skills/                       # 8 skills: research-analyst, epistemic-research, codebase-navigator,
                              #   code-review, collaborative-research, document-analyst,
                              #   personal-assistant, inner-work
data/corpora.toml             # Compiled-in corpus tier registry
models.toml                   # Per-hardware-profile model manifest
SYSTEM_OVERVIEW.md            # This file
```

### commonwealth/

```
crates/
├── commonwealth-core/        # Shared types
│   ├── ids.rs mesh.rs capabilities.rs model.rs scheduler.rs knowledge.rs
│   ├── ledger.rs ledger_store.rs
│   ├── oicp_registry.rs latency.rs config.rs
│   ├── model_aliases.rs glob.rs default_aliases.toml
│   ├── pipeline_aliases.rs default_pipelines.toml   # ATOS sovereign-coder pipeline
│
├── commonwealth-discovery/   # Membership, mDNS, gossip, latency probe, hardware, TLS, peering
│
├── commonwealth-inference/   # Scheduling + orchestration
│   ├── scheduler/            #   layer_assignment, plan_builder, knowledge_assignment, leader,
│   │                         #   oicp_select, oicp_cache, portfolio, usage_predictor, adaptive
│   └── orchestrator/         #   apply_shard_plan, ManagedProcess, HealthTracker, FaultDetector,
│                             #   GracefulDeparture
│   ├── inference_plan.rs plan.rs topology.rs tier_router.rs store_adapter.rs
│
├── commonwealth-api/         # HTTP servers (client 9741 + internal 9742 mTLS)
│   ├── server.rs state.rs openai_types.rs
│   ├── routes_inference.rs   #   /v1/chat/completions (+ pipeline-alias resolution)
│   ├── routes_knowledge.rs routes_status.rs routes_oicp.rs
│   ├── routes_internal.rs    #   gossip, scheduling, model/index transfer, knowledge fan-out, latency
│   ├── routes_apps.rs routes_app_internal.rs
│   └── middleware/           #   approval_gate, session_briefing, context_injector, tool_injector,
│                             #   artifact_surface (sovereign-coder pipeline stack)
│
├── commonwealth-knowledge/   # corpus-engine integration (mesh_corpus, shard_manager, embed_http,
│                             #   grounding, store_adapter)
├── commonwealth-app/         # Mesh-app platform (manifest, lifecycle, registry, proxy)
├── commonwealth-state/       # MeshStore — gossip-replicated SQLite KV w/ TTL GC
├── commonwealth-daemon/      # CLI entry + signal handling
└── commonwealth-test-harness/   # SimulatedMesh, SimulatedNode, MockLlamaServer

contrib/                      # install.sh, systemd unit, launchd plist
docs/oicp-v0.3.md             # OICP canonical spec
```

### sovereign-recipes/

```
registry.toml                          # Recipe catalog (schema_version 1)
wikipedia/recipe.toml                  # English Wikipedia (Layer 1 — Vital Articles L5 by default)
wikipedia/data/vital_articles_l5.txt   # ~51K curated titles (consumed via corpus-engine/build.rs)
wikipedia/scripts/                     # build_vital_articles.py, build_pageview_ranks.py
wikipedia-simple/recipe.toml           # Layer 0 — Simple English (~230K articles, separate corpus_id)
sep/recipe.toml                        # Stanford Encyclopedia of Philosophy
stackexchange/recipe.toml openalex/recipe.toml
gutenberg/recipe.toml crs_reports/recipe.toml
codebase/                              # Recipe scaffolding for the codebase indexer
```

---

## 3. corpus-engine — The Shared Knowledge Layer

Self-contained library between "raw source on the internet" and "ranked search
hits with provenance." Both upstream projects use it through the same public
API; neither knows the other exists.

### 3.1 Pipeline

```
Acquirer → Extractor → Filter → Chunker → Embedder → Index
                                          (caller-supplied EmbedFn)
```

Each stage is a trait. A **Recipe** TOML configures the whole pipeline.

| Stage      | Built-ins                                                          |
|------------|--------------------------------------------------------------------|
| Acquirer   | `bulk_download`, `huggingface_dataset`, `local_file`               |
| Extractor  | `mediawiki_xml`, `stackexchange_xml`, `jsonl`, `wikipedia_jsonl`, `wikipedia_structured`, `html`, `csv`, `parquet`, `plaintext`, `code` |
| Filter     | `pageview_rank`, `title_list`, composed via `[[filter]]` array (`Any` / `All`) |
| Chunker    | `paragraph`, `sentence`, `fixed`, `semantic`, `passthrough`        |
| Index      | `CorpusIndex` over LanceDB (IVF-PQ) + Tantivy FTS                  |

### 3.2 Storage

- LanceDB for vectors (IVF-PQ, memory-mapped from SSD)
- Tantivy for keyword full-text search
- One on-disk dir per corpus, identical schema for full or shard
- `_corpus_meta.json` is the authoritative metadata

```
~/.sovereign/indexes/
├── wikipedia/
│   ├── _corpus_meta.json
│   └── chunks.lance/{_versions, data, _indices, _latest.manifest}
└── stackexchange-shard-0-6200000/      # shard — same schema as a full index
```

`IndexMeta` carries `ScopeMeta { filter_descriptions, filter_signature,
expandable }` plus an optional `filter_override` so a corpus can be expanded
in place (relax filters → delta-ingest the additions → rebuild IVF-PQ).

### 3.3 The injection contract

`corpus-engine` never embeds or generates text itself.

```rust
pub type EmbedFn      = Arc<dyn Fn(&str) -> Pin<Box<dyn Future<Output = Result<Vec<f32>>> + Send>> + Send + Sync>;
pub type InferenceFn  = Arc<dyn Fn(&str) -> Pin<Box<dyn Future<Output = Result<String>> + Send>> + Send + Sync>;
```

| Caller       | `EmbedFn`                                | `InferenceFn` (enrichment)        |
|--------------|------------------------------------------|-----------------------------------|
| Sovereign    | wraps local Embed slot                   | wraps Main responder slot         |
| Commonwealth | `embed_http::http_embed_fn` → `/v1/embeddings` | mesh inference endpoint    |
| Tests        | zero-vector mock                         | canned-JSON mock                  |

Default expected embedding model: `qwen3-embedding-0.6b` (768 dims). Indexes
record the model in `_corpus_meta.json`; opening with a different model fails
with `Error::IncompatibleEmbedding`. The embed slot is a **cross-peer
interoperability contract** — nodes sharing a corpus must produce
bit-compatible vectors (`EmbedModelInfo` must match).

### 3.4 The three-operation sharding contract

| Operation                              | Effect                                          |
|----------------------------------------|-------------------------------------------------|
| `index_stats(corpus_id)`               | Total chunks, ID range, size on disk            |
| `extract_shard(corpus_id, range, dir)` | Build a new index containing only chunks in range |
| `merge_shards(dirs, dir)`              | Reconstitute a complete index from N shards     |

Shards are structurally identical to full indexes, so `CorpusIndex::search`
doesn't know or care which it operates on.

### 3.5 Filters and recipe scope

A `[[filter]]` array on a recipe expresses scope without forking corpus
identity. Filters operate on `ExtractedDoc` between extract and chunk; the
extractor's lazy iterator is wrapped, so resume positions count post-filter
documents.

```toml
# Wikipedia recipe — Core scope = Vital Articles L5 (~51K curated titles)
[[filter]]
type = "title_list"
list_file = "@bundled:vital_articles_l5"
```

`@bundled:` keys resolve via `filters/loader.rs` → `filters/assets.rs`, which
embeds the data with `include_bytes!(concat!(env!("OUT_DIR"), …))`. The build
script `corpus-engine/build.rs` copies source files from
`sovereign-recipes/wikipedia/data/` into `OUT_DIR` at compile time
(override via `CORPUS_ENGINE_DATA_DIR` for standalone clones).

Expanding a corpus: `CorpusEngine::expand_corpus(corpus_id, new_filters)`
relaxes the filter, runs ingest in delta mode (skipping `source_doc_id`s
already indexed), and rebuilds IVF-PQ. Search stays live during the rebuild.

### 3.6 Enrichment (optional)

Two coexisting systems. Both opt-in per recipe.

- **v1 (`enrichment/field_engine.rs`)** — five-phase pipeline (skeleton
  extraction → HDBSCAN clustering → alignment → fault lines → open
  questions). Domain trait + `DomainRegistry` for dispatch. Nine domains:
  `philosophy` (full), `multi` (Wikipedia), `personal` /
  `conversational` / `institutional` (KnowledgeView, see §4.12), and
  four stubs.
- **v2 atlas (`enrichment/pipeline/`)** — typed atom graph (7 atom types ×
  7 edge types). `Pipeline` trait + `PipelineRegistry` + `ExemplarBank`
  + `PhaseCache`. Three pipelines: `literary`, `literary_atlas`,
  `philosophy_atlas`. Atlas state stored at
  `~/.sovereign/indexes/<corpus>/atlas/`.

See `corpus-engine/ENRICHMENT_V2.md` for the v2 plan of record (status table,
landing-by-landing scope, schema validation targets).

### 3.7 Recipes and the recipe registry

Six-plus recipes ship in `sovereign-recipes` and are consumed via
`RecipeRegistry`:

- **Bundled snapshot** — `registry_snapshot.toml` is `include_str!`'d so the
  engine works fully offline.
- **Bundled fallback** — `recipe.rs::bundled_recipe_toml(id)` returns the full
  recipe TOML for snapshot entries when the live URL is unreachable. Tested.
- **Live refresh** — `RecipeRegistry::refresh()` pulls the latest
  `registry.toml` from GitHub.
- **Resolution order** — local override on disk → remote `toml_url` → bundled
  fallback. SHA-256 verified when the entry's `sha256` is non-empty.
- **`cargo xtask update-registry-snapshot`** refreshes the bundled snapshot.

### 3.8 Delta updates

`update/delta.rs` — `VersionManifest` per-document revision IDs;
`ManifestDiff::compute` produces additions/updates/deletions; three-phase
apply (delete → update → add); `_update_progress.json` for resume.

### 3.9 Safety

Hardcoded, not configurable per recipe: robots.txt compliance; 1s/domain rate
limit; UA `CorpusEngine/0.1 (+https://sovereign.dev/corpus-engine)`; crawl
scope enforced against seed URL domain; download size warnings at 1.5×
estimate.

### 3.10 Recipe authoring platform

The recipe schema is open: a domain expert (financial journalist, legal aid
attorney, grad student) writes a TOML and the engine runs it. Five generic
primitives turn the format into a platform; the matching agent tools let the
local LLM author recipes too.

**Generic acquisition + extraction**

- **`http_api` acquirer** (`corpus-engine/src/acquirers/http_api/`) — URL
  templating with `{name}` placeholders, four pagination strategies (offset /
  cursor / next-URL / page-number), JSONPath document-URL follow with bounded
  concurrency, token-bucket rate limit, custom headers / User-Agent.
  `_progress.json` journal for resume.
- **`[recipe.parameters]`** (`corpus-engine/src/recipe.rs`) —
  String/Int/Date/List parameters with defaults and required flags.
  `Recipe::resolve_parameters` validates user input; the resolved values
  stamp on a transient `resolved_parameters` field and interpolate into
  `[acquire]`. The CLI prompts; the desktop renders a form
  (`corpus_get_recipe_parameters`).
- **`html_sections` extractor** (`corpus-engine/src/extractors/html_sections.rs`)
  — multi-regex section extraction with a `MissReport` sidecar
  (`_section_misses.json`) so `recipe test` can show "section X missed in
  filing Y; nearby text: …; suggestion: …" without re-running the regex.

**Investigation enrichment pipeline**
(`corpus-engine/src/enrichment/investigation/`)

A typed-relationship graph runs as a parallel module to the atlas pipelines.
Recipe-author declares `[[enrichment.entity_types]]` and
`[[enrichment.relationship_types]]`; the LLM extract prompt is generated
from that schema (LLGuidance JSON-grammar constrained). Three built-in
graph-pattern detectors:

- `circular_flow` — petgraph DiGraph + Tarjan SCC + DFS-based simple-cycle
  enumeration with `min_entities` and edge-type filters.
- `role_overlap` — same pair of entities holding two roles (`investor =
  "investment.from"` AND `customer = "revenue.to"`).
- `threshold` — numeric attribute against 5 comparison ops.

Outputs land in `<index_dir>/<corpus>/investigation/{entities,
relationships, pattern_findings}.json`. CLI shim:
`sovereign enrich investigation build <id>` (extract → coalesce → detect)
and `… show <id>` (render findings).

**Lifecycle tooling** (`sovereign-cli/src/recipe_cmd.rs`)

- `sovereign recipe validate <path>` — schema, regex compile,
  URL-template placeholder cross-reference, `for_each` parameter resolution.
- `sovereign recipe test <path> [--params k=v…] [--params-file <json>]` —
  sample acquire / extract / chunk; section-miss reporting in the markdown
  report.
- `sovereign recipe publish <path> [--submit-pr]` — sha256 the TOML, write
  to `~/.sovereign/recipes/registry.toml` + `~/.sovereign/recipes/<id>/recipe.toml`,
  record a publish marker, print the upstream-PR template.
- `sovereign recipe list` — bundled + local-merged registry with `(local)`
  badge.

**Agent-callable tools** (`sovereign-tools/src/recipe_author/`)

Five Tool impls, each requiring `Permission::RecipeAuthoring`:

- `RecipeReadTool`, `RecipeWriteTool` — allowlisted to
  `~/.sovereign/recipes/`; refuse `..` traversal.
- `RecipeValidateTool`, `RecipeTestTool` — wrap the same
  `CorpusEngine::test_recipe` the CLI uses; return structured
  `{passed, errors[], warnings[], section_misses[]}` so the LLM iterates
  on `nearby_text` hints.
- `RegistryBrowseTool` — bundled + local list with `is_local` per row.

Registered in `sovereign-cli/src/main.rs:591`. Validation-only mode
(`sample_size=0` + no `--params`) skips parameter resolution so the agent
doesn't have to fabricate values just to run `validate`.

**Desktop "Add Knowledge Source"**
(`sovereign-desktop/src-tauri/src/recipe_commands.rs`)

- `corpus_browse_registry()` → reuses `commands::list_corpora` (registry +
  local merge, with installed status).
- `corpus_import_recipe(toml_text)` → validates, writes to
  `~/.sovereign/recipes/<id>/recipe.toml`, appends to local registry.
  Returns `{success, errors[]}` so the import dialog can surface validation
  failures inline (the recipe is NOT written when validation fails).
- `corpus_get_recipe_parameters(corpus_id)` → declared `[parameters]` block
  with kinds + defaults; drives the install-time form.
- `corpus_install_with_parameters(request)` → POST to
  `/internal/corpus/install` with the resolved parameter map. Daemon
  validates synchronously and rejects mismatched / missing required values
  with HTTP 4xx before spawning the ingest task.

TS bindings: `sovereign-desktop/src/lib/api.ts` (`corpusImportRecipe`,
`corpusGetRecipeParameters`, `corpusInstallWithParameters`).

**Daemon side**
(`commonwealth/crates/commonwealth-api/src/routes_internal.rs`)

`POST /internal/corpus/install` accepts `{corpus_id, parameters}` (parameters
default to `{}`). The daemon resolves the recipe (registry + bundled fallback +
local merge), runs `Recipe::resolve_parameters` synchronously, stamps via
`with_resolved_parameters`, and ingests via `CorpusSpec::Inline` so the
http_api acquirer can interpolate `{name}` placeholders during acquisition.
Mismatched parameters fail the install POST with a clear message instead of
silently producing an empty corpus three minutes later.

**Local registry merge**
(`corpus-engine/src/registry.rs`)

`RecipeRegistry::with_local_registry(path)` shadows upstream entries by id
with values from `~/.sovereign/recipes/registry.toml`. Resolution order:
local > live > bundled. `is_local_entry(id)` powers the `(local)` badge in
both the CLI list view and the desktop "Add Knowledge Source" panel.

**Publish nudge**
(`sovereign-cli/src/project_cmd.rs::compose_publish_recipe_nudge`)

`sovereign project audit` walks `~/.sovereign/indexes/*/investigation/
pattern_findings.json` and emits a one-time markdown nudge per locally-authored
recipe that produced findings but hasn't been published. Suppressed by
`~/.sovereign/published_recipes.json` (written by `recipe publish`) and
`~/.sovereign/dismissed_nudges.json` (written by
`sovereign nudge dismiss <id>`). Family + per-recipe dismissal:

```sh
sovereign nudge dismiss recipe-publish                    # all
sovereign nudge dismiss recipe-publish:sec-investigation  # one
```

---

## 4. Sovereign — The Local Agent

A single-machine local AI assistant. Runs as desktop, CLI, or HTTP server
against the same `Runtime`. No data leaves the machine unless the user opts
in to web search or a Commonwealth mesh.

### 4.1 Trait architecture

`sovereign-core/src/traits.rs`:

| Trait                     | Surface                                                     |
|---------------------------|-------------------------------------------------------------|
| `InferenceProvider`       | `complete`, `complete_stream`, `complete_stream_with_id`, `embed`, `embed_query`, `capabilities`, `code_model_id` |
| `Router`                  | `classify(message, ctx, tools) → RouterClassification`      |
| `Planner`                 | `plan(goal, context, tools) → Plan`, `replan(...)`          |
| `Tool`                    | `descriptor`, `execute`, `validate`, `retry_config`, `required_permissions` |
| `LandscapeDigestProvider` | `splice_landscape_digests(ctx, active_skill)`               |
| `ApprovalChannel`         | Human-in-the-loop tool approval (CLI / Tauri / Server / Auto) |
| `MeshKnowledgeSource`     | Fan-out knowledge search to mesh peers                      |
| `InsightStore` / `InsightSink` | Long-term insight extraction + persistence             |

`StateStore` is decomposed per ISP into focused sub-traits aggregated by a
single blanket impl: `ConversationStore`, `TaskStore`, `MemoryStore`,
`RoutingStore`, `DocumentStore`, `CorpusStateStore`, `BudgetStore`,
`PermissionStore`, `HealthStore`, `DocumentSessionStore`, `DocumentAssetStore`,
`InsightStore`. Callers narrow bounds to what they need.

### 4.2 Runtime data flow

```
User message
  → Router.classify           (Quick slot, two-pass coarse → refine)
       → RouterClassification { primary, alternatives, rationale, … }
  → decide_policy(classification, ConfidenceThresholds)   (pure fn)
       → RoutingPolicy { tier, move_kind: Commit | Propose | Ask, … }
  → SessionStore.begin → QuerySession (in-memory; CancellationToken-bearing)
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

`Plan` is a flat JSON DAG (`steps`, `edges`). `StepKind`: `Reason`, `Tool`,
`UserInput`, `Branch`, `ReasonWithTools`. Planner emits `[sample:N:method]` /
`[eval:name]` annotations; the executor parses them into config.

The router emits **facts**; the runtime applies **policy**. Splitting them
keeps classification testable without a model and lets thresholds calibrate
without touching the trait.

### 4.3 Inference

`sovereign-inference/embedded.rs` wraps `llama-cpp-2` with a lazy-loaded slot
system:

| Role (user-facing)  | Slot     | Purpose                                  | Typical model       |
|---------------------|----------|------------------------------------------|---------------------|
| Quick responder     | Quick    | Routing, working-memory compression      | Qwen3 0.6B–1.7B     |
| Main responder      | Main     | Planning, synthesis, evaluation          | Qwen3.5 4B/9B/27B   |
| Code specialist     | Code     | Hint-routed code work; shares Main mutex | DeepSeek-Coder etc. |
| Knowledge embedder  | Embed    | Vector embeddings                        | Qwen3-Embedding 0.6B/4B |

Code specialist shares the Main slot's lazy chat mutex (hot-swap on hint
switch); only one of {Main, Code} resident at a time. The Embed slot stays
on its own `Arc<EmbedSlot>` (cross-peer contract; never folded into chat).

`models.toml` — five hardware profiles (`cpu_only`, `low_mem`, `default`,
`high`, `very_high`) each declare `repo`, `file`, `family`, `quant`,
`size_gb`, `thinking`. Per-slot `quirks_override` tunes family defaults.

`model_family.rs` encodes per-family quirks: `ThinkingControl`, sampling
defaults, `EmbedQuirks` (`PoolingStrategy`, `NormalizationStrategy`,
`query_instruction` for asymmetric retrieval).

`hybrid.rs` is a multi-backend `InferenceProvider`: `BackendSelector` →
`CapabilityAwareSelector` (OICP scoring) → `PrioritySelector` fallback;
per-backend `HealthTracker` (EWMA latency α=0.3, 3-strike availability);
background loop refreshes OICP manifests; up to 2 retries.

`remote.rs` is OpenAI-compatible (vLLM, Ollama, llama.cpp server, TGI,
Commonwealth).

### 4.4 Tools

Local tools (`sovereign-tools/src/`):

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

Code-intelligence MCP server (`sovereign project serve` ad-hoc, `sovereign
daemon` long-running). 24 tools across eight groups:

| Group                 | Tools                                                     |
|-----------------------|-----------------------------------------------------------|
| Code index (LanceDB)  | `symbol_lookup`, `code_search`, `recent_changes`          |
| SCIP call graph       | `find_callers`, `find_callees`, `blast_radius`            |
| Lint watcher          | `lint_status`, `get_lint_output`                          |
| Test watcher          | `test_status`, `run_tests`, `get_run_output`              |
| Working notes         | `write_note`, `read_notes`, `delete_note`                 |
| ATOS feature lifecycle| `provision_feature`, `archive_feature`, `read_note_by_id`, `promote_note`, `read_note_digest`, `record_atos_event`, `write_redteam_finding` |
| Project context       | `project_context`                                         |
| Session reflection / doc health | `session_reflection`, `check_doc_paths`         |

A filesystem watcher (`CodeWatcher`) re-indexes modified files and marks
them stale in the call graph. Staleness levels carry calibrated confidence:
`None` / `SomeCallSitesMayBeStale` / `GraphIsAging` / `GraphIsStale` /
`LanguageNotIndexed`.

`blast_radius` performs BFS over the call graph and appends a `macro_hints`
text scan for symbol references inside macro invocations and attributes that
SCIP doesn't capture.

### 4.5 State

`sovereign-store` provides three `StateStore` impls behind one trait:
`SqliteStateStore` (default), `PostgresStateStore` (deadpool +
tokio-postgres), `MemoryStateStore` (tests). Schema in `migrations.rs`:
`conversations` + `messages` (FTS5), `tasks`, `memories`, `documents`,
`corpus_states`, `routing_log`, `search_budget`, `permissions`. Every record
carries a Lamport `version`; soft-deletable rows have `deleted_at`. Two
stores can union-merge without schema migration.

### 4.6 Memory

- **Working memory** — compressed every message via `memory::compress_working_memory` (Quick slot, ≤200 tokens) into `{ current_goal, facts, active_documents }`.
- **Long-term memory** — extracted at conversation end. Each `Memory` has `confidence`, `created_at`, `last_used`. FTS5 retrieval.
- **Decay** — exponential monthly (default 10%, overridable per skill). Pruned below `prune_threshold`.
- **Routing-correction memory** — `RoutingCorrection { message_hash, classified_as, was_correct }` fed back into the router prompt as "avoid these mistakes."

### 4.7 Skills

TOML files in `sovereign/skills/`. `SkillRegistry` merges routing hints,
planner templates, prompt overrides, memory rules, and OICP requirements
into the runtime. Bundled: `research-analyst`, `epistemic-research`,
`codebase-navigator`, `code-review`, `collaborative-research`,
`document-analyst`, `personal-assistant`, `inner-work` (privacy =
`local_only`).

Skill TOML structure:

```toml
[skill]      id, name, version, description
[routing]    trigger_phrases, default_intent, min_confidence
[[planner.templates]] name, trigger, steps   # supports [sample:N:method] / [eval:name]
[tools]      required, optional, tool_settings
[prompts]    synthesis override
[memory]     extract_prompt_addendum, confidence_decay_per_month, prune_threshold
[inference]  privacy, capability_hint, latency_class, min_context_tokens, max_output_tokens
```

Skills carry `signature` / `signed_by` and a derived `TrustLevel
{ CommunityReviewed, AuthorSigned, Unsigned }`.

### 4.8 OICP (v0.3)

Spec: `commonwealth/docs/oicp-v0.3.md`. Types: `oicp-types/src/lib.rs`,
re-exported as `sovereign_core::oicp` and `commonwealth_core::oicp`.

**Wire types:**

- `CapabilityHint` — validated tag. Standardized: `general`, `code`. Open-vocabulary specializations use `x:<tag>`.
- `LatencyClass` — `Fast`, `Normal` (default), `Extended`.
- `CapabilityClaim { hint, latency_class, max_context, max_output, affinity }` — one claim per kind-of-work a node serves well.
- `InferenceRequirements { oicp_version, capability_hint, latency_class, context_tokens, max_output_tokens, privacy, request_id }`.
- `ProviderManifest` — what a backend advertises (with `knowledge` + `federation` sections).
- `ProviderModel { id, base_model, quantization, context_tokens, status, size_gb, claims }`.
- `ShardingPrivacy` — `LocalOnly` (default), `MeshAllowed`.

**Internal model-metadata vocabulary** (off-wire): `Capability`,
`CapabilityProfile`, `proficiency()`, `infer_hint_from_profile` — used by
the runtime to translate model descriptions into `CapabilityHint` at
advertisement time.

**Three schedulers, one shared scoring pipeline:**
`commonwealth-inference::scheduler::oicp_select`,
`sovereign-inference::selector::CapabilityAwareSelector`,
`sovereign-mesh::oicp_select` all call
`score_claim_for_request(&CapabilityClaim, &InferenceRequirements)` then
fold in operational adjustments (helpers in `oicp-types`):

- **Hint match** — exact `1.0`; specific request + general claim `0.5`; general request + specific claim `0.0`.
- **Context / output** — hard gate.
- **Latency class** — `1.0` exact / `0.8` adjacent / `0.5` two-class gap.
- **Affinity** — clamped `[0.0, 1.0]`, final multiplier.
- **Observed health** — `effective_affinity` blends self-reported with rolling failure rate; trusts claim at zero samples, fully observation-driven past 50 samples.
- **Load penalty** — hyperbolic `1 / (1 + 0.05 * in_flight)`.
- **Locality bonus** — `Local` 1.15× / `Near` 1.05× / `Far` 1.0×; classified from manifest-fetch RTT (`<5ms` → `Local`, `<25ms` → `Near`).
- **Cold-start ramp** — new nodes start at `0.7×` weight, ramp to `1.0×` over 20 observations.
- **Throughput factor** — `[0.3, 1.0]` from observed token-generation rate (≥5 samples) or, in its absence, a benchmark-derived estimate scaled by model-size ratio. Returns neutral `1.0` for peers with neither signal so the change is wire-tolerant. Reference rate is 20 tok/s; the floor preserves last-resort routability for slow peers.
- **Inference availability** — gossiped clamped `[0.2, 1.0]`.

Observations are local per scheduler, never advertised between nodes.

**Benchmark advertisement.** Each node runs a one-shot probe at startup
(re-runs when `HardwareProfile` fingerprint changes) measuring prompt
processing + token generation against a fixed prompt. The result rides
`NodeCapabilities.benchmark` (`Option<BenchmarkResult>`, serde default
so older peers ignore) and feeds the throughput-factor extrapolation
when a peer hasn't accumulated observation samples yet. Probe lives in
`sovereign-inference/src/benchmark.rs`; observation pipeline (TTFT +
tg_tok/s EWMA, α=0.3) is wrapped around streaming completions in
`sovereign-mesh/src/peer_inference.rs::ThroughputObservedStream`.

**Advertisers:** `commonwealth-api/routes_oicp.rs::synthesize_default_claim`
(one claim per `ModelInfo`); `sovereign-mesh/inference_adapter.rs`
synthesises slot-claims (Quick → `Fast`, Main → `Normal`) and an optional
Code-specialist claim (`code` hint, `Normal`, affinity floor 0.5 for
filename-signalled coders).

**Extension governance:** `MeshInferenceProvider` carries an
`ExtensionRegistry` (in `oicp-types`) that passively records `x:*` hints
seen on outgoing requests and incoming claims. Consumed by an external
governance review process (spec §4.3); **not** the scheduler.

### 4.9 Frontends

| Frontend            | Purpose                                                                  |
|---------------------|--------------------------------------------------------------------------|
| `sovereign-cli`     | Interactive REPL + named subcommands: `setup`, `project` (init/design/plan/charter/found/amend/phase/audit/serve/refresh/install-hooks), `atos` (provision/start-milestone/end-milestone/spec/teardown/doctor/install-plugin), `daemon` (long-running, owns :9741), `doctor`, `mesh`, `corpus`, `code`, `mcp`, `recipe`, `tools`, `reflect` |
| `sovereign-server`  | Axum REST + WebSocket on configurable port; multi-tenant via `tenant.rs`; SSE + WS streaming; server-side `ApprovalChannel` w/ `/v1/tasks/{id}/approve` |
| `sovereign-desktop` | Tauri 2 + Svelte 5; setup wizard, chat w/ streaming + provenance, knowledge management (`KnowledgeStatus`, `CorpusProgressBanner`), skill manager, mesh UI, `sovereign://` deep-link handler, system tray |

CLI flags: `--model`, `--primary-model`, `--data-dir`, `--skills-dir`,
`--router`, `--ingest`, `--brave-api-key`, `--tavily-api-key`,
`--no-knowledge-view`. `project init` prompts for AI-assistant harness
(Claude Code / opencode / both / skip) and writes `.opencode/config.json` +
`AGENTS.md` for opencode and installs the ATOS opencode plugin.

The daemon (`daemon_cmd.rs::run`) rotates its own logs at startup via
`util::log_rotation` — copy-truncate, 10 MiB cap, 5 backups, 30-min sweep
loop; preserves the inode for launchd-held FDs.

### 4.10 Deep links

`sovereign-mesh/deep_link.rs` parses `sovereign://create?name=<name>` and
`sovereign://join?key=<key>` (with relay hints for NAT traversal). The
desktop app registers as the system handler.

### 4.11 Knowledge integration

Sovereign hands `corpus-engine` an `EmbedFn` that wraps its local Embed
slot, plus an optional `InferenceFn` for enrichment. `data/corpora.toml` is
the manifest the desktop uses for tier-driven install.

**Layered Wikipedia** (recipe `wikipedia` + `wikipedia-simple`):

- Layer 0 — `wikipedia-simple` (Simple English, ~230K articles, separate corpus_id). Hidden from the UI's main list; bundled with the main Wikipedia install.
- Layer 1 — `wikipedia` Core scope. Single `[[filter]] type = "title_list"` against Vital Articles L5 (~51K curated titles). Indexes in 5–8 min on M-series.
- Layer 1+ — Full Wikipedia. Same corpus_id; remove the filter via `expand_corpus`. Delta-ingests the additions; rebuilds IVF-PQ; search stays live.

The desktop `KnowledgeStatus` panel shows one row for "Wikipedia" — the
two-layer setup is hidden behind elegant install / expand UX.

### 4.12 KnowledgeView — Landscape digest assembly

Splices short structured summaries of the user's own world into the system
prompt before each turn. Three views:

| View                   | Source (StateStore)                                  | Domain (enrichment) |
|------------------------|------------------------------------------------------|---------------------|
| `personal-knowledge`   | `memories` (confidence > 0.2, not deleted)           | `personal`          |
| `conversation-history` | `conversations + messages`, 180-day window, excludes `local_only` skills | `conversational` |
| `institutional-notes`  | `notes` (decisions, invariants, todos, redteam findings — not reflections) | `institutional` |

Each view runs the v1 enrichment pipeline and writes `field_skeleton.json`
to `~/.sovereign/indexes/<view>/`. Before each message,
`LandscapeDigestProvider::splice_landscape_digests` formats per-view
markdown blocks within token budgets (300 / 200 / 100), computes cross-view
**resonance** (cosine ≥ 0.75, ≤5 matches per digest, phrased tentatively),
and splices the result into `ConversationContext`.

**Structural privacy invariants** (enforced in code):

- All three recipes hardcode `scope = "local"`, `mesh_sharing = false`, `query_sharing = false`.
- The conversational acquirer applies a `WHERE skill_id NOT IN (<local_only_ids>)` clause **at ingest time**.
- When the active skill is `local_only`, the splice suppresses conversational + institutional + cross-view, leaving only personal.

Disable paths: `--no-knowledge-view`, `[knowledge_view] enabled = false` in
`sovereign-server.toml`, the desktop Settings toggle. Disabled = manager
never instantiated.

### 4.13 ATOS — Agent Task Orchestration System

Two-layer scaffolding stored under `.sovereign/`:

- **Project layer** — single charter governs the repo. Subcommands: `project init` / `design` / `plan` / `charter` / `found` / `amend [design|charter]` / `phase pass N` / `audit`.
- **Feature layer** — one charter per feature, nested. Subcommands: `atos provision` / `start-milestone` / `end-milestone` / `spec diff` / `spec accept` / `teardown` / `doctor` / `install-plugin`.

Storage: `FeatureStore` (features, milestones, runs, tool events) and
`NoteStore` extended with kinds `deviation`, `redteam_finding`,
`postmortem_pointer` and a `NoteScope` (`Global | Feature | Session`).
Both share a SQLite connection.

**Drift detection.** Every agent turn recomputes SHA-256 of the feature
`spec.md` against the recorded approval hash. Mismatches **warn, not block**
— next turn's preamble carries the deviation. Approval source is git
history (walking commits that touch the spec) or a Commonwealth
`atos-approvals` MeshStore app, so force-push doesn't silently erase it.

**The `commonwealth/sovereign-coder` pipeline.** Defined in
`commonwealth-core/src/default_pipelines.toml`; resolved via
`PipelineAliasTable` on `/v1/chat/completions`. Middleware chain:
`approval_gate → session_briefing → context_injector → tool_injector →
artifact_surface`. The context injector prepends `<atos-instructions>`,
charter frame (invariants + current phase), scoped notes digest (Global +
Feature), and the spec body. A paired `sovereign-red-team` pipeline uses
`read_only_enforcer + context_injector (invariants-only)` and is
auto-spawned after the final milestone passes when the charter carries
`**Red team:** auto`.

**opencode plugin.** Source at `sovereign-cli/assets/sovereign-atos.ts`,
embedded via `include_str!` with a `// sovereign-atos-version: X.Y.Z`
header. Injects `X-Feature-Id` (from current branch's feature dir) and
`X-Session-Id` so the daemon knows which spec to splice.
`sovereign atos doctor` compares installed plugin to CLI binary version.

### 4.14 Local Corpora — Folder Drop + Obsidian Vault

Two flows in **Settings → Local Knowledge** — "Drop or browse a folder"
(PDFs + TXT) and "Connect Obsidian vault" (markdown) — share the same
`sovereign-tools/src/local_corpus/` machinery (config, pre_scanner, humanise,
extract_stage, progress, excerpt, clusterer, preview, frontmatter, writeback,
git, manager). Differ only in configuration and extension points.

Architectural invariants (test-pinned):

- **Snapshot before any write.** Atomic JSON snapshot under `~/.sovereign/vault-snapshots/{corpus_id}/` *before* the first file is touched. Lives outside the vault to avoid self-ingest.
- **`<namespace>/*` is inviolable.** Only `<namespace>/` tags and `<namespace>_*` frontmatter keys are added/modified/removed. Every other key/tag round-trips at value level.
- **Rollback is idempotent.** Deleted-since-snapshot files are re-created from the snapshot payload, not reported as errors.
- **pdf-extract panics are caught.** `safe_extract_pdf_text` runs `pdf_extract::extract_text` inside `catch_unwind` so a DeviceN colour-space panic gets classified as `Corrupt` instead of taking down the `spawn_blocking` task.

Resume-on-relaunch: `CorpusEngine::ingest` checkpoints to
`_source_manifest.json` on every flush; `lc_incomplete_jobs` surfaces
non-`Complete` corpora; the ResumePrompt re-invokes `lc_ingest`.

---

## 5. Commonwealth — The Coordination Daemon

A symmetric daemon. Every node runs the same binary; no master. Members
find each other via mDNS on the LAN or transitively over a VPN
(Tailscale/WireGuard) and converge on shared state via gossip.

In one sentence: translates "complete this chat with model X" into a plan
that spawns `llama-server` on one node and `rpc-server` on others, holds
the OpenAI-compatible HTTP endpoint open, and keeps the plan healthy as
nodes come and go.

### 5.1 Discovery and membership

- **Join keys** — `cwth-XXXX-XXXX-XXXX`. `membership::generate_join_key` stores BLAKE3 hash, discards plaintext. `verify_join_key` is constant-time. First node calls `init_mesh`; subsequent nodes call `accept_join`.
- **mDNS** — `_commonwealth._tcp.local` advertising `node_id`, `mesh_id`, `name`. `MdnsDiscovery::browse` populates `DiscoveredPeer`.
- **Gossip** — 10 s epidemic loop, 2–3 random peers per round. Three-phase digest/delta/response. Conflicts: timestamp LWW. Payloads: `MemberState`, `InferencePlan`, `KnowledgePlan`, `LedgerEntry`, `MeshConfig`.
- **Latency probing** — UDP RTT every 30 s, magic bytes `CWLP`, EWMA α=0.3. `LatencyMatrix` shared via gossip.
- **Hardware detection** — `discovery/hardware.rs` tries `nvidia-smi`, then `rocm-smi`, then Metal.
- **TLS** — `tls.rs` generates per-session certs with `rcgen`; pinned on the internal API.
- **Mesh peering** — `peering.rs`; two `PeerTrustLevel`s: `ModelAndKnowledgeSharing`, `Full`.

### 5.2 Scheduling

`commonwealth-inference/scheduler/` is a pure-functional layer over
gossiped state. A deterministic per-decision leader (lowest `NodeId`)
prevents thrash without consensus.

| Module                   | Algorithm                                                    |
|--------------------------|--------------------------------------------------------------|
| `layer_assignment.rs`    | Proportional VRAM, contiguous ranges per node, topology-aware ordering, privacy-aware entry-node preference |
| `plan_builder.rs`        | `build_shard_plan`, `build_inference_plan`, `estimate_performance` (TPS / TTFT) |
| `knowledge_assignment.rs`| Greedy by free storage; whole-corpus if it fits, else `ChunkRange` split; respects per-corpus `mesh_sharing` |
| `oicp_select.rs`         | Shared OICP scoring (see §4.8)                              |
| `oicp_cache.rs`          | Hashes `CapabilityRequirements` to `(ModelId, score)` keyed by portfolio version |
| `portfolio.rs`           | `ModelPortfolio` with `ModelTransition` state machine; `SWAP_THRESHOLD = 0.3` |
| `usage_predictor.rs`     | `(weekday, hour, CapabilityCategory)` request counts → preemptive loading |
| `adaptive.rs`            | Adaptive scheduler hooks                                     |

### 5.3 Orchestration

`commonwealth-inference/orchestrator/`:

- `Orchestrator::apply_shard_plan` spawns `llama-server` on the entry node (sequential allocation from `next_llama_port`) and `rpc-server` on remote nodes holding layer subsets.
- `ManagedProcess` tracks `id`, `kind`, `state` (`Starting | Running | Unhealthy | Failed | Stopped`), `pid`, `child`, `listen_address`, `spawned_at`. Graceful SIGTERM with timeout, then SIGKILL.
- `HealthTracker` polls every 5 s (HTTP for llama-server, TCP for rpc-server); 20-sample latency window; `Unresponsive` after 3 consecutive failures. Statuses: `Healthy`, `Degraded { reason }`, `Unresponsive`, `Dead`, `Unknown`.
- `GracefulDeparture` — 30 s countdown state machine (`Announced → Rebalancing → Draining → Complete`).
- `FaultDetector` collapses health changes into `FaultEvent`s.

### 5.4 HTTP API

Two listeners, two trust domains.

**Client API — :9741, no mTLS, binds 0.0.0.0** (federated inference needs
peer reachability)

| Path                          | Notes                                                  |
|-------------------------------|--------------------------------------------------------|
| `POST /v1/chat/completions`   | OpenAI-compatible. Routing: OICP → exact name → glob alias → pipeline alias → default. `LocalOnly` privacy → 400. |
| `GET  /v1/models`             | Loaded models w/ capabilities and performance estimates|
| `POST /v1/knowledge/search`   | Determines target corpora, fans out to shard nodes, merges, reranks |
| `GET  /status`                | Comprehensive node/mesh/inference/knowledge summary    |
| `GET  /oicp/v1/capabilities`  | Provider manifest + federation info                    |
| `/v1/mesh/*` `/v1/admin/*` `/mcp/*` | **Loopback-only** (router middleware + per-handler `enforce_localhost`) |

**Internal API — :9742, mTLS**

| Path                                | Purpose                          |
|-------------------------------------|----------------------------------|
| `POST /internal/gossip`             | Gossip exchange                  |
| `POST /internal/scheduling/intent`  | Scheduling decision notification |
| `POST /internal/scheduling/plan`    | New shard plan distribution      |
| `POST /internal/model/transfer`     | Model file transfer (peer-to-peer)|
| `POST /internal/index/transfer`     | Corpus shard upload (push)       |
| `GET  /internal/index/serve`        | Corpus shard download (pull) — used by `coordinate_merge` |
| `POST /internal/knowledge/search`   | Inter-node shard query (fan-out target) |
| `GET  /internal/latency/probe`      | Latency probe response           |

The loopback guard is defended in three layers: router-level
`from_fn(loopback_only)` middleware, per-handler `ConnectInfo` extraction,
and a pinned listener-shape test
(`admin_http::tests::loopback_guard_works_under_production_listener_shape`).
The listener must use `.into_make_service_with_connect_info::<SocketAddr>()`
in `daemon::start_daemon` — bare `axum::serve` leaves `ConnectInfo` absent
and the guards fail closed for *every* caller.

### 5.5 Knowledge

`commonwealth-knowledge` wraps `corpus-engine`:

- `MeshCorpusManager` — install / list / remove
- `ShardManager` — `prepare_shards` / `install_received_shard` / `consolidate_shards`
- `embed_http::http_embed_fn` — POSTs to `/v1/embeddings` (default `qwen3-embedding-0.6b`); how a node without a local embed model still ingests via the engine
- `grounding.rs` — `GroundingConfig { enabled, corpora, max_chunks, max_context_tokens, min_relevance, citation_instructions }` + `search_for_grounding` + `format_knowledge_context`

### 5.6.2 Desktop integration

`sovereign-desktop/src-tauri/src/mesh_commands.rs` exposes four
new Tauri commands for the Mesh Health UI:

- `mesh_get_contributions` → `Vec<NodeContributionsDto>` (per-peer
  dimensional view).
- `mesh_set_peer_preference(node_id, multiplier, reason)` → `()`.
- `mesh_clear_peer_preference(node_id)` → `bool`.
- `mesh_list_peer_preferences` → `Vec<PeerPreferenceDto>`.

Local mode talks to the in-process `EmbeddedDaemon`'s `AppState`
(via the new `app_state()` accessor); Attach mode currently
returns an empty list / "unsupported" error — the daemon doesn't
yet expose these over HTTP, which is captured as a TODO in the
command body.

`MeshSettings.svelte` consumes the four commands above:
the legacy `contribution_level` 0–5 score and `MeshContributionSummary`
card are gone, replaced by per-peer **Inference / Knowledge / Network**
blocks (no totals, no ranking — incommensurable per spec §2.2). Each
non-self peer row exposes a "Serve this peer at: [N%]" `<details>`
control with a slider clamped to the same `(0.0, 1.0]` range the Rust
constructor enforces, plus an optional reason field that stays local.

End-to-end coverage: `tests/e2e/specs/mesh-health.spec.ts` runs **9
specs** — six pin the Tauri command contract (response shape, the
structural multiplier rejection, list/clear semantics) and three
mount the real Svelte tree to assert the dimensional layout, the
slider→`mesh_set_peer_preference` dispatch, and a chaos invariant
(a phantom contribution row for an unknown peer renders zero ghost
rows and triggers no `pageerror`, mirroring the
`feedback_chaos_testing_pattern.md` discipline).

### 5.6.1 Peer preferences (Ostrom sanctions)

`commonwealth-state::peer_preferences` is the local-only,
gossip-excluded store of per-peer affinity multipliers. An operator
sets a preference (clamped at construction to `(0.0, 1.0]` per
ARCH_PRINCIPLES §7.1) via `commonwealth peer-preference set <node>
<multiplier>`; the manifest endpoint at `/oicp/v1/capabilities`
reads `X-Node-Id` from the inbound request, looks up any matching
preference, and multiplies every advertised `CapabilityClaim.affinity`
before serialization. The penalized peer's scorer sees lower
affinities and naturally routes elsewhere — the sanction is
expressed structurally, never communicated as a distinct signal.

`MeshStore::all_entries_for_gossip` filters out the
`peer_preferences` `app_id` so private adjustments cannot leak. The
invariant is pinned by tests in both
`commonwealth-state/src/peer_preferences.rs::tests::gossip_excludes_peer_preferences_app_id`
and
`commonwealth-state/src/store.rs::tests::all_entries_for_gossip_excludes_peer_preferences_namespace`.

`sovereign-mesh::peer_inference::MeshInferenceProvider` adds the
`X-Node-Id` header on every manifest fetch via the new
`PeerEndpointSource::local_node_id` accessor; manifest-fetches
without an identifiable requester serve unmodified affinities.

### 5.6 Dimensional contribution ledger

`commonwealth-core::contributions` defines the append-only event
log (`LedgerEvent` + variants `InferenceServed`, `InferenceReceived`,
`KnowledgeQueryServed`, `ShardTransferred`, `StorageSnapshot`) and
the pure aggregation function that collapses an event stream into
per-node `NodeContributions` (separate `InferenceActivity` totals
for served vs. consumed, plus `corpora_hosted` with `is_sole_host`
and bytes_served/bytes_received).

**Write sites — wired:**

- `InferenceServed` — `routes_inference.rs::serve_local_non_stream`
  + `serve_local_stream` (cross-mesh requests identified by
  `X-Node-Id`).
- `InferenceReceived` — sovereign-mesh
  `peer_inference::ThroughputObservedStream::Drop` (peer-routed
  streams that yielded ≥1 chunk).
- `KnowledgeQueryServed` — `routes_internal::knowledge_search` (one
  event per corpus that contributed chunks to the response).
- `ShardTransferred` — `commonwealth-knowledge::ShardManager::coordinate_merge`
  on each successful peer-shard pull during merge. Emitted by the
  merge leader (the puller) on behalf of the peer that shipped the
  bytes — the peer never observes the transfer completing, so the
  schema carries an explicit `from_node` and the aggregator's
  pull-emission special case credits `bytes_served` to that node
  rather than the emitter. The legacy push site
  (`ShardManager::stream_index`) keeps its emission too, but has
  no production callers today.
- `StorageSnapshot` — emitted by `run_storage_snapshot_loop`,
  spawned in TWO places: `commonwealth-daemon::main` (alongside
  `RetentionGc`) and `sovereign-mesh::EmbeddedDaemon::start_daemon`
  (alongside the gossip loop). Without the sovereign-side spawn
  the in-process desktop daemon never emits snapshots — only the
  CLI daemon would, leaving the dimensional ledger blank in
  desktop-only meshes. The first tick fires immediately so a
  freshly-restarted daemon emits one snapshot at boot.

`X-Node-Id` parsing is centralized in
`commonwealth-api::headers::parse_x_node_id` so every handler
stamps the same way. Local-origin requests (no header) skip
emission per spec §10 — the dimensional ledger is intra-mesh-only.

Storage lives in `commonwealth-state::ContributionEmitter`, which
writes events into the existing gossip-replicated `MeshStore` under
`app_id = "contributions"`. Every node aggregates the same event
stream locally and arrives at identical `NodeContributions` —
there is **no `balance` field, no exchange rate, and no ranking**
(the units are intentionally incommensurable per the Mesh Health
design).

`FairnessPolicy`, `FairnessConfig`, `NodeBalance`, and the old
`LedgerEntry` machinery were deleted in the same change — none of
them had any production write sites. Mesh-level "fairness" is now
expressed as per-peer affinity preferences (§5.7, commit 3) rather
than a policy enum.

### 5.7 Test harness

`commonwealth-test-harness`:

- `SimulatedMesh` — orchestrates many `SimulatedNode`s in-process, each with its own `AppState` and HTTP listeners on random ports.
- `SimulatedNodeBuilder` — fluent hardware-profile builder.
- `MockLlamaServer` — Axum responding to `/v1/chat/completions` and `/health` with canned responses; request counting via `Arc<AtomicU64>`.
- `fixtures.rs` — reusable hardware profiles, models, capability profiles.

`tests/integration.rs` covers mesh formation, gossip convergence, layer
assignment, inference E2E through the mock server, fault recovery,
graceful pause/resume, OICP routing, multi-model portfolio, knowledge
fan-out, ledger accuracy, fairness throttling. Deterministic timing — no
real 10s gossip waits.

### 5.8 Distributed state and apps

`commonwealth-state::MeshStore` — gossip-replicated SQLite KV (WAL mode):
`StoreEntry { app_id, key, value: Bytes, timestamp, origin: NodeId }`,
LWW conflict resolution, per-`app_id` namespace, `RetentionGc` for TTL.

`commonwealth-app` — mesh app platform: `MeshAppManifest` (gossiped),
`AppPermissions` (`mesh_store_read`/`_write`, `inference_access`,
`knowledge_access`), `AppRegistry`, `AppProcess` lifecycle, `AppPortMap`
+ `forward()` reverse-proxy helpers.

### 5.9 CLI

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

### 5.10 Deployment

`contrib/`: `install.sh` (curl installer), `systemd/commonwealth.service`,
`launchd/com.commonwealth.daemon.plist`.

---

## 6. How the Four Projects Fit Together

**Sovereign standalone** — Tauri/CLI/server runs against `EmbeddedLlamaCpp`.
Knowledge bases via `MeshCorpusManager` (named for the mesh case but works
without one); `EmbedFn` wraps the local Embed slot.

**Commonwealth standalone** — daemon serving `localhost:9741`. Any
OpenAI-compatible client points at it for distributed inference.
Knowledge ingest uses `embed_http::http_embed_fn` so it can index without
its own embed model.

**Sovereign + Commonwealth (integrated)** — `sovereign-mesh::EmbeddedDaemon`
runs Commonwealth in-process. Runtime's inference is wrapped in
`MeshInferenceProvider`, which OICP-routes synthesis to peers when scoring
favours them. Both sides share `sovereign_mesh::oicp_select` so Joiner's
selected model and Founder's served slot can't drift.
`complete_stream_with_id` returns model attribution alongside the stream so
peer-served completions show in `ResponseProvenance.inference_backend` as
`"Qwen3.5-9B.Q8_0 @ peer BeefyMac"`. Skills with `privacy = "local_only"`
short-circuit to local.

**Desktop attach mode** — both the desktop app and `sovereign daemon` (the
launchd/systemd service started by `sovereign setup`) want :9741. The
desktop probes `http://127.0.0.1:9741/v1/models` at startup
(`bootstrap::detect`); on success it enters Attach mode: inference flows
through `RemoteApiProvider`, mesh mutations go over HTTP via
`sovereign-mesh::mesh_http`, and `commands::save_config` POSTs
`/v1/admin/reload` so the daemon swaps its `InferenceProvider` in place.
Smoke test at `sovereign/scripts/smoke-attach-mode.sh`.

`/v1/admin/reload` rebuilds only what changed:

| Changed field                       | Reload action                                      |
|-------------------------------------|----------------------------------------------------|
| `models.primary` / `.fast` / `.embed` | Rebuild via `ProviderFactory`, atomic swap       |
| `daemon.client_port` / `.internal_port` | `restart_required: true`                       |
| `data.dir`                          | `restart_required: true`                            |

When `restart_required: true`, `save_config` falls back to `launchctl
kickstart -k gui/$(id -u)/com.sovereign.daemon` (macOS) or `systemctl
--user restart sovereign` (Linux).

---

## 7. Build, Test, Run

### Prerequisites

- Rust toolchain (stable)
- `cmake` (llama.cpp)
- `protoc` (LanceDB → `lance-table`); macOS: `brew install protobuf`; Debian: `apt install protobuf-compiler`
- For Commonwealth: `llama-server` + `rpc-server` from `llama.cpp` on `PATH`
- For desktop: Node.js + Tauri 2 (`cargo install tauri-cli --version "^2"`)

### Build / test

Each project is its own Cargo workspace. Use the **sovereign watcher**
(`lint_status` / `test_status` MCP tools) for compilation feedback —
running `cargo build` / `cargo test` directly via Bash contends with the
watcher for the file lock and idles.

```sh
cd corpus-engine && cargo build --release   # bundled assets copied via build.rs
cd sovereign     && cargo build --release
cd commonwealth  && cargo build --release
```

```sh
cd corpus-engine && cargo test                 # ~95 tests
cd sovereign     && cargo test --workspace     # ~289 tests
cd commonwealth  && cargo test --workspace     # ~222 unit + integration
```

No tests require GPU, models, or network. Sovereign uses
`DeterministicInference` + in-memory SQLite + real FTS5 for functional
tests. Commonwealth's harness runs simulated meshes deterministically.

### Run

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
| 9741  | Commonwealth/Sovereign client API (OpenAI-compatible) |
| 9742  | Commonwealth/Sovereign internal API (mTLS)  |
| 9743+ | `llama-server` instances                    |
| 50051+| `rpc-server` instances for layer shards     |
| 8080  | Sovereign HTTP server (configurable)        |

---

## 8. Where to Look for What

| You want to                                      | Read                                                                |
|--------------------------------------------------|---------------------------------------------------------------------|
| Understand the agent runtime                     | `sovereign/crates/sovereign-core/src/runtime.rs`                    |
| See how plans are executed                       | `sovereign/crates/sovereign-core/src/executor.rs`                   |
| Add a tool                                       | `sovereign-core/src/traits.rs` then a new file under `sovereign-tools/src/` |
| Add a corpus parser                              | `corpus-engine/src/extractors/` then register in `engine/ingest.rs` |
| Add a corpus filter                              | `corpus-engine/src/filters/` (impl `DocumentFilter`) + `recipe.rs::FilterConfig` + `filters/loader.rs` |
| Bundle a generated data file in corpus-engine    | Place in `sovereign-recipes/<corpus>/data/`, append filename to `corpus-engine/build.rs::BUNDLED_ASSETS`, `include_bytes!(concat!(env!("OUT_DIR"), …))` in `filters/assets.rs` |
| Write a recipe                                   | `sovereign-recipes/<id>/recipe.toml` then add an entry to `registry.toml` |
| Author a recipe via the agent loop               | `sovereign-tools/src/recipe_author/` (5 tools, `Permission::RecipeAuthoring`); wired in `sovereign-cli/src/main.rs:591` |
| Add an `http_api` recipe (REST source)           | See §3.10; example shape in `corpus-engine/src/recipe.rs` round-trip tests |
| Add an investigation recipe                      | Declare `enrichment.type = "investigation"` + `[[entity_types]]` + `[[relationship_types]]` + `[[patterns]]`; run via `sovereign enrich investigation build <id>` |
| Surface findings in an audit                     | `sovereign-cli/src/project_cmd.rs::compose_publish_recipe_nudge` reads `<index>/investigation/pattern_findings.json` |
| Write a skill                                    | `sovereign/skills/<id>/skill.toml`                                  |
| Tune model selection per hardware                | `sovereign/models.toml`                                             |
| Understand the SCIP call graph                   | `corpus-engine/src/scip_graph.rs` (schema, staleness, queries)      |
| Add a SCIP language exporter                     | `corpus-engine/src/scip_export.rs::all_exporters()`                 |
| See the code-intelligence MCP server             | `sovereign/crates/sovereign-cli/src/project_cmd.rs` (`cmd_serve`, inline `mcp_server`) |
| See the Sovereign HTTP MCP route                 | `sovereign/crates/sovereign-server/src/routes_mcp.rs`               |
| Understand session reflections                   | `corpus-engine/src/notes.rs` (NoteStore, write_reflection, retire_by_tool) and `sovereign-cli/src/reflect_cmd.rs` |
| Trace a Commonwealth scheduling decision         | `commonwealth-inference/src/scheduler/plan_builder.rs`              |
| Trace a Commonwealth shard plan                  | `commonwealth-inference/src/scheduler/layer_assignment.rs`          |
| Trace process spawning                           | `commonwealth-inference/src/orchestrator/process.rs`                |
| Add an internal mesh route                       | `commonwealth-api/src/routes_internal.rs`                           |
| Stand up a multi-node test                       | `commonwealth-test-harness/`                                        |
| See OICP routing logic                           | `oicp-types/src/lib.rs` + `sovereign-mesh/src/oicp_select.rs` + `commonwealth-inference/src/scheduler/oicp_select.rs` + `sovereign-inference/src/selector.rs` |
| Understand index storage on disk                 | `corpus-engine/src/index/mod.rs`                                    |
| Understand embedding injection                   | `corpus-engine/src/types.rs` (`EmbedFn`) + `commonwealth-knowledge/src/embed_http.rs` |
| Understand v1 enrichment domains                 | `corpus-engine/src/enrichment/domain.rs` + `enrichment/domains/`    |
| Understand the v2 atlas pipeline                 | `corpus-engine/ENRICHMENT_V2.md` + `enrichment/pipeline/mod.rs` (`Pipeline` trait, `PipelineRegistry`, `ExemplarBank`, `PhaseCache`) |
| Add a v2 pipeline                                | `corpus-engine/src/enrichment/pipeline/pipelines/` + `PipelineRegistry::builtin` |
| Drive v2 enrichment from the CLI                 | `sovereign-cli/src/enrich_cmd/`                                     |
| Understand the recipe registry                   | `corpus-engine/src/registry.rs` (+ `recipe.rs::bundled_recipe_toml` for offline fallback) |
| Understand delta updates                         | `corpus-engine/src/update/delta.rs`                                 |
| Understand scope expansion (filter delta)        | `corpus-engine/src/engine/expand.rs`                                |
| Understand KnowledgeView digest assembly         | `sovereign-tools/src/knowledge_view/manager.rs`, `digest.rs`, `cross_view.rs`, `recipes.rs` |
| See where KnowledgeView is injected              | `sovereign-core/src/runtime.rs::splice_landscape_digests` + `traits.rs::LandscapeDigestProvider` |
| Understand ATOS lifecycle                        | `sovereign-atos/src/local/orchestrator.rs`, `charter.rs`, `approval.rs` |
| See the ATOS CLI surface                         | `sovereign-cli/src/atos_cmd/` + `project_cmd.rs` (`cmd_found`, `cmd_amend`, `cmd_phase`, `cmd_audit`) |
| Trace a sovereign-coder pipeline turn            | `commonwealth-api/src/middleware/` + `commonwealth-core/src/default_pipelines.toml` |
| Install / upgrade the ATOS opencode plugin       | `sovereign-cli/assets/sovereign-atos.ts` + `sovereign-cli/src/atos_plugin.rs` (include_str! + version header) |
| Run the long-running Sovereign daemon            | `sovereign-cli/src/daemon_cmd.rs::run` + `contrib/launchd` + `contrib/systemd` |
| Rotate daemon logs                               | `sovereign-cli/src/util/log_rotation.rs` (copy-truncate; preserves inode for launchd-held FDs) |
| Understand the loopback guard                    | `sovereign-mesh/src/loopback_guard.rs` + `admin_http::tests::loopback_guard_works_under_production_listener_shape` |
| Understand local-corpus snapshot/rollback        | `sovereign-tools/src/local_corpus/writeback.rs` + `frontmatter.rs`  |

---

## 9. Glossary

- **OICP** — Open Inference Capabilities Protocol (v0.3). Wire types in `oicp-types`. A model publishes one `CapabilityClaim` per kind-of-work it does well; schedulers score requests against claims with shared protocol-level + per-scheduler operational adjustments.
- **CapabilityHint** — Validated tag identifying a kind of work. Standardized: `general`, `code`. Open vocabulary via `x:<tag>`.
- **Recipe** — A TOML file in `sovereign-recipes` describing how to ingest one corpus end-to-end (acquire, extract, filter, chunk, index, optionally enrich).
- **Registry** — The recipe catalog at `sovereign-recipes/registry.toml`. `corpus-engine` ships a compile-time bundled snapshot; can refresh from GitHub.
- **DocumentFilter** — Trait between extract and chunk that drops `ExtractedDoc`s by predicate. Composable via `[[filter]]` array (`Any`/`All`).
- **FilterPipeline / ScopeMeta** — A recipe's filter set + its hash. Stored in `_corpus_meta.json`; lets a corpus expand in place by relaxing filters and delta-ingesting.
- **Field Model (v1)** — Five-phase enrichment (skeleton → cluster → align → fault lines → open questions) that analyses a corpus holistically rather than per-chunk.
- **Domain (v1)** — Trait encoding the epistemic conventions of a knowledge field (philosophy, science, …). The single extension point for v1.
- **Atlas (v2)** — Typed atom graph (7 atom types × 7 edge types) + `Pipeline` trait + `PipelineRegistry` + `ExemplarBank` + `PhaseCache`. See `corpus-engine/ENRICHMENT_V2.md`.
- **SCIP** — Source Code Intelligence Protocol. `scip_graph.rs` stores SCIP data in SQLite; `scip_export.rs` dispatches to language-specific analyzers.
- **CodeWatcher** — `notify`-crate filesystem watcher. Re-indexes modified files via `CorpusEngine::reindex_file` and marks them stale in the call graph (800 ms debounce).
- **Shard** — A `corpus-engine` index containing only a contiguous chunk-ID range. Structurally identical to a complete index.
- **Skill** — A TOML file configuring routing triggers, planner templates, prompt overrides, memory rules, and OICP requirements for a class of work.
- **Slot** — A model-loading position in `EmbeddedLlamaCpp`: Quick (router/compression), Main (planning/synthesis), Code (hint-routed code work, shares Main's mutex), Embed (vector embeddings — its own `Arc<EmbedSlot>`).
- **Mesh** — A closed trust ring of Commonwealth nodes that share inference and knowledge. Joined via a `cwth-XXXX-XXXX-XXXX` key.
- **Peering** — A trust relationship between two distinct meshes that lets them exchange models or knowledge under a chosen `PeerTrustLevel`.
- **EmbedFn / InferenceFn** — Function types `corpus-engine` accepts from its caller for embedding text and (optionally) running an LLM during enrichment. Keeps the engine free of any specific runtime.
- **EmbedModelInfo** — `{ model_id, dimensions, pooling, normalization }`. A cross-peer interoperability contract — nodes sharing a corpus must produce bit-compatible vectors.
- **KnowledgeView** — Three-map landscape-digest system (personal memories, 180-day conversation history, institutional notes) spliced into the system prompt before each turn. Strict local-scope privacy is structural, not policy.
- **ATOS** — Agent Task Orchestration System. Two-layer scaffolding (project charter + per-feature specs) injecting the relevant contract into every agent turn, detecting SHA-256 drift, recording decisions/deviations/milestones under `.sovereign/`.
- **Charter** — ATOS specification document (`CHARTER.md` for the project, `spec.md` for a feature). Committing it is approval.
- **Drift** — ATOS term for "spec file changed since approval." Warns next turn; does not block. Either revert or `atos spec accept`.
- **Sovereign-coder pipeline** — Commonwealth middleware chain (`approval_gate → session_briefing → context_injector → tool_injector → artifact_surface`) that adapts a generic coder model into an ATOS-aware one.
