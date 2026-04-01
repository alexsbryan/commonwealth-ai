mod approval;
mod auth;
mod config;
mod routes;
mod tenant;
mod ws;

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
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "sovereign_server=info,tower_http=info".into()),
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

    tracing::info!("Loading model: {}", config.inference.model.display());

    // Load inference.
    let inference = match EmbeddedLlamaCpp::load_dual(
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
        inference.start_idle_monitor(60);
    }

    // Open database.
    let store: Arc<dyn StateStore> = match SqliteStateStore::open(&config.store.path) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("Failed to open database: {e}");
            std::process::exit(1);
        }
    };

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
    let inference_arc: Arc<dyn sovereign_core::traits::InferenceProvider> = inference;

    let router: Box<dyn sovereign_core::traits::Router> = Box::new(LlmRouter::new(
        Arc::clone(&inference_arc),
        Arc::clone(&store),
        Arc::clone(&skills),
    ));

    let planner = LlmPlanner::new(Arc::clone(&inference_arc), Arc::clone(&skills));

    // Register tools.
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(ShellTool));
    tools.register(Box::new(sovereign_tools::document::DocumentTool::new(
        Arc::clone(&store),
        Arc::clone(&inference_arc),
    )));
    tools.register(Box::new(sovereign_tools::knowledge::KnowledgeTool::new(
        Arc::clone(&store),
        Arc::clone(&inference_arc),
    )));
    tools.register(Box::new(sovereign_tools::web::WebSearchTool::new(
        Arc::clone(&inference_arc),
    )));
    tools.register(Box::new(sovereign_tools::web::WebFetchTool::new()));
    tracing::info!("Tools: {} registered", tools.count());

    // Approval channel.
    let (approval_channel, _event_rx) = ServerApprovalChannel::new();
    let approval = Arc::new(approval_channel);

    let runtime = Arc::new(Runtime::new(
        inference_arc,
        router,
        Box::new(planner),
        Arc::new(tools),
        store,
        skills,
        approval.clone() as Arc<dyn sovereign_core::traits::ApprovalChannel>,
    ));

    // Auth state.
    let auth_state = if config.auth.mode == "api_key" && !config.auth.keys.is_empty() {
        tracing::info!("Auth: API key ({} keys configured)", config.auth.keys.len());
        AuthState::new(config.auth.keys.clone())
    } else {
        tracing::info!("Auth: disabled");
        AuthState::disabled()
    };

    // Build Axum router.
    let app = axum::Router::new()
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
        .layer(middleware::from_fn(auth::auth_middleware))
        .layer(Extension(auth_state))
        .layer(Extension(runtime))
        .layer(Extension(approval))
        .layer(CorsLayer::permissive());

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

    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("Server error: {e}");
        std::process::exit(1);
    }
}
