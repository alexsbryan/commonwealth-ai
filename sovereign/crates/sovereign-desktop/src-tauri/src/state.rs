use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use corpus_engine::CorpusEngine;

use sovereign_core::health_monitor::{HealthMonitor, MonitorConfig};
use sovereign_core::insight::{InsightService, InsightSinkRegistry};
use sovereign_core::planner::LlmPlanner;
use sovereign_core::router::LlmRouter;
use sovereign_core::runtime::Runtime;
use sovereign_core::traits::{InferenceProvider, InsightStore, StateStore};
use sovereign_core::types::InferenceConfig;
use sovereign_core::{SkillRegistry, ToolRegistry};
use sovereign_inference::embedded::EmbeddedLlamaCpp;
use sovereign_store::insight_store::SqliteInsightStore;
use sovereign_store::sqlite::SqliteStateStore;
use sovereign_tools::index_validator::EmbedSlotConfig;
use sovereign_tools::shell::ShellTool;
use tokio_util::sync::CancellationToken;

use crate::approval::TauriApprovalChannel;

// ─── Desktop Config ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopConfig {
    pub model_path: PathBuf,
    #[serde(default)]
    pub primary_model_path: Option<PathBuf>,
    /// Optional dedicated embedding model. Required for any feature
    /// that needs vector search: corpus install, RAG, knowledge tools.
    /// When unset, those features return a clear "configure an
    /// embedding model" error rather than producing garbage vectors.
    #[serde(default)]
    pub embed_model_path: Option<PathBuf>,
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

    // ── Advanced Tuning ─────────────────────────────────────────
    /// Generation temperature (0.0–1.0). Higher = more creative, lower = more focused.
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    /// Maximum tokens to generate per response.
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// Maximum tokens allowed inside a `<think>` block before it is
    /// force-closed, preventing the model from spiralling indefinitely.
    #[serde(default = "default_think_budget")]
    pub think_budget: u32,
    /// Top-k sampling. `None` defers to the model family default.
    /// Bundled with `temperature` in the Creativity preset selector.
    #[serde(default)]
    pub top_k: Option<u32>,
    /// Epistemic humility mode. When true, the runtime audits each
    /// answer for thin evidence and surfaces an `InformationRequest`
    /// card asking the user to paste a source. Default **on**; see
    /// `sovereign_core::types::InferenceConfig::auto_collaborate` for
    /// the full story. Named `#[serde(default = …)]` so existing
    /// saved configs without the field also upgrade to on.
    #[serde(default = "default_auto_collaborate")]
    pub auto_collaborate: bool,
}

fn default_auto_collaborate() -> bool {
    true
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

fn default_temperature() -> f32 {
    0.7
}

fn default_max_tokens() -> u32 {
    2048
}

fn default_think_budget() -> u32 {
    512
}

fn default_search_provider() -> String {
    "duckduckgo".to_string()
}

impl Default for DesktopConfig {
    fn default() -> Self {
        Self {
            model_path: PathBuf::from("models/fast.gguf"),
            primary_model_path: None,
            embed_model_path: None,
            data_dir: default_data_dir(),
            skills_dir: default_skills_dir(),
            active_skills: Vec::new(),
            enabled_tools: default_enabled_tools(),
            context_size: default_context_size(),
            search_backend: SearchBackendConfig::default(),
            setup_complete: false,
            selected_tier: None,
            temperature: default_temperature(),
            max_tokens: default_max_tokens(),
            think_budget: default_think_budget(),
            top_k: None,
            auto_collaborate: default_auto_collaborate(),
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
    /// Background health monitor. Populated during bootstrap; None before first boot.
    pub health_monitor: RwLock<Option<Arc<HealthMonitor>>>,
    /// CancellationToken to shut down the health monitor on exit.
    pub health_shutdown: CancellationToken,
    /// Insight capture service. Created during bootstrap from the same
    /// SQLite connection as the state store.
    pub insight_service: RwLock<Option<Arc<InsightService>>>,
}

impl AppState {
    pub fn new(approval: Arc<TauriApprovalChannel>) -> Self {
        let config = DesktopConfig::load();
        // The mesh daemon persists its running-mesh state into
        // `<data_dir>/mesh.json` so a create/join survives an app
        // restart — otherwise the founder loses their mesh on quit
        // and would-be joiners get "no peer on this network".
        let mesh_data_dir = config.data_dir.clone();
        Self {
            runtime: RwLock::new(None),
            approval,
            config: RwLock::new(config),
            inference: RwLock::new(None),
            store: RwLock::new(None),
            corpus_engine: RwLock::new(None),
            install_progress: RwLock::new(HashMap::new()),
            mesh: Arc::new(sovereign_mesh::EmbeddedDaemon::new(mesh_data_dir)),
            health_monitor: RwLock::new(None),
            health_shutdown: CancellationToken::new(),
            insight_service: RwLock::new(None),
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

    // Load inference (reuse if already loaded). Loads up to three slots:
    // fast (always), primary (optional, lazy), embed (optional, eager).
    let inference: Arc<dyn InferenceProvider> = {
        let existing = state.inference.read().await;
        if let Some(ref inf) = *existing {
            Arc::clone(inf)
        } else {
            drop(existing);
            tracing::info!("Loading fast model: {}", config.model_path.display());
            if let Some(ref ep) = config.embed_model_path {
                tracing::info!("Loading embed model: {}", ep.display());
            } else {
                tracing::warn!(
                    "No embedding model configured. Corpus install and RAG features \
                     will be unavailable until you set Settings → Embedding model."
                );
            }
            let loaded = Arc::new(
                EmbeddedLlamaCpp::load_full(
                    &config.model_path,
                    config.primary_model_path.as_deref(),
                    config.embed_model_path.as_deref(),
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
            let sqlite_store = SqliteStateStore::open(&db_path)
                .map_err(|e| format!("Failed to open database: {e}"))?;

            // Create insight store sharing the same connection.
            let insight_store: Arc<dyn InsightStore> =
                Arc::new(SqliteInsightStore::new(sqlite_store.connection()));
            let insight_service = Arc::new(InsightService::new(
                insight_store,
                Arc::new(InsightSinkRegistry::new()),
                Arc::clone(&inference),
            ));
            *state.insight_service.write().await = Some(insight_service);

            let s: Arc<dyn StateStore> = Arc::new(sqlite_store);
            *state.store.write().await = Some(Arc::clone(&s));
            s
        }
    };

    // Load skills. Two sources:
    //
    //   1. **Built-in skills** — shipped with the binary via
    //      `include_str!` (see `BUILTIN_SKILLS` below). Always
    //      available regardless of install / CWD / Tauri resource
    //      resolution. The whole point: the Settings → Skills panel
    //      should never read "No skills found." on a fresh install.
    //
    //   2. **User skills dir** — `config.skills_dir` (e.g. on macOS
    //      `~/Library/Application Support/sovereign/skills`). Loaded
    //      after built-ins so user versions can override built-ins
    //      with the same id (SkillRegistry keeps the last registered).
    //
    // Debug-mode escape hatch: when the workspace `skills/` directory
    // exists (we're running `cargo tauri dev` from the repo), load
    // anything we haven't already embedded — makes iterating on new
    // built-ins painless without recompiling the binary.
    let mut skills = SkillRegistry::new();
    register_builtin_skills(&mut skills);
    #[cfg(debug_assertions)]
    {
        if let Some(workspace_skills) = dev_workspace_skills_dir() {
            if workspace_skills.exists() {
                tracing::info!(
                    "Loading dev-only skills overlay from {}",
                    workspace_skills.display()
                );
                skills.load_and_register(&workspace_skills);
            }
        }
    }
    if config.skills_dir.exists() && config.skills_dir != std::path::PathBuf::new() {
        skills.load_and_register(&config.skills_dir);
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
        let approval_for_doc = Arc::clone(&state.approval);
        tools.register(Box::new(
            sovereign_tools::DocumentOperationTool::new(
                Arc::clone(&store),
                Arc::clone(&inference),
            )
            .with_progress(Arc::new(move |p| {
                approval_for_doc.emit_event("document-progress", &p);
            })),
        ));
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
    let batch_embed_fn =
        sovereign_tools::corpus::inference_to_batch_embed_fn(Arc::clone(&inference));
    let inference_fn =
        sovereign_tools::corpus::inference_to_inference_fn(Arc::clone(&inference));
    // Derive the embedding model identifier from the configured file path so
    // _corpus_meta.json records the actual model rather than the hardcoded
    // "nomic-embed-text-v2" default.  We use the filename stem (without .gguf)
    // as a stable, human-readable identifier.
    let embed_model_name = config
        .embed_model_path
        .as_ref()
        .and_then(|p| p.file_stem())
        .and_then(|s| s.to_str())
        .unwrap_or("unknown-embed-model")
        .to_string();
    let corpus_engine = Arc::new(
        corpus_engine::CorpusEngine::new(recipes_dir, indexes_dir, embed_fn)
            .with_embedding_model(&embed_model_name)
            .with_batch_embed_fn(batch_embed_fn)
            .with_inference_fn(inference_fn),
    );
    *state.corpus_engine.write().await = Some(Arc::clone(&corpus_engine));

    // Startup dimension guard: probe the loaded embed model's actual output
    // size and compare against every installed corpus index. A mismatch means
    // the user swapped embed models after building their library — retrieval
    // will silently return wrong results unless they rebuild.
    if config.embed_model_path.is_some() {
        match inference.embed("probe").await {
            Ok(probe_vec) => {
                let dims = probe_vec.len();
                if let Err(e) = corpus_engine.validate_embed_dimensions(dims).await {
                    tracing::warn!(
                        "Embed dimension mismatch detected at startup: {} \
                         Retrieval results may be incorrect until the affected \
                         corpus is rebuilt (Settings → Knowledge → Rebuild).",
                        e
                    );
                }
            }
            Err(_) => {} // embed not configured or failed — skip validation
        }
    }

    // ── Health Monitor ────────────────────────────────────────────────────────
    // Only build the monitor once (it survives Runtime rebuilds).
    if state.health_monitor.read().await.is_none() {
        let embed_dims = config
            .embed_model_path
            .as_ref()
            .map(|_| {
                // If we successfully embedded a probe above, use that dimension.
                // Fall back to a reasonable default — the checker will detect mismatches.
                0usize
            })
            .unwrap_or(0);
        let embed_slot = Arc::new(tokio::sync::RwLock::new(EmbedSlotConfig {
            model_id: embed_model_name.clone(),
            output_dims: embed_dims,
        }));

        let monitor = Arc::new(HealthMonitor::new(MonitorConfig::default(), Arc::clone(&store)));

        // Register CorpusIndexChecker.
        monitor
            .register(Arc::new(sovereign_tools::index_validator::CorpusIndexChecker::new(
                Arc::clone(&corpus_engine),
                Arc::clone(&embed_slot),
            )))
            .await;

        // Register EnrichmentChecker.
        monitor
            .register(Arc::new(sovereign_tools::enrichment_checker::EnrichmentChecker::new(
                Arc::clone(&corpus_engine),
            )))
            .await;

        // Register StateStoreChecker (SQLite only).
        let db_path = config.data_dir.join("sovereign.db");
        if let Ok(sqlite_store) = SqliteStateStore::open(&db_path) {
            monitor
                .register(Arc::new(
                    sovereign_store::state_store_checker::StateStoreChecker::new(
                        Arc::new(sqlite_store),
                        db_path,
                    ),
                ))
                .await;
        }

        // Register RouterCircuitChecker.
        // Only wire if HybridProvider exposes a primary health tracker.
        // We use the inference provider directly for probe completion.
        // Wire RouterCircuitChecker with a standalone HealthTracker.
        // The monitor probes the inference provider on repair to test liveness.
        {
            let tracker = Arc::new(sovereign_inference::health::HealthTracker::new());
            monitor
                .register(Arc::new(
                    sovereign_inference::router_circuit::RouterCircuitChecker::new(
                        Arc::clone(&tracker),
                        Arc::clone(&inference),
                    ),
                ))
                .await;
        }

        let m = Arc::clone(&monitor);
        let shutdown = state.health_shutdown.clone();
        tokio::spawn(async move { m.run(shutdown).await });
        *state.health_monitor.write().await = Some(monitor);
        tracing::info!("HealthMonitor started");
    }

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
    tracing::info!("Tools: {} registered", tools.count());

    let approval: Arc<dyn sovereign_core::traits::ApprovalChannel> =
        Arc::clone(&state.approval) as Arc<dyn sovereign_core::traits::ApprovalChannel>;

    let inference_config = {
        let cfg = state.config.read().await;
        InferenceConfig {
            temperature: cfg.temperature,
            max_tokens: cfg.max_tokens as usize,
            think_budget: cfg.think_budget as usize,
            top_k: cfg.top_k,
            auto_collaborate: cfg.auto_collaborate,
        }
    };

    let runtime = Runtime::new(
        inference,
        router,
        Box::new(planner),
        Arc::new(tools),
        Arc::clone(&store),
        skills,
        approval,
        inference_config,
    )
    .with_corpus_engine(Arc::clone(&corpus_engine));

    *state.runtime.write().await = Some(Arc::new(runtime));

    // Auto-resume a previously-persisted mesh so the founder sees
    // their mesh on restart and existing joiners pick up where they
    // left off. Fails soft — a missing or corrupt mesh.json never
    // blocks startup.
    match state.mesh.try_resume().await {
        Ok(true) => tracing::info!("mesh: resumed from persisted state"),
        Ok(false) => tracing::debug!("mesh: no persisted state, starting fresh"),
        Err(e) => tracing::warn!(error = %e, "mesh: try_resume failed"),
    }

    // Background startup task: verify per-corpus vector index readiness and
    // write results to the store so handle_knowledge_query can gate correctly.
    {
        let verify_store = Arc::clone(&store);
        let verify_engine = Arc::clone(&corpus_engine);
        tokio::spawn(async move {
            let corpora = verify_store.list_corpus_states().await.unwrap_or_default();
            for cs in corpora {
                let Ok(indexes) = verify_engine.installed_indexes().await else { continue };
                let Some(info) = indexes.iter().find(|i| i.corpus_id == cs.corpus_id) else {
                    continue
                };
                let Ok(idx) = verify_engine.open_index(&info.path).await else { continue };
                let ready = idx.is_vector_index_ready().await;
                let _ = verify_store
                    .set_vector_index_ready(&cs.corpus_id, ready)
                    .await;
                if !ready {
                    tracing::warn!(
                        corpus = %cs.corpus_id,
                        "Vector index not built — KnowledgeQuery will use FTS-only search"
                    );
                } else {
                    tracing::info!(corpus = %cs.corpus_id, "Vector index ready");
                }
            }
        });
    }

    tracing::info!("Runtime ready");
    Ok(())
}

/// Rebuild the Runtime with updated config. Reuses the loaded model and database.
pub async fn rebuild_runtime(state: &AppState) -> Result<(), String> {
    // Drop existing runtime first to release Arc references.
    *state.runtime.write().await = None;
    bootstrap(state).await
}

/// Skills shipped with the binary. Each entry is the raw `skill.toml`
/// contents embedded at compile time via `include_str!`. This keeps
/// the Settings → Skills panel populated on every fresh install
/// regardless of filesystem layout, and survives Tauri bundle
/// repackaging without needing `bundle.resources` plumbing.
///
/// To add a new built-in skill:
///   1. Drop a `skill.toml` under `<repo>/skills/<name>/`.
///   2. Add a matching `include_str!("../../../../skills/<name>/skill.toml")`
///      entry here.
/// User-created skills live under `config.skills_dir` and are loaded
/// alongside these at runtime.
const BUILTIN_SKILLS: &[&str] = &[
    include_str!("../../../../skills/collaborative-research/skill.toml"),
    include_str!("../../../../skills/code-review/skill.toml"),
    include_str!("../../../../skills/codebase-navigator/skill.toml"),
    include_str!("../../../../skills/document-analyst/skill.toml"),
    include_str!("../../../../skills/epistemic-research/skill.toml"),
    include_str!("../../../../skills/inner-work/skill.toml"),
    include_str!("../../../../skills/personal-assistant/skill.toml"),
    include_str!("../../../../skills/research-analyst/skill.toml"),
];

fn register_builtin_skills(skills: &mut SkillRegistry) {
    for (idx, toml) in BUILTIN_SKILLS.iter().enumerate() {
        match sovereign_core::skills::parse_skill_toml(toml) {
            Some(skill) => skills.register(skill),
            None => tracing::warn!(
                idx,
                "built-in skill #{idx}: failed to parse skill.toml — skipping"
            ),
        }
    }
}

/// Debug-only: look up the workspace `skills/` directory so developers
/// running `cargo tauri dev` can add a new skill TOML without needing
/// to rebuild the binary with a new `include_str!` entry. Returns
/// `None` outside the workspace layout (e.g. an installed debug build).
#[cfg(debug_assertions)]
fn dev_workspace_skills_dir() -> Option<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    Some(
        manifest
            .parent()? // crates/sovereign-desktop/
            .parent()? // crates/
            .parent()? // <repo root>
            .join("skills"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_config_without_auto_collaborate_upgrades_to_on() {
        // Users who upgraded from the Phase 2 build have a TOML config
        // that omits `auto_collaborate`. Without the named-default
        // serde helper they'd silently get `false` (bool::default()),
        // losing the feature. Guard against that regression.
        let legacy = r#"
model_path = "/some/model.gguf"
data_dir = "/some/data"
skills_dir = "/some/skills"
active_skills = []
enabled_tools = []
context_size = 2048
setup_complete = true
temperature = 0.7
max_tokens = 2048
think_budget = 512

[search_backend]
provider = "duckduckgo"
"#;
        let cfg: DesktopConfig = toml::from_str(legacy)
            .expect("legacy config should deserialize");
        assert!(
            cfg.auto_collaborate,
            "legacy config (no auto_collaborate field) must upgrade to true"
        );
    }

    #[test]
    fn default_desktop_config_has_auto_collaborate_on() {
        let cfg = DesktopConfig::default();
        assert!(
            cfg.auto_collaborate,
            "DesktopConfig::default().auto_collaborate must be true"
        );
    }

    #[test]
    fn builtin_skills_all_parse() {
        // include_str! paths are resolved at compile time, but the
        // skill.toml contents must still parse at runtime. Require
        // that EVERY built-in skill parses — a malformed one would
        // silently be skipped at runtime and the user would see a
        // shorter-than-expected Skills list with no explanation.
        //
        // A previous build had 7/8 TOMLs using PascalCase privacy
        // variants that serde (`rename_all = "snake_case"`) rejected;
        // `builtin_skills_all_parse` with a `>= 1` assertion let the
        // bug ship. Keep this strict.
        let mut reg = sovereign_core::SkillRegistry::new();
        register_builtin_skills(&mut reg);
        assert_eq!(
            reg.list().len(),
            BUILTIN_SKILLS.len(),
            "every built-in skill.toml must parse successfully; \
             registered {} of {} — check logs for the malformed entries",
            reg.list().len(),
            BUILTIN_SKILLS.len(),
        );
    }

    #[test]
    fn registering_same_skill_twice_does_not_duplicate() {
        // In dev builds, `bootstrap()` first registers built-ins via
        // `include_str!` and then loads the workspace `skills/` directory
        // as a live overlay. If these two paths register the same skill
        // id, the registry must treat the second as an *override*, not
        // an append. Svelte's `{#each (skill.id)}` crashes on duplicate
        // keys and bails mid-render — users saw "Loading skills…"
        // freeze on screen in browser console `each_key_duplicate`.
        let mut reg = sovereign_core::SkillRegistry::new();
        register_builtin_skills(&mut reg);
        register_builtin_skills(&mut reg); // duplicate pass
        assert_eq!(
            reg.list().len(),
            BUILTIN_SKILLS.len(),
            "registering the same built-ins twice must not double the count"
        );
        let mut ids: Vec<&str> = reg.list().iter().map(|s| s.id.as_str()).collect();
        ids.sort();
        let before_dedupe = ids.len();
        ids.dedup();
        assert_eq!(
            ids.len(),
            before_dedupe,
            "registry must contain no duplicate ids after double-registration"
        );
    }
}
