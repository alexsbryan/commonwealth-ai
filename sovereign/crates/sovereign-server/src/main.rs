mod activity;
mod approval;
mod auth;
mod config;
mod routes;
mod routes_documents;
mod routes_mcp;
mod startup;
mod tenant;
mod ws;

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

    // Load inference — single model or multi-backend hybrid.
    let inference: Arc<dyn sovereign_core::traits::InferenceProvider> =
        if config.inference.backends.is_empty() {
            // Legacy single-model mode.
            tracing::info!("Loading model: {}", config.inference.model.display());
            let embedded = match EmbeddedLlamaCpp::load_dual(
                &config.inference.model,
                config.inference.primary_model.as_deref(),
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
                        match EmbeddedLlamaCpp::load_dual(
                            model,
                            bc.primary_model.as_deref(),
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
    let router: Box<dyn sovereign_core::traits::Router> = Box::new(LlmRouter::new(
        Arc::clone(&inference),
        Arc::clone(&store),
        Arc::clone(&skills),
    ));

    let planner = LlmPlanner::new(Arc::clone(&inference), Arc::clone(&skills));

    // Construct a shared CorpusEngine for the epistemic tools.
    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    let recipes_dir = home.join(".sovereign").join("recipes");
    let indexes_dir = home.join(".sovereign").join("indexes");
    let embed_fn = sovereign_tools::corpus::inference_to_embed_fn(Arc::clone(&inference));
    let batch_embed_fn =
        sovereign_tools::corpus::inference_to_batch_embed_fn(Arc::clone(&inference));
    let inference_fn =
        sovereign_tools::corpus::inference_to_inference_fn(Arc::clone(&inference));
    let corpus_engine = Arc::new(
        corpus_engine::CorpusEngine::new(recipes_dir, indexes_dir, embed_fn)
            .with_batch_embed_fn(batch_embed_fn)
            .with_inference_fn(inference_fn.clone()),
    );

    // KnowledgeView integration: register the SQLite acquirer on the
    // engine, build the manager with the `inner-work` skill's
    // conversations excluded, and wire it as both the store's
    // post-commit `StateStoreObserver` and the Runtime's
    // `LandscapeDigestProvider`. The manager also spawns its
    // debouncer task so Tier-3 enrichment fires on bursts of memory
    // or message writes. See
    // `sovereign_tools::knowledge_view::KnowledgeViewManager`.
    // Resolve `local_only` skill ids dynamically so any skill whose
    // `[inference] privacy = "local_only"` participates in the
    // conversational-view exclusion without editing this bootstrap.
    let local_only_skill_ids = skills.local_only_skill_ids();
    tracing::info!(
        local_only_skills = ?local_only_skill_ids,
        "knowledge_view: skills excluded from conversational corpus"
    );
    let knowledge_view_manager = Arc::new(
        sovereign_tools::knowledge_view::KnowledgeViewManager::new(
            Arc::clone(&corpus_engine),
            inference_fn.clone(),
            config.store.path.clone(),
            local_only_skill_ids,
        )
        .await,
    );
    // Install as the store's observer now that the manager exists.
    // The store was Arc-wrapped earlier; interior mutability on the
    // observer slot lets us swap it in without restructuring.
    store_concrete
        .set_observer(knowledge_view_manager.clone() as sovereign_core::observer::SharedStateStoreObserver);
    // Kick off initial ingest of empty views. Errors are logged,
    // not fatal — the rest of the runtime proceeds even if a view
    // fails to enrich on first start.
    if let Err(e) = knowledge_view_manager.init().await {
        tracing::warn!(error = %e, "knowledge_view: init() failed; landscape digests will be missing until a later manual enrich");
    }

    // Register tools.
    let mut tools = ToolRegistry::new();
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
    tools.register(Box::new(sovereign_tools::compute::ComputeTool));
    tools.register(Box::new(sovereign_tools::ClaimSearchTool::new(
        Arc::clone(&corpus_engine),
    )));
    tools.register(Box::new(sovereign_tools::EpistemicLandscapeTool::new(
        Arc::clone(&corpus_engine),
    )));
    // Code Intelligence tools.
    tools.register(Box::new(sovereign_tools::SymbolLookupTool::new(
        Arc::clone(&corpus_engine),
    )));
    tools.register(Box::new(
        sovereign_tools::CodeSearchTool::new(Arc::clone(&corpus_engine))
            .with_inference(Arc::clone(&inference)),
    ));
    tools.register(Box::new(sovereign_tools::RecentChangesTool::new(
        Arc::clone(&corpus_engine),
    )));

    // SCIP call graph database + tools (v2).
    //
    // The call-graph tools take `Arc<ArcSwap<ScipGraph>>` so the CLI's
    // `project serve` can hot-swap the graph when the on-disk DB changes.
    // sovereign-server opens a single on-disk DB at a fixed path and doesn't
    // swap, but the tool signature still requires the wrapper.
    let scip_db_path = home.join(".sovereign").join("indexes").join("_scip_graph.db");
    let scip_graph = corpus_engine::ScipGraph::open(&scip_db_path, "default")
        .expect("SCIP graph database");
    let scip_graph: sovereign_tools::ScipGraphHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(scip_graph));
    let health_checker = Arc::new(sovereign_tools::IndexHealthChecker::new(Arc::clone(&scip_graph)));
    tools.register(Box::new(sovereign_tools::FindCalleesTool::new(
        Arc::clone(&corpus_engine),
        Arc::clone(&scip_graph),
    ).with_health_checker(Arc::clone(&health_checker))));
    tools.register(Box::new(sovereign_tools::FindCallersTool::new(
        Arc::clone(&corpus_engine),
        Arc::clone(&scip_graph),
    ).with_health_checker(Arc::clone(&health_checker))));

    // Working notes tools — persist across sessions, used for session attribution.
    let notes_db_path = home.join(".sovereign").join("notes.db");
    match corpus_engine::NoteStore::open(&notes_db_path) {
        Ok(store) => {
            let store = Arc::new(store);
            tools.register(Box::new(sovereign_tools::WriteNoteTool::new(Arc::clone(&store))));
            tools.register(Box::new(sovereign_tools::ReadNotesTool::new(Arc::clone(&store))));
            tools.register(Box::new(sovereign_tools::DeleteNoteTool::new(store)));
            tracing::info!("Notes: tools registered ({})", notes_db_path.display());
        }
        Err(e) => tracing::warn!(error = %e, "notes.db unavailable — note tools disabled"),
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
    let _mcp_manager = sovereign_tools::mcp::McpServerManager::from_config(
        &config.mcp.servers,
        &mut tools,
    )
    .await;

    tracing::info!("Tools: {} registered", tools.count());

    // Approval channel.
    let (approval_channel, _event_rx) = ServerApprovalChannel::new();
    let approval = Arc::new(approval_channel);

    let runtime = Arc::new(
        Runtime::new(
            inference,
            router,
            Box::new(planner),
            Arc::new(tools),
            store,
            skills,
            approval.clone() as Arc<dyn sovereign_core::traits::ApprovalChannel>,
            sovereign_core::types::InferenceConfig::default(),
        )
        .with_corpus_engine(Arc::clone(&corpus_engine))
        .with_landscape_digests(
            knowledge_view_manager.clone()
                as Arc<dyn sovereign_core::traits::LandscapeDigestProvider>,
        ),
    );

    // Auth state.
    let auth_state = if config.auth.mode == "api_key" && !config.auth.keys.is_empty() {
        tracing::info!("Auth: API key ({} keys configured)", config.auth.keys.len());
        AuthState::new(config.auth.keys.clone())
    } else {
        tracing::info!("Auth: disabled");
        AuthState::disabled()
    };

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
        .route("/v1/search", post(routes::search))
        .route("/v1/conversations/{id}/stream", get(ws::ws_handler))
        .merge(routes_documents::document_router())
        .layer(middleware::from_fn(auth::auth_middleware))
        .layer(Extension(auth_state));

    let app = authed
        .merge(routes_mcp::mcp_router())
        .layer(Extension(Arc::clone(&runtime)))
        .layer(Extension(approval))
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
