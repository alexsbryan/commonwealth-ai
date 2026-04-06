use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use corpus_engine::CorpusEngine;

use sovereign_core::planner::LlmPlanner;
use sovereign_core::router::LlmRouter;
use sovereign_core::runtime::Runtime;
use sovereign_core::traits::{InferenceProvider, StateStore};
use sovereign_core::{SkillRegistry, ToolRegistry};
use sovereign_inference::embedded::EmbeddedLlamaCpp;
use sovereign_store::sqlite::SqliteStateStore;
use sovereign_tools::shell::ShellTool;

use crate::approval::TauriApprovalChannel;

// ─── Desktop Config ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopConfig {
    pub model_path: PathBuf,
    #[serde(default)]
    pub primary_model_path: Option<PathBuf>,
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    #[serde(default = "default_skills_dir")]
    pub skills_dir: PathBuf,
    #[serde(default)]
    pub active_skills: Vec<String>,
    #[serde(default = "default_enabled_tools")]
    pub enabled_tools: Vec<String>,
    #[serde(default = "default_context_size")]
    pub context_size: u32,
    #[serde(default)]
    pub search_backend: SearchBackendConfig,
    #[serde(default)]
    pub setup_complete: bool,
    #[serde(default)]
    pub selected_tier: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchBackendConfig {
    #[serde(default = "default_search_provider")]
    pub provider: String,
    #[serde(default)]
    pub api_key: Option<String>,
}

fn default_data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("sovereign")
}

fn default_skills_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("sovereign")
        .join("skills")
}

fn default_enabled_tools() -> Vec<String> {
    vec![
        "shell".to_string(),
        "search".to_string(),
        "web_fetch".to_string(),
        "document".to_string(),
    ]
}

fn default_context_size() -> u32 {
    2048
}

fn default_search_provider() -> String {
    "duckduckgo".to_string()
}

impl Default for DesktopConfig {
    fn default() -> Self {
        Self {
            model_path: PathBuf::from("models/fast.gguf"),
            primary_model_path: None,
            data_dir: default_data_dir(),
            skills_dir: default_skills_dir(),
            active_skills: Vec::new(),
            enabled_tools: default_enabled_tools(),
            context_size: default_context_size(),
            search_backend: SearchBackendConfig::default(),
            setup_complete: false,
            selected_tier: None,
        }
    }
}

impl DesktopConfig {
    pub fn config_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("sovereign")
            .join("desktop.toml")
    }

    pub fn load() -> Self {
        let path = Self::config_path();
        if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(content) => match toml::from_str(&content) {
                    Ok(config) => return config,
                    Err(e) => tracing::warn!("Failed to parse config: {e}"),
                },
                Err(e) => tracing::warn!("Failed to read config: {e}"),
            }
        }
        Self::default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create config dir: {e}"))?;
        }
        let content =
            toml::to_string_pretty(self).map_err(|e| format!("Failed to serialize config: {e}"))?;
        std::fs::write(&path, content).map_err(|e| format!("Failed to write config: {e}"))?;
        Ok(())
    }
}

// ─── App State ───────────────────────────────────────────────

pub struct AppState {
    pub runtime: RwLock<Option<Arc<Runtime>>>,
    pub approval: Arc<TauriApprovalChannel>,
    pub config: RwLock<DesktopConfig>,
    /// Reusable across Runtime rebuilds (model stays loaded).
    pub inference: RwLock<Option<Arc<dyn InferenceProvider>>>,
    pub store: RwLock<Option<Arc<dyn StateStore>>>,
    /// The shared corpus engine. Set during bootstrap and used by both
    /// the install/list/remove Tauri commands and the in-runtime
    /// epistemic tools (`ClaimSearchTool`, `EpistemicLandscapeTool`).
    /// Built-in recipes ship as Rust source via `builtin_recipes()` —
    /// no sidecar TOML or build-time `include_str!` magic.
    pub corpus_engine: RwLock<Option<Arc<CorpusEngine>>>,
    pub install_progress: RwLock<HashMap<String, crate::commands::CorpusProgressPayload>>,
    /// Embedded Commonwealth daemon — started on-demand when the user
    /// creates or joins a mesh.
    pub mesh: Arc<sovereign_mesh::EmbeddedDaemon>,
}

impl AppState {
    pub fn new(approval: Arc<TauriApprovalChannel>) -> Self {
        let config = DesktopConfig::load();
        Self {
            runtime: RwLock::new(None),
            approval,
            config: RwLock::new(config),
            inference: RwLock::new(None),
            store: RwLock::new(None),
            corpus_engine: RwLock::new(None),
            install_progress: RwLock::new(HashMap::new()),
            mesh: Arc::new(sovereign_mesh::EmbeddedDaemon::new()),
        }
    }
}

/// Bootstrap the Runtime from the current config. Loads the model if not
/// already loaded, opens the database, wires up skills/tools/routing.
pub async fn bootstrap(state: &AppState) -> Result<(), String> {
    let config = state.config.read().await.clone();

    if !config.model_path.exists() {
        return Err(format!(
            "Model not found: {}. Place a GGUF model file at this path.",
            config.model_path.display()
        ));
    }

    // Load inference (reuse if already loaded).
    let inference: Arc<dyn InferenceProvider> = {
        let existing = state.inference.read().await;
        if let Some(ref inf) = *existing {
            Arc::clone(inf)
        } else {
            drop(existing);
            tracing::info!("Loading model: {}", config.model_path.display());
            let loaded = Arc::new(
                EmbeddedLlamaCpp::load_dual(
                    &config.model_path,
                    config.primary_model_path.as_deref(),
                    config.context_size,
                    None,
                )
                .map_err(|e| format!("Failed to load model: {e}"))?,
            );

            if config.primary_model_path.is_some() {
                loaded.start_idle_monitor(60);
            }

            let inf: Arc<dyn InferenceProvider> = loaded;
            *state.inference.write().await = Some(Arc::clone(&inf));
            inf
        }
    };

    // Open database.
    let store: Arc<dyn StateStore> = {
        let existing = state.store.read().await;
        if let Some(ref s) = *existing {
            Arc::clone(s)
        } else {
            drop(existing);
            let db_path = config.data_dir.join("sovereign.db");
            if let Some(parent) = db_path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create data dir: {e}"))?;
            }
            tracing::info!("Database: {}", db_path.display());
            let s: Arc<dyn StateStore> = Arc::new(
                SqliteStateStore::open(&db_path)
                    .map_err(|e| format!("Failed to open database: {e}"))?,
            );
            *state.store.write().await = Some(Arc::clone(&s));
            s
        }
    };

    // Load skills.
    let mut skills = SkillRegistry::new();
    if config.skills_dir.exists() {
        skills.load_and_register(&config.skills_dir);
    }
    // Also load bundled skills from the working directory.
    let bundled = std::env::current_dir().unwrap_or_default().join("skills");
    if bundled.exists() && bundled != config.skills_dir {
        skills.load_and_register(&bundled);
    }
    // Activate configured skills (or all if none specified).
    if config.active_skills.is_empty() {
        skills.activate_all();
    } else {
        for id in &config.active_skills {
            skills.activate(id);
        }
    }
    tracing::info!("Skills: {} loaded", skills.list().len());
    let skills = Arc::new(skills);

    // Router.
    let router: Box<dyn sovereign_core::traits::Router> = Box::new(LlmRouter::new(
        Arc::clone(&inference),
        Arc::clone(&store),
        Arc::clone(&skills),
    ));

    // Planner.
    let planner = LlmPlanner::new(Arc::clone(&inference), Arc::clone(&skills));

    // Tools.
    let mut tools = ToolRegistry::new();
    let enabled = &config.enabled_tools;

    if enabled.iter().any(|t| t == "shell") {
        tools.register(Box::new(ShellTool));
    }
    if enabled.iter().any(|t| t == "document") {
        tools.register(Box::new(sovereign_tools::document::DocumentTool::new(
            Arc::clone(&store),
            Arc::clone(&inference),
        )));
    }
    if enabled.iter().any(|t| t == "search" || t == "knowledge" || t == "web_search") {
        let backend = match config.search_backend.provider.as_str() {
            "tavily" => {
                if let Some(ref key) = config.search_backend.api_key {
                    sovereign_tools::web::search::SearchBackend::Tavily {
                        api_key: key.clone(),
                    }
                } else {
                    sovereign_tools::web::search::SearchBackend::DuckDuckGo
                }
            }
            "brave" => {
                if let Some(ref key) = config.search_backend.api_key {
                    sovereign_tools::web::search::SearchBackend::Brave {
                        api_key: key.clone(),
                    }
                } else {
                    sovereign_tools::web::search::SearchBackend::DuckDuckGo
                }
            }
            _ => sovereign_tools::web::search::SearchBackend::DuckDuckGo,
        };
        tools.register(Box::new(sovereign_tools::search::SearchTool::with_web(
            Arc::clone(&store),
            Arc::clone(&inference),
            backend,
        )));
    }
    if enabled.iter().any(|t| t == "web_fetch") {
        tools.register(Box::new(sovereign_tools::web::WebFetchTool::new()));
    }

    // Construct a shared CorpusEngine. This single instance backs both
    // the install/list/remove Tauri commands AND the in-runtime epistemic
    // tools — there's no second corpus subsystem.
    //
    // Built-in recipes (Wikipedia, SEP, OpenAlex, …) live in Rust source
    // via `corpus_engine::recipe::builtin_recipes()`. Users can drop
    // additional `.toml` files into `~/.sovereign/recipes` for custom
    // corpora; nothing is bundled at build time.
    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    let recipes_dir = home.join(".sovereign").join("recipes");
    let indexes_dir = home.join(".sovereign").join("indexes");
    let embed_fn = sovereign_tools::corpus::inference_to_embed_fn(Arc::clone(&inference));
    let inference_fn =
        sovereign_tools::corpus::inference_to_inference_fn(Arc::clone(&inference));
    let corpus_engine = Arc::new(
        corpus_engine::CorpusEngine::new(recipes_dir, indexes_dir, embed_fn)
            .with_inference_fn(inference_fn),
    );
    *state.corpus_engine.write().await = Some(Arc::clone(&corpus_engine));

    tools.register(Box::new(sovereign_tools::ClaimSearchTool::new(
        Arc::clone(&corpus_engine),
    )));
    tools.register(Box::new(sovereign_tools::EpistemicLandscapeTool::new(
        Arc::clone(&corpus_engine),
    )));
    tracing::info!("Tools: {} registered", tools.count());

    let approval: Arc<dyn sovereign_core::traits::ApprovalChannel> =
        Arc::clone(&state.approval) as Arc<dyn sovereign_core::traits::ApprovalChannel>;

    let runtime = Runtime::new(
        inference,
        router,
        Box::new(planner),
        Arc::new(tools),
        store,
        skills,
        approval,
    );

    *state.runtime.write().await = Some(Arc::new(runtime));

    tracing::info!("Runtime ready");
    Ok(())
}

/// Rebuild the Runtime with updated config. Reuses the loaded model and database.
pub async fn rebuild_runtime(state: &AppState) -> Result<(), String> {
    // Drop existing runtime first to release Arc references.
    *state.runtime.write().await = None;
    bootstrap(state).await
}
