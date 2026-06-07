mod activity;
mod approval;
mod auth;
mod busy;
mod config;
mod narration;
mod projection;
mod routes;
mod routes_documents;
mod routes_mcp;
mod routes_tdd;
mod startup;
mod tenant;
mod ws;

#[cfg(test)]
mod http_tests;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::middleware;
use axum::routing::{delete, get, post};
use axum::Extension;
use tower_http::cors::CorsLayer;

use sovereign_core::planner::LlmPlanner;
use sovereign_core::router::LlmRouter;
use sovereign_core::runtime::Runtime;
use sovereign_core::traits::StateStore;
use sovereign_core::{SkillRegistry, ToolRegistry};
use sovereign_inference::embedded::EmbeddedLlamaCpp;
use sovereign_inference::health::HealthTracker;
use sovereign_inference::hybrid::HybridProvider;
use sovereign_inference::remote::RemoteApiProvider;
use sovereign_inference::selector::BackendEntry;
use sovereign_store::sqlite::SqliteStateStore;
use sovereign_tools::shell::ShellTool;

use crate::approval::ServerApprovalChannel;
use crate::auth::AuthState;
use crate::config::ServerConfig;

fn print_usage() {
    eprintln!("Usage: sovereign-server --config <path.toml>");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --config <path>   Server configuration file (required)");
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "sovereign_server=info,\
                 sovereign_core=debug,\
                 sovereign_tools=debug,\
                 sovereign_inference=debug,\
                 corpus_engine=info,\
                 tower_http=info"
                    .into()
            }),
        )
        .init();

    // Parse args.
    let args: Vec<String> = std::env::args().collect();
    let config_path = args
        .windows(2)
        .find(|w| w[0] == "--config")
        .map(|w| PathBuf::from(&w[1]));

    let config_path = match config_path {
        Some(p) => p,
        None => {
            print_usage();
            std::process::exit(1);
        }
    };

    let config = match ServerConfig::load(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Configuration error: {e}");
            std::process::exit(1);
        }
    };

    // Always load a dedicated embedding model. The chat/fast slot is the
    // wrong tool for embeddings, and the prior `load_dual` path left the
    // embed slot empty — so `embed()` errored and corpus retrieval was
    // silently dead. Resolve it once and thread it into every backend.
    let embed_model = resolve_embed_model(&config.inference);
    match embed_model {
        Some(ref p) => tracing::info!("Embedding model: {}", p.display()),
        None => tracing::warn!(
            "No embedding model configured and no qwen-embedding-0.6b.gguf found \
             next to the chat model — corpus retrieval is DISABLED (query \
             embedding unavailable). Set [inference] embed_model or co-locate the \
             model."
        ),
    }

    // Load inference — single model or multi-backend hybrid.
    let inference: Arc<dyn sovereign_core::traits::InferenceProvider> =
        if config.inference.backends.is_empty() {
            // Legacy single-model mode.
            tracing::info!("Loading model: {}", config.inference.model.display());
            let embedded = match EmbeddedLlamaCpp::load_full(
                &config.inference.model,
                config.inference.primary_model.as_deref(),
                embed_model.as_deref(),
                config.inference.context_size,
                None,
            ) {
                Ok(p) => Arc::new(p),
                Err(e) => {
                    eprintln!("Failed to load model: {e}");
                    std::process::exit(1);
                }
            };
            if config.inference.primary_model.is_some() {
                embedded.start_idle_monitor(60);
            }
            embedded
        } else {
            // Multi-backend hybrid mode.
            tracing::info!(
                "Hybrid mode: {} backends configured",
                config.inference.backends.len()
            );
            let mut backends: Vec<(
                Arc<dyn sovereign_core::traits::InferenceProvider>,
                BackendEntry,
            )> = Vec::new();

            for bc in &config.inference.backends {
                match bc.backend_type.as_str() {
                    "embedded" => {
                        let model = bc.model.as_ref().unwrap_or(&config.inference.model);
                        tracing::info!("  Backend {}: embedded ({})", bc.name, model.display());
                        match EmbeddedLlamaCpp::load_full(
                            model,
                            bc.primary_model.as_deref(),
                            embed_model.as_deref(),
                            bc.context_size,
                            None,
                        ) {
                            Ok(p) => {
                                let p = Arc::new(p);
                                if bc.primary_model.is_some() {
                                    p.start_idle_monitor(60);
                                }
                                let entry = BackendEntry::new_local(
                                    &bc.name,
                                    Arc::new(HealthTracker::new()),
                                    bc.priority,
                                );
                                backends.push((p, entry));
                            }
                            Err(e) => {
                                tracing::error!("  Failed to load {}: {e}", bc.name);
                            }
                        }
                    }
                    "remote" => {
                        let endpoint = bc.endpoint.as_deref().unwrap_or("http://localhost:8000/v1");
                        let model_id = bc.model_id.as_deref().unwrap_or("default");
                        tracing::info!("  Backend {}: remote ({})", bc.name, endpoint);
                        let provider = Arc::new(RemoteApiProvider::new(
                            endpoint,
                            bc.api_key.clone(),
                            model_id,
                            bc.context_size,
                        ));
                        let entry = BackendEntry::new_remote(
                            &bc.name,
                            Arc::new(HealthTracker::new()),
                            bc.priority,
                            None,
                        );
                        backends.push((provider, entry));
                    }
                    other => {
                        tracing::warn!("  Unknown backend type: {other}");
                    }
                }
            }

            if backends.is_empty() {
                eprintln!("No backends loaded successfully");
                std::process::exit(1);
            }

            let hybrid = Arc::new(HybridProvider::with_defaults(backends));
            hybrid.start_health_loop(30);
            hybrid
        };

    // Open database. We keep two handles: a concrete
    // `Arc<SqliteStateStore>` (so we can call `set_observer` once the
    // KnowledgeViewManager is ready, later in this function) and the
    // `Arc<dyn StateStore>` trait object used throughout the runtime
    // and tools. Both point at the same underlying store.
    let store_concrete: Arc<SqliteStateStore> = match SqliteStateStore::open(&config.store.path) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("Failed to open database: {e}");
            std::process::exit(1);
        }
    };
    let store: Arc<dyn StateStore> = store_concrete.clone();

    // Load skills.
    let mut skills = SkillRegistry::new();
    if let Some(ref skills_dir) = config.skills.dir {
        if skills_dir.exists() {
            skills.load_and_register(skills_dir);
            skills.activate_all();
            tracing::info!("Skills: {} loaded", skills.list().len());
        }
    }
    let skills = Arc::new(skills);

    // Build components.
    //
    // Install the binary current-info classifier (time-sensitive →
    // external search) so the `force_action` pre-check decides
    // semantically instead of substring-matching a keyword list. Without
    // it, "…history of Lebanon from antiquity to today" tripped on the
    // bare word "today" → ACTION/external-tool route → refused-to-write
    // loop. Falls through to the keyword heuristic on load failure, so a
    // missing example file is a soft degrade, not a startup error.
    let mut llm_router = LlmRouter::new(
        Arc::clone(&inference),
        Arc::clone(&store),
        Arc::clone(&skills),
    );
    if let Some(path) = resolve_current_info_examples_path() {
        match sovereign_core::current_info_classifier::CurrentInfoClassifier::load(
            &path,
            Arc::clone(&inference),
        )
        .await
        {
            Ok(cls) => {
                tracing::info!(
                    current = cls.current_count(),
                    evergreen = cls.evergreen_count(),
                    path = %path.display(),
                    "router current-info classifier loaded"
                );
                llm_router = llm_router.with_current_info_classifier(Arc::new(cls));
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "current-info classifier load failed; force_action falls back to keyword heuristic"
                );
            }
        }
    }
    let router: Box<dyn sovereign_core::traits::Router> = Box::new(llm_router);

    let planner = LlmPlanner::new(Arc::clone(&inference), Arc::clone(&skills));

    // Construct a shared CorpusEngine for the epistemic tools.
    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    let recipes_dir = home.join(".sovereign").join("recipes");
    let indexes_dir = home.join(".sovereign").join("indexes");
    let embed_fn = sovereign_tools::corpus::inference_to_embed_fn(Arc::clone(&inference));
    let batch_embed_fn =
        sovereign_tools::corpus::inference_to_batch_embed_fn(Arc::clone(&inference));
    let inference_fn = sovereign_tools::corpus::inference_to_inference_fn(Arc::clone(&inference));
    // Tell the engine which embedding model it's running, so its
    // per-corpus model-mismatch check is meaningful (corpora are indexed
    // with e.g. `qwen-embedding-0.6b`; an empty expected id warned on
    // every open). Derived from the resolved embed model's file stem.
    let embed_model_name = embed_model
        .as_deref()
        .and_then(|p| p.file_stem())
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();
    let corpus_engine = Arc::new(
        corpus_engine::CorpusEngine::new(recipes_dir, indexes_dir.clone(), embed_fn)
            .with_batch_embed_fn(batch_embed_fn)
            .with_embedding_model(&embed_model_name)
            // Scope retrieval to the operator's chosen corpora (empty =
            // all). Skips opening/searching experiment/partial corpora.
            .with_corpus_allow_list(config.retrieval.corpora.clone())
            .with_inference_fn(inference_fn.clone()),
    );
    if !config.retrieval.corpora.is_empty() {
        tracing::info!(
            corpora = ?config.retrieval.corpora,
            "retrieval scoped to allow-listed corpora"
        );
    }

    // KnowledgeView integration: register the SQLite acquirer on the
    // engine, build the manager with the `inner-work` skill's
    // conversations excluded, and wire it as both the store's
    // post-commit `StateStoreObserver` and the Runtime's
    // `LandscapeDigestProvider`. The manager also spawns its
    // debouncer task so Tier-3 enrichment fires on bursts of memory
    // or message writes. See
    // `sovereign_tools::knowledge_view::KnowledgeViewManager`.
    // Gated on `[knowledge_view] enabled` in sovereign-server.toml.
    // When disabled, the server skips the three enriched views +
    // cross-view resonance entirely — no ingest, no observer, no
    // landscape-digest splice. Mirror of the desktop Settings toggle.
    let knowledge_view_manager = if config.knowledge_view.enabled {
        // Resolve `local_only` skill ids dynamically so any skill whose
        // `[inference] privacy = "local_only"` participates in the
        // conversational-view exclusion without editing this bootstrap.
        let local_only_skill_ids = skills.local_only_skill_ids();
        tracing::info!(
            local_only_skills = ?local_only_skill_ids,
            "knowledge_view: enabled; skills excluded from conversational corpus"
        );
        // Project-local ATOS state — `.sovereign/{features.db,project.toml}`
        // at the current working directory's repo root. Same layout
        // `sovereign project serve` writes to. Wired into the manager
        // so the strategic digest can render ATOS phase / drift on
        // initiative entities; both paths are optional (the splice
        // path falls through gracefully when missing).
        let project_sov_dir = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(".sovereign");
        let features_db_path = project_sov_dir.join("features.db");
        let project_toml_path = project_sov_dir.join("project.toml");
        let mut mgr = sovereign_tools::knowledge_view::KnowledgeViewManager::new(
            Arc::clone(&corpus_engine),
            inference_fn.clone(),
            config.store.path.clone(),
            local_only_skill_ids,
        )
        .await;
        if features_db_path.exists() {
            mgr = mgr.with_features_db_path(features_db_path);
        }
        if project_toml_path.exists() {
            mgr = mgr.with_project_toml_path(project_toml_path);
        }
        let mgr = Arc::new(mgr);
        // Install as the store's observer now that the manager exists.
        // The store was Arc-wrapped earlier; interior mutability on the
        // observer slot lets us swap it in without restructuring.
        store_concrete
            .set_observer(mgr.clone() as sovereign_core::observer::SharedStateStoreObserver);
        // Kick off initial ingest in the background. First-run ingest
        // can take 10–60s on a populated DB; blocking startup on it
        // would delay the /v1/* listener binding for no good reason —
        // the landscape digests are additive and can lag behind the
        // first few conversations.
        let _init_handle = Arc::clone(&mgr).spawn_init();
        Some(mgr)
    } else {
        tracing::info!(
            "knowledge_view: disabled via config — landscape digests \
             skipped, no ingest will run"
        );
        None
    };

    // Register tools. Tier 4 — shared tool-result cache so the
    // sovereign-server (standalone HTTP daemon) gets the same
    // per-conversation cache the CLI / desktop bootstrap wire.
    let tool_cache = Arc::new(sovereign_core::tool_result_cache::ToolResultCache::new());
    let mut tools = ToolRegistry::new().with_cache(Arc::clone(&tool_cache));
    tools.register(Box::new(ShellTool));
    tools.register(Box::new(sovereign_tools::document::DocumentTool::new(
        Arc::clone(&store),
        Arc::clone(&inference),
    )));
    tools.register(Box::new(sovereign_tools::DocumentOperationTool::new(
        Arc::clone(&store),
        Arc::clone(&inference),
    )));
    tools.register(Box::new(sovereign_tools::search::SearchTool::with_web(
        Arc::clone(&store),
        Arc::clone(&inference),
        sovereign_tools::web::search::SearchBackend::DuckDuckGo,
    )));
    tools.register(Box::new(sovereign_tools::web::WebFetchTool::new()));
    tools.register(Box::new(sovereign_tools::WikipediaFetchTool::new(
        Arc::clone(&corpus_engine),
    )));
    tools.register(Box::new(sovereign_tools::compute::ComputeTool));
    tools.register(Box::new(sovereign_tools::ClaimSearchTool::new(Arc::clone(
        &corpus_engine,
    ))));
    tools.register(Box::new(sovereign_tools::EpistemicLandscapeTool::new(
        Arc::clone(&corpus_engine),
    )));
    tools.register(Box::new(
        sovereign_tools::parcel_analytics::ParcelAnalyticsTool::new(Arc::clone(&corpus_engine)),
    ));
    // SCIP call graph database + tools (v2).
    //
    // The call-graph tools take `Arc<ArcSwap<ScipGraph>>` so the CLI's
    // `project serve` can hot-swap the graph when the on-disk DB changes.
    // sovereign-server opens a single on-disk DB at a fixed path and doesn't
    // swap, but the tool signature still requires the wrapper.
    //
    // Built before the code-intel tools below so `SymbolLookupTool`
    // can share the handle (exact-name lookup reads SCIP directly
    // since the SCIP-as-truth refactor).
    let scip_db_path = home
        .join(".sovereign")
        .join("indexes")
        .join("_scip_graph.db");
    let scip_graph =
        corpus_engine_scip::ScipGraph::open(&scip_db_path, "default").expect("SCIP graph database");
    let scip_graph: sovereign_tools::ScipGraphHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(scip_graph));
    let health_checker = Arc::new(sovereign_tools::IndexHealthChecker::new(Arc::clone(
        &scip_graph,
    )));

    // Code Intelligence tools.
    tools.register(Box::new(sovereign_tools::SymbolLookupTool::new(
        Arc::clone(&corpus_engine),
        Arc::clone(&scip_graph),
    )));
    tools.register(Box::new(
        sovereign_tools::CodeSearchTool::new(Arc::clone(&corpus_engine))
            .with_inference(Arc::clone(&inference)),
    ));
    tools.register(Box::new(sovereign_tools::RecentChangesTool::new(
        Arc::clone(&corpus_engine),
    )));
    tools.register(Box::new(
        sovereign_tools::FindCalleesTool::new(Arc::clone(&corpus_engine), Arc::clone(&scip_graph))
            .with_health_checker(Arc::clone(&health_checker)),
    ));
    tools.register(Box::new(
        sovereign_tools::FindCallersTool::new(Arc::clone(&corpus_engine), Arc::clone(&scip_graph))
            .with_health_checker(Arc::clone(&health_checker)),
    ));

    // Working notes tools — persist across sessions, used for session attribution.
    let notes_db_path = home.join(".sovereign").join("notes.db");
    let note_store_for_runtime: Option<Arc<corpus_engine_notes::NoteStore>> =
        match corpus_engine_notes::NoteStore::open(&notes_db_path) {
            Ok(store) => {
                let store = Arc::new(store);
                tools.register(Box::new(sovereign_tools::WriteNoteTool::new(Arc::clone(
                    &store,
                ))));
                tools.register(Box::new(sovereign_tools::ReadNotesTool::new(Arc::clone(
                    &store,
                ))));
                tools.register(Box::new(sovereign_tools::DeleteNoteTool::new(Arc::clone(
                    &store,
                ))));
                tracing::info!("Notes: tools registered ({})", notes_db_path.display());
                Some(store)
            }
            Err(e) => {
                tracing::warn!(error = %e, "notes.db unavailable — note tools disabled");
                None
            }
        };

    // Recipe-author workspace tools. Registered so a conversation tagged
    // `skill_id = "recipe-author"` (via `POST /v1/conversations
    // {"skill_id":"recipe-author"}`) can drive the authoring agent loop over
    // the conversation API — the headless equivalent of the desktop
    // recipe-author workspace (`sovereign-desktop/.../state.rs`). The narrowed
    // catalog only surfaces these when `active_mode == recipe-author`, so
    // generic chat is unaffected.
    {
        use sovereign_tools::recipe_author::{
            CapabilityRequestTool, CheckpointTool, DecisionLogTool, ProbeUrlTool, RecipeReadTool,
            RecipeTestTool, RecipeValidateTool, RecipeWriteStructuredTool, RecipeWriteTool,
            RegistryBrowseTool, ResearchFindingTool,
        };
        tools.register(Box::new(RecipeReadTool::new()));
        tools.register(Box::new(RecipeWriteTool::new()));
        tools.register(Box::new(RecipeWriteStructuredTool::new()));
        tools.register(Box::new(RecipeValidateTool::new()));
        tools.register(Box::new(RecipeTestTool::new()));
        tools.register(Box::new(RegistryBrowseTool));
        tools.register(Box::new(ProbeUrlTool::new()));
        if let Some(ref notes) = note_store_for_runtime {
            tools.register(Box::new(DecisionLogTool::with_notes(Arc::clone(notes))));
            tools.register(Box::new(ResearchFindingTool::with_notes(Arc::clone(notes))));
            let features_db = home.join(".sovereign").join("features.db");
            match corpus_engine_atos::FeatureStore::open(&features_db) {
                Ok(features) => {
                    let features = Arc::new(features);
                    tools.register(Box::new(CheckpointTool::with_stores(
                        Arc::clone(notes),
                        Arc::clone(&features),
                    )));
                    tools.register(Box::new(CapabilityRequestTool::with_stores(
                        Arc::clone(notes),
                        Arc::clone(&features),
                    )));
                    tracing::info!("Recipe-author: tools registered (with feature store)");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "features.db unavailable — recipe-author checkpoint/capability tools disabled");
                }
            }
        } else {
            tracing::warn!(
                "notes.db unavailable — recipe-author decision/research/checkpoint tools disabled"
            );
        }
    }

    // Activity reporter — signals coding intensity to Commonwealth so
    // the scheduler can route inference away from busy nodes.
    if let Some(ref commonwealth_url) = config.commonwealth.url {
        let reporter = Arc::new(activity::ActivityReporter::new(commonwealth_url.clone()));
        reporter.start_decay_loop();
        // TODO: pass reporter to WatcherCoordinator when watcher support is added
        tracing::info!(url = %commonwealth_url, "Commonwealth activity reporter started");
    }

    // Connect MCP servers (stdio and HTTP+SSE).
    let _mcp_manager =
        sovereign_tools::mcp::McpServerManager::from_config(&config.mcp.servers, &mut tools).await;

    tracing::info!("Tools: {} registered", tools.count());

    // Approval channel.
    let (approval_channel, _event_rx) = ServerApprovalChannel::new();
    let approval = Arc::new(approval_channel);

    // Glassbox progress: republish the runtime's turn narration on a
    // broadcast channel so each WS turn can forward its own stage frames
    // (retrieval / synthesis / gap-check / tool calls) to the client.
    // The `Sender` is layered as an Extension for `ws::stream_turn`.
    let (narration_sink, narration_tx) = crate::narration::BroadcastRoutingEventSink::new();

    let mut runtime_builder = Runtime::new(
        Arc::clone(&inference),
        router,
        Box::new(planner),
        Arc::new(tools),
        store,
        skills,
        approval.clone() as Arc<dyn sovereign_core::traits::ApprovalChannel>,
        // Honour the configured response-length budget ([inference]
        // max_tokens) instead of hardcoding the 2048 default — the
        // server-side equivalent of the desktop "Response length"
        // setting. All other knobs keep their core defaults.
        sovereign_core::types::InferenceConfig {
            max_tokens: config.inference.max_tokens,
            ..sovereign_core::types::InferenceConfig::default()
        },
    )
    .with_corpus_engine(Arc::clone(&corpus_engine))
    .with_routing_events(std::sync::Arc::new(narration_sink));
    // Note store for commitment persistence (CommissiveQuery handler).
    if let Some(store) = note_store_for_runtime {
        runtime_builder = runtime_builder.with_note_store(store);
    }
    // GLiNER entity extractor for retrieval-over-history. Probe the
    // default model id; if installed, load it and wire it onto the
    // Runtime. Failures soft-fall-through to pure cosine + MMR.
    {
        let model_id = sovereign_tools::gliner_ner::DEFAULT_MODEL_ID;
        if sovereign_tools::gliner_ner::probe_model_available(model_id) {
            match sovereign_tools::gliner_ner::GlinerExtractor::new_default() {
                Ok(g) => {
                    let arc: Arc<dyn sovereign_core::traits::EntityExtractor> = Arc::new(g);
                    runtime_builder = runtime_builder.with_gliner(arc);
                    tracing::info!(model = model_id, "server: GLiNER entity extractor loaded");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "server: GLiNER probe ok but load failed; entity-aware retrieval disabled");
                }
            }
        } else {
            tracing::debug!(
                model = model_id,
                "server: GLiNER model not installed; entity-aware retrieval disabled (falls back to cosine+MMR)"
            );
        }
    }
    // Install the landscape-digest provider only when KnowledgeView
    // is enabled. When disabled, the splice path stays a no-op —
    // identical to pre-KnowledgeView behaviour.
    if let Some(ref mgr) = knowledge_view_manager {
        runtime_builder = runtime_builder.with_landscape_digests(
            Arc::clone(mgr) as Arc<dyn sovereign_core::traits::LandscapeDigestProvider>
        );
    }
    // Atlas Layer 0: load any installed Wikipedia link graph at
    // `<indexes_dir>/<corpus>/wikipedia_graph.db`. Build via
    // `sovereign atlas wikipedia build-graph <corpus-id>`. Absent =
    // pre-Layer-0 behaviour preserved exactly.
    if let Some(graph) = load_wikipedia_graph_for_server(&corpus_engine, &indexes_dir).await {
        tracing::info!(
            articles = graph.article_count().await,
            edges = graph.edge_count().await,
            "wikipedia link graph: loaded"
        );
        runtime_builder = runtime_builder.with_wikipedia_graph(graph);
    }

    // Atlas-grounded retrieval: scan installed corpora for `atlas/`
    // dirs, pre-embed Entity descriptions, and stash them on the
    // Runtime so `prepare_knowledge_query_plan` can fuse atlas
    // matches into chunk hits as virtual ScoredChunks. Init runs in
    // the background — daemon listener binds without waiting on the
    // embed pass (cold first-run on a wiki-scale atlas can be
    // ~minutes; subsequent boots replay the on-disk cache near
    // instantly).
    let embed_model_id = config
        .inference
        .model
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    if embed_model_id.is_empty() {
        tracing::warn!(
            "atlas-context: could not derive embed model id from config.inference.model; \
             atlas grounding will be skipped"
        );
    } else {
        let atlas_mgr = Arc::new(
            sovereign_tools::atlas_context_manager::AtlasContextManager::new(
                indexes_dir.clone(),
                Arc::clone(&inference),
                embed_model_id,
            ),
        );
        runtime_builder = runtime_builder
            .with_atlas_context_provider(Arc::clone(&atlas_mgr)
                as Arc<dyn sovereign_core::atlas_context::AtlasContextProvider>);
        let _atlas_init = Arc::clone(&atlas_mgr).spawn_init();
        // Phase B2 — bump flusher writes adaptive triage priors to
        // disk every 30s so the next rebuild picks them up.
        let _bump_flusher = Arc::clone(&atlas_mgr).spawn_bump_flusher(30);
    }

    // Cross-corpus meta-atlas (Move 5). Loads the persisted
    // `canonical_atoms.json` produced by `sovereign meta-atlas build`.
    let meta_atlas_path = corpus_engine::meta_atlas::default_meta_atlas_path();
    match corpus_engine::meta_atlas::MetaAtlasIndex::load(meta_atlas_path.as_deref()) {
        Ok(idx) => {
            tracing::info!(
                atoms = idx.len(),
                corpora = idx.corpus_count(),
                "meta-atlas loaded"
            );
            runtime_builder = runtime_builder.with_meta_atlas(Arc::new(idx));
        }
        Err(e) => {
            tracing::warn!(error = %e, "meta-atlas load failed; boost disabled");
        }
    }

    let runtime = Arc::new(runtime_builder);

    // Auth state.
    let auth_state = if config.auth.mode == "api_key" && !config.auth.keys.is_empty() {
        tracing::info!("Auth: API key ({} keys configured)", config.auth.keys.len());
        AuthState::new(config.auth.keys.clone())
    } else {
        tracing::info!("Auth: disabled");
        AuthState::disabled()
    };

    // Busy guard — bounds concurrent inference turns; saturation surfaces
    // as `503 + Retry-After` (REST) / a busy stream frame (WS).
    let busy_guard = busy::BusyGuard::new(
        config.server.max_concurrent_turns,
        config.server.retry_after_secs,
    );
    tracing::info!(
        max_concurrent_turns = config.server.max_concurrent_turns,
        retry_after_secs = config.server.retry_after_secs,
        "Busy guard configured"
    );

    // Build Axum router. The `/v1/*` API goes through the auth
    // middleware; the MCP routes do not — MCP is local-only and
    // enforced via `ConnectInfo<SocketAddr>` inside the handlers.
    let authed = axum::Router::new()
        .route("/v1/conversations", post(routes::create_conversation))
        .route("/v1/conversations", get(routes::list_conversations))
        .route("/v1/conversations/{id}", get(routes::get_conversation))
        .route(
            "/v1/conversations/{id}",
            delete(routes::delete_conversation),
        )
        .route(
            "/v1/conversations/{id}/messages",
            post(routes::send_message),
        )
        .route("/v1/tasks/{id}/approve", post(routes::approve_task))
        .route("/v1/tools", get(routes::list_tools))
        .route("/v1/corpora", get(routes::list_corpora))
        .route(
            "/v1/corpora/{corpus_id}/chunks/{chunk_id}",
            get(routes::read_chunk),
        )
        .route("/v1/search", post(routes::search))
        .route("/v1/conversations/{id}/stream", get(ws::ws_handler))
        .merge(routes_documents::document_router())
        .merge(routes_tdd::tdd_router())
        .layer(middleware::from_fn(auth::auth_middleware))
        .layer(Extension(auth_state));

    // Build the TDD ChatBackend once at startup. Provider URL =
    // the server's own bind address by default — the daemon hosts
    // chat completions and the solver loop posts to them. Operators
    // who run an external provider (Anthropic, OpenAI-compat
    // backend) can override via SOVEREIGN_TDD_PROVIDER_URL until
    // the dedicated config section lands.
    let tdd_provider_url = std::env::var("SOVEREIGN_TDD_PROVIDER_URL")
        .unwrap_or_else(|_| format!("http://{}", config.server.bind));
    let tdd_backend: Arc<dyn commonwealth_tdd::ChatBackend> = Arc::new(
        commonwealth_tdd::ReqwestChatBackend::new(format!("{tdd_provider_url}/v1")),
    );
    let tdd_state = routes_tdd::TddState(Arc::clone(&tdd_backend));

    let app = authed
        .merge(routes_mcp::mcp_router())
        // Unauthenticated liveness probe (added at the `app` level so it sits
        // OUTSIDE the `/v1/*` auth layer). A supervisor — the desktop's
        // Mobile-access toggle, the CLI, or systemd — can poll `GET /health`
        // for a 200 to know the host is up, without holding a tenant token.
        .route("/health", get(|| async { "ok" }))
        .layer(Extension(Arc::clone(&runtime)))
        .layer(Extension(approval))
        .layer(Extension(tdd_state))
        .layer(Extension(busy_guard))
        .layer(Extension(narration_tx))
        .layer(CorsLayer::permissive());

    // Startup UX — mesh peer count from Commonwealth (non-fatal if daemon
    // isn't running).
    if let Some(ref commonwealth_url) = config.commonwealth.url {
        startup::print_mesh_status(commonwealth_url).await;
    }

    // Serve.
    let bind_addr = &config.server.bind;
    tracing::info!("Listening on {bind_addr}");

    let listener = match tokio::net::TcpListener::bind(bind_addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Failed to bind to {bind_addr}: {e}");
            std::process::exit(1);
        }
    };

    // Use `into_make_service_with_connect_info` so MCP handlers can
    // extract `ConnectInfo<SocketAddr>` to enforce localhost-only
    // access. Without this the extractor fails and every MCP request
    // is rejected.
    let service = app.into_make_service_with_connect_info::<SocketAddr>();
    if let Err(e) = axum::serve(listener, service).await {
        eprintln!("Server error: {e}");
        std::process::exit(1);
    }
}

/// Resolve the embedding model path. An explicit `[inference] embed_model`
/// (or the `SOVEREIGN_EMBED_MODEL` env var) wins; otherwise default to
/// `qwen-embedding-0.6b.gguf` co-located with the chat model — the standard
/// repo layout (`sovereign/models/`). Returns `None` only when neither is
/// found, which leaves retrieval disabled (the caller logs it loudly).
fn resolve_embed_model(inf: &config::InferenceSection) -> Option<PathBuf> {
    if let Some(p) = inf.embed_model.clone() {
        return Some(p);
    }
    let default = inf.model.parent()?.join("qwen-embedding-0.6b.gguf");
    default.exists().then_some(default)
}

/// Resolve the path to `router/current_info_examples.toml`. Mirrors
/// `chat_cmd::bootstrap::resolve_scope_examples_path`: honour
/// `$SOVEREIGN_CURRENT_INFO_EXAMPLES` (absolute or cwd-relative) first,
/// else the default `sovereign/router/current_info_examples.toml`
/// relative to the cwd. Returns `None` when neither exists so the
/// caller degrades to the keyword heuristic rather than erroring.
fn resolve_current_info_examples_path() -> Option<PathBuf> {
    if let Ok(env) = std::env::var("SOVEREIGN_CURRENT_INFO_EXAMPLES") {
        let p = PathBuf::from(env);
        if p.exists() {
            return Some(p);
        }
        tracing::warn!(
            path = %p.display(),
            "SOVEREIGN_CURRENT_INFO_EXAMPLES set but file missing; trying default"
        );
    }
    let default = PathBuf::from("sovereign/router/current_info_examples.toml");
    if default.exists() {
        Some(default)
    } else {
        None
    }
}

/// Probe `<indexes_dir>/<corpus_id>/wikipedia_graph.db` for each
/// installed corpus and return the first WikipediaGraph that opens
/// cleanly. Mirrors `chat_cmd::bootstrap::load_wikipedia_graph` —
/// duplicated here so the server doesn't take a dep on the CLI
/// binary's internal modules.
async fn load_wikipedia_graph_for_server(
    engine: &corpus_engine::CorpusEngine,
    indexes_dir: &std::path::Path,
) -> Option<Arc<corpus_engine::WikipediaGraph>> {
    let infos = engine.installed_indexes().await.ok()?;
    for info in infos {
        let db_path = corpus_engine::WikipediaGraph::default_db_path(indexes_dir, &info.corpus_id);
        if !db_path.exists() {
            continue;
        }
        match corpus_engine::WikipediaGraph::open(&db_path, &info.corpus_id) {
            Ok(g) => return Some(Arc::new(g)),
            Err(e) => {
                tracing::warn!(
                    corpus = %info.corpus_id,
                    db = %db_path.display(),
                    error = %e,
                    "wikipedia_graph: open failed; skipping"
                );
            }
        }
    }
    None
}
