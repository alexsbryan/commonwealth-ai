use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use corpus_engine::CorpusEngine;

use sovereign_core::health_monitor::{HealthMonitor, MonitorConfig};
use sovereign_core::insight::{InsightService, InsightSinkRegistry};
use sovereign_core::model_family::{EmbedModelInfo, ModelFamily, NormalizationStrategy, PoolingStrategy};
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
use sovereign_tools::local_corpus::LocalCorpusManager;
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
    /// PR-E2: Optional Code specialist GGUF. When set, `code`-
    /// hinted inference requests route to this slot instead of the
    /// Main responder (primary). Lazy-loaded on first use; idle-
    /// unloads after 60s, same as primary. `None` (the common
    /// case) means the Main responder handles code too — a
    /// well-rounded general model does this adequately per v0.3
    /// §4.4 guidance.
    #[serde(default)]
    pub code_model_path: Option<PathBuf>,
    /// Model family of the code slot — drives tokenizer / chat
    /// template quirks. Typically `Qwen35` for Qwen Coder lineage,
    /// `Unknown` for BYOM coders.
    #[serde(default)]
    pub code_family: ModelFamily,
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

    /// Model family of the embed slot. Controls pooling strategy (mean /
    /// last-token / cls) and instruction prefixes. For most open-weights
    /// embedding models this should be:
    ///   - `Qwen3Embedding` for qwen3-embedding-* GGUF files (last-token pooling)
    ///   - `Unknown` (default) for mxbai and similar mean-pooling
    ///     embedders
    ///
    /// Getting this wrong does not prevent ingestion but will produce
    /// incompatible vectors if you later try to collaborate with a peer
    /// that has it set correctly.
    #[serde(default)]
    pub embed_family: ModelFamily,

    /// Display name used for this node when creating or joining a
    /// mesh — shows up in other members' mesh rosters. Empty string
    /// means "use the system hostname at join time". The user can
    /// override this from Settings → Mesh; changes take effect on
    /// the next mesh create/join, not retroactively (existing
    /// `MemberRecord`s stay put until that member rejoins).
    #[serde(default)]
    pub node_name: String,

    /// Whether the `KnowledgeView` landscape-digest layer is active.
    /// When `true` (default), Sovereign builds + maintains three
    /// enriched views over memories / conversations / notes and
    /// splices their digest into every conversation's system prompt.
    /// When `false`, the feature is skipped at Runtime construction —
    /// Sovereign behaves exactly as it did before KnowledgeView existed.
    ///
    /// Toggling this requires a desktop restart: the Runtime is built
    /// once at app startup, with or without the landscape-digest
    /// provider. The setting persists across restarts.
    ///
    /// Default: on. Existing configs without the field read as `true`.
    #[serde(default = "default_knowledge_view_enabled")]
    pub knowledge_view_enabled: bool,
}

fn default_knowledge_view_enabled() -> bool {
    true
}

fn default_auto_collaborate() -> bool {
    true
}

/// Resolve the node name the user sees in others' mesh rosters.
/// Preference order: explicit config override → system hostname
/// (via the `hostname` crate, which wraps `gethostname(2)` on
/// Unix and `GetComputerNameW` on Windows) → literal fallback.
/// The hostname path handles macOS properly, where `HOSTNAME`
/// env var isn't exported to launched apps.
pub fn resolve_node_name(configured: &str) -> String {
    let trimmed = configured.trim();
    if !trimmed.is_empty() {
        return trimmed.to_string();
    }
    match hostname::get() {
        Ok(os) => os
            .into_string()
            .ok()
            // Strip `.local` Bonjour suffix so "Alexs-MBP.local"
            // renders as "Alexs-MBP" in the roster.
            .map(|s| {
                s.strip_suffix(".local")
                    .map(|trimmed| trimmed.to_string())
                    .unwrap_or(s)
            })
            .unwrap_or_else(|| "sovereign-node".to_string()),
        Err(_) => "sovereign-node".to_string(),
    }
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
    // 8192 ≈ 8K tokens — ample headroom for KnowledgeView Phase 2
    // entity-extraction prompts (~1848 tokens each) plus a 2K
    // generation budget without invoking the
    // `max_tokens exceeded context headroom` clamp. Previously 2048,
    // which silently truncated entity-extraction outputs to ~200
    // tokens and degraded enrichment quality.
    //
    // Both fast and primary slots share this — the ~600-900 MB extra
    // KV cache for a Qwen3.5-9B primary at 8K is comfortable on
    // anything ≥ 16 GB unified memory. Asymmetry note:
    // `sovereign-core::setup_config::default_context_size` returns
    // 16384 for the daemon path; desktop is now closer but still
    // conservative. Users on 64 GB+ machines can safely bump this
    // higher via Settings.
    8192
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
            code_model_path: None,
            code_family: ModelFamily::Unknown,
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
            embed_family: ModelFamily::Unknown,
            node_name: String::new(),
            knowledge_view_enabled: default_knowledge_view_enabled(),
        }
    }
}

/// Marker file recording that the friendly-name first-launch
/// generator has run for this user. Held next to `desktop.toml`
/// so wiping config also resets the sentinel — letting the user
/// recover a fresh suggestion by deleting both files.
fn sentinel_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("sovereign")
        .join(".first-name-generated")
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
        let mut config: DesktopConfig = if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(content) => match toml::from_str(&content) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("Failed to parse config: {e}");
                        Self::default()
                    }
                },
                Err(e) => {
                    tracing::warn!("Failed to read config: {e}");
                    Self::default()
                }
            }
        } else {
            Self::default()
        };

        // Migration: bump persisted context_size below the new default.
        // Older configs were saved with context_size = 2048, which
        // caused the `max_tokens exceeded context headroom` clamp on
        // KnowledgeView Phase 2 entity extraction (1848-token prompts
        // + 200-token output budget). 8192 fixes that without
        // measurable memory impact on supported hardware. Config is
        // re-saved so the migration is one-shot.
        let migrated_default = default_context_size();
        if config.context_size < migrated_default {
            tracing::info!(
                old = config.context_size,
                new = migrated_default,
                "config migration: bumping context_size to new default"
            );
            config.context_size = migrated_default;
            if let Err(e) = config.save() {
                tracing::warn!(
                    "failed to persist context_size migration: {e} \
                     (in-memory value still applied for this run)"
                );
            }
        }

        // Friendly first-launch node name. Without this, anyone who
        // never opened the node-name input ends up identified by their
        // raw system hostname ("Alexs-MacBook-2") in mesh rosters,
        // which is forgettable and easy to mistake for someone else.
        // Generate once and persist; never overwrite a name the user
        // explicitly set, and never re-roll if they later cleared the
        // field on purpose (sentinel guards against that).
        let sentinel = sentinel_path();
        let already_generated = sentinel.exists();
        if config.node_name.trim().is_empty() && !already_generated {
            let suggested = crate::friendly_names::generate(None);
            tracing::info!(
                node_name = %suggested,
                "first-launch friendly node name generated"
            );
            config.node_name = suggested;
            // Persist immediately so the suggestion survives even if
            // the user closes the app before opening MeshSettings.
            // Errors here are non-fatal — the in-memory config is
            // still good for this session.
            if let Err(e) = config.save() {
                tracing::warn!(
                    "failed to persist first-launch node name: {e}"
                );
            } else if let Some(parent) = sentinel.parent() {
                let _ = std::fs::create_dir_all(parent);
                if let Err(e) = std::fs::write(&sentinel, b"1") {
                    tracing::warn!(
                        "failed to write friendly-name sentinel: {e}"
                    );
                }
            }
        }

        config
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
    /// PR2 — sink for interpretation-proposed, clarification-request,
    /// and turn-narration events. Constructed at app setup alongside
    /// `approval` (both wrap the same Tauri AppHandle) and handed to
    /// Runtime::with_routing_events during bootstrap.
    pub routing_events: Arc<crate::routing_events::TauriRoutingEventSink>,
    pub config: RwLock<DesktopConfig>,
    /// Reusable across Runtime rebuilds (model stays loaded).
    pub inference: RwLock<Option<Arc<dyn InferenceProvider>>>,
    pub store: RwLock<Option<Arc<dyn StateStore>>>,
    /// Concrete `Arc<SqliteStateStore>` kept alongside the trait-object
    /// `store` so the KnowledgeView manager can be installed as an
    /// observer via `set_observer` after the store is already Arc-
    /// wrapped. Both handles point at the same underlying DB.
    pub sqlite_store: RwLock<Option<Arc<SqliteStateStore>>>,
    /// The shared corpus engine. Set during bootstrap and used by both
    /// the install/list/remove Tauri commands and the in-runtime
    /// epistemic tools (`ClaimSearchTool`, `EpistemicLandscapeTool`).
    /// Built-in recipes ship as Rust source via `builtin_recipes()` —
    /// no sidecar TOML or build-time `include_str!` magic.
    pub corpus_engine: RwLock<Option<Arc<CorpusEngine>>>,
    pub install_progress: RwLock<HashMap<String, crate::commands::CorpusProgressPayload>>,
    /// Embedded Commonwealth daemon — started on-demand when the user
    /// creates or joins a mesh.
    ///
    /// `None` in Attach mode: the CLI (`sovereign daemon run` under
    /// launchd/systemd) already owns the daemon on `:9741`. Mesh
    /// mutations route through the daemon's HTTP API
    /// (`/v1/mesh/create/join/rotate/leave`) instead of calling
    /// in-process `EmbeddedDaemon` methods. See `crate::bootstrap`.
    pub mesh: Option<Arc<sovereign_mesh::EmbeddedDaemon>>,
    /// How this process bootstrapped. Used by mesh_commands and the UI
    /// badge to decide whether to drive mesh via Rust or HTTP.
    pub bootstrap_mode: crate::bootstrap::BootstrapMode,
    /// Background health monitor. Populated during bootstrap; None before first boot.
    pub health_monitor: RwLock<Option<Arc<HealthMonitor>>>,
    /// CancellationToken to shut down the health monitor on exit.
    pub health_shutdown: CancellationToken,
    /// Insight capture service. Created during bootstrap from the same
    /// SQLite connection as the state store.
    pub insight_service: RwLock<Option<Arc<InsightService>>>,
    /// Manager for locally-sourced corpora — backs the "Local
    /// Knowledge" settings section. `None` until bootstrap completes
    /// and the `CorpusEngine` is ready; commands check this and
    /// surface a "finish setup first" error when unset.
    pub local_corpus: RwLock<Option<Arc<LocalCorpusManager>>>,
}

impl AppState {
    /// True when this process is talking to an external CLI daemon at
    /// `:9741` rather than running its own embedded daemon. The
    /// idiomatic check used to be `state.mesh.is_none()`, but the
    /// connection between "no embedded mesh" and "we are a passive
    /// UI" is non-obvious to readers; use this accessor instead.
    pub fn is_attach_mode(&self) -> bool {
        matches!(
            self.bootstrap_mode,
            crate::bootstrap::BootstrapMode::Attach { .. }
        )
    }

    /// Construct `AppState` branching on the bootstrap mode probed at
    /// app start:
    ///
    /// - `Attach` — a CLI-started daemon already owns `:9741`. We
    ///   skip creating an `EmbeddedDaemon` (it would silently fail to
    ///   bind); mesh mutations land on the daemon's HTTP API.
    /// - `Local` — no daemon running. Create our own `EmbeddedDaemon`
    ///   just like before; it'll be started on demand when the user
    ///   creates or joins a mesh.
    pub fn new_with_mode(
        approval: Arc<TauriApprovalChannel>,
        mode: crate::bootstrap::BootstrapMode,
    ) -> Self {
        let config = DesktopConfig::load();
        // The mesh daemon persists its running-mesh state into
        // `<data_dir>/mesh.json` so a create/join survives an app
        // restart — otherwise the founder loses their mesh on quit
        // and would-be joiners get "no peer on this network".
        let mesh_data_dir = config.data_dir.clone();

        let mesh = match &mode {
            crate::bootstrap::BootstrapMode::Attach { .. } => None,
            crate::bootstrap::BootstrapMode::Local { .. } => {
                Some(Arc::new(sovereign_mesh::EmbeddedDaemon::new(mesh_data_dir)))
            }
        };

        // Mint a routing event sink from the same AppHandle the
        // approval channel uses. Constructing it here (rather than
        // plumbing a second AppHandle through the constructor) keeps
        // the call sites tight and reuses the handle clone.
        let routing_events = Arc::new(
            crate::routing_events::TauriRoutingEventSink::new(approval.app_handle()),
        );

        Self {
            runtime: RwLock::new(None),
            approval,
            routing_events,
            config: RwLock::new(config),
            inference: RwLock::new(None),
            store: RwLock::new(None),
            sqlite_store: RwLock::new(None),
            corpus_engine: RwLock::new(None),
            install_progress: RwLock::new(HashMap::new()),
            mesh,
            bootstrap_mode: mode,
            health_monitor: RwLock::new(None),
            health_shutdown: CancellationToken::new(),
            insight_service: RwLock::new(None),
            local_corpus: RwLock::new(None),
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

    // Load inference. We end up with two distinct provider Arcs:
    //
    //   • `raw_inference` — the plain `EmbeddedLlamaCpp`. Handed to
    //     the embedded Commonwealth daemon so when a PEER POSTs
    //     `/v1/chat/completions` to our `:9741`, their request is
    //     served by this raw local model. NEVER the mesh wrapper,
    //     or a peer hitting us would trigger our own re-routing
    //     loop.
    //
    //   • `inference` — `raw_inference` wrapped in a
    //     `MeshInferenceProvider`. Handed to the Runtime. This is
    //     the one that routes THIS user's synthesis requests to a
    //     beefier mesh peer when one is online. Fast/Medium stays
    //     local; only Slow-slot work crosses the wire.
    //
    // Both share the same underlying weights — there's no double-
    // load. The wrapper is a thin router over an Arc clone.
    let raw_inference: Arc<dyn InferenceProvider> = {
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

            // Crash-isolated smoke test: spawn ourselves with
            // `--smoketest` and run a 1-token decode against the
            // chat slot's GGUF before loading it in-process. If the
            // child SIGSEGVs (e.g., Gemma 4 on Apple Metal in
            // llama-cpp-2 0.1.145, where ggml's Metal kernel-pipeline
            // lookup returns nil and gets dereferenced), set
            // `SOVEREIGN_FORCE_CPU_CHAT=1` for THIS process and
            // continue — the in-process load below will then
            // configure the chat slot with `n_gpu_layers=0`.
            //
            // Skipped silently when `SOVEREIGN_FORCE_CPU_CHAT=1` is
            // already set (the user has already chosen CPU) or when
            // we can't determine GPU layers from hardware (no GPU
            // configured anyway).
            let env_force_cpu = std::env::var("SOVEREIGN_FORCE_CPU_CHAT")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            if !env_force_cpu {
                let smoke_gpu_layers = sovereign_inference::hardware::HardwareProfile::detect()
                    .recommended_gpu_layers;
                if smoke_gpu_layers > 0 {
                    let smoke_ctx = config.context_size.min(2048);
                    tracing::info!(
                        model = %config.model_path.display(),
                        gpu_layers = smoke_gpu_layers,
                        n_ctx = smoke_ctx,
                        "smoketest: probing GPU compatibility before in-process load"
                    );
                    let outcome = crate::smoketest::run_in_subprocess(
                        &config.model_path,
                        smoke_gpu_layers,
                        smoke_ctx,
                        std::time::Duration::from_secs(60),
                    );
                    match &outcome {
                        crate::smoketest::SmokeResult::Ok => {
                            tracing::info!("smoketest: GPU path ok — proceeding");
                        }
                        other if other.suggests_cpu_fallback() => {
                            tracing::error!(
                                outcome = %other,
                                "smoketest: GPU path crashed — falling back to CPU. \
                                 Set SOVEREIGN_FORCE_CPU_CHAT=0 to disable this guard."
                            );
                            // SAFETY: bootstrap runs once, single
                            // task, no concurrent env mutation.
                            // SOVEREIGN_FORCE_CPU_CHAT is read by
                            // sovereign-inference's chat-slot loader
                            // immediately below.
                            std::env::set_var("SOVEREIGN_FORCE_CPU_CHAT", "1");
                        }
                        other => {
                            tracing::warn!(
                                outcome = %other,
                                "smoketest: inconclusive — proceeding with GPU load. \
                                 The model may still load and run normally; this just \
                                 means we couldn't pre-confirm it."
                            );
                        }
                    }
                }
            }

            let loaded = Arc::new(
                EmbeddedLlamaCpp::load_full_with_families(
                    &config.model_path,
                    config.primary_model_path.as_deref(),
                    config.embed_model_path.as_deref(),
                    config.code_model_path.as_deref(),
                    config.context_size,
                    None,
                    ModelFamily::Unknown,               // fast slot
                    ModelFamily::Unknown,               // primary slot (lazy-loaded)
                    config.embed_family.clone(),        // embed slot — drives pooling/instructions
                    config.code_family.clone(),         // code slot (lazy, hot-swaps with primary)
                )
                .map_err(|e| format!("Failed to load model: {e}"))?,
            );

            if config.primary_model_path.is_some() {
                loaded.start_idle_monitor(60);
            }

            let raw: Arc<dyn InferenceProvider> = loaded;
            *state.inference.write().await = Some(Arc::clone(&raw));
            raw
        }
    };

    // Wrap raw with mesh routing only in Local mode. The wrapper asks
    // the embedded daemon on every Slow-slot request whether any peer
    // with more RAM is online; if yes AND privacy isn't LocalOnly, it
    // forwards over HTTP to `<peer>:9741/v1/chat/completions`. On any
    // remote error, auto-falls back to `raw_inference`.
    //
    // In Attach mode (`state.mesh == None`) the CLI daemon already
    // owns peer-routing decisions — wrapping `raw_inference` with a
    // MeshInferenceProvider against a None daemon would be a no-op at
    // best and misleading at worst, so we just hand the raw provider
    // through.
    let inference: Arc<dyn InferenceProvider> = match state.mesh.as_ref() {
        Some(mesh) => Arc::new(
            sovereign_mesh::peer_inference::MeshInferenceProvider::new(
                Arc::clone(&raw_inference),
                Arc::clone(mesh),
            ),
        ),
        None => Arc::clone(&raw_inference),
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

            // Two handles for KnowledgeView wire-up: concrete Arc for
            // `set_observer` (called once the manager exists below),
            // trait-object Arc for the runtime + tools.
            let store_concrete: Arc<SqliteStateStore> = Arc::new(sqlite_store);
            let s: Arc<dyn StateStore> = store_concrete.clone();
            *state.store.write().await = Some(Arc::clone(&s));
            // Stash the concrete handle in the AppState so the later
            // KnowledgeView wiring can call `set_observer` on it.
            *state.sqlite_store.write().await = Some(store_concrete);
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
    // Derive the embedding model identifier from the configured file path
    // so `_corpus_meta.json` records the actual model rather than the
    // hardcoded `"qwen3-embedding-0.6b"` default. We use the filename
    // stem (without .gguf) as a stable, human-readable identifier.
    let embed_model_name = config
        .embed_model_path
        .as_ref()
        .and_then(|p| p.file_stem())
        .and_then(|s| s.to_str())
        .unwrap_or("unknown-embed-model")
        .to_string();
    // Resolve the persistent node_id so partition_path() returns a
    // directory name the Desktop-side daemon and the CLI daemon both
    // agree on (`<corpus>-partition-node-<hex>`). Without this the
    // engine defaults to `self_node_id = "local"` and
    // `in_progress_ingestions` silently misses partition-of-self
    // directories, leaving the UI stuck on "Install" for corpora
    // that are actively being ingested on disk.
    //
    // Resolution order matches `EmbeddedDaemon::start_daemon`: the
    // `node_id` sidecar file first (rare — only appears after
    // `load_or_generate` has written one), then mesh.json's
    // `self_node_id` (the common path when a mesh already exists),
    // falling back to generate-and-persist for fresh installs.
    let mesh_data_dir_resolved = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("sovereign");
    let self_node_id = match sovereign_mesh::persist::load_node_id(&mesh_data_dir_resolved) {
        Ok(Some(id)) => id,
        _ => match sovereign_mesh::persist::load(&mesh_data_dir_resolved) {
            Ok(Some(persisted)) => persisted.self_node_id,
            _ => sovereign_mesh::persist::load_or_generate_self_node_id(&mesh_data_dir_resolved),
        },
    };

    let corpus_engine = Arc::new(
        corpus_engine::CorpusEngine::new(recipes_dir, indexes_dir, embed_fn)
            .with_embedding_model(&embed_model_name)
            .with_batch_embed_fn(batch_embed_fn)
            .with_inference_fn(inference_fn.clone())
            .with_self_node_id(self_node_id.to_string()),
    );
    *state.corpus_engine.write().await = Some(Arc::clone(&corpus_engine));

    // Bring up the LocalCorpusManager alongside the engine. Loads any
    // previously-registered corpora from their sidecar JSON so the
    // "Local Knowledge" settings list populates immediately on launch.
    {
        let store_for_lcm = state
            .store
            .read()
            .await
            .as_ref()
            .cloned()
            .ok_or_else(|| "state store not ready".to_string())?;
        let snapshot_root = config.data_dir.join("vault-snapshots");
        match LocalCorpusManager::init(
            Arc::clone(&corpus_engine),
            store_for_lcm,
            Some(Arc::clone(&raw_inference)),
            config.data_dir.clone(),
            snapshot_root,
        )
        .await
        {
            Ok(mgr) => {
                *state.local_corpus.write().await = Some(Arc::new(mgr));
                tracing::info!("local_corpus manager initialised");
            }
            Err(e) => {
                tracing::warn!(
                    "local_corpus manager init failed: {e} — \
                     Local Knowledge section will show a setup error"
                );
            }
        }
    }

    // KnowledgeView wire-up (desktop mirror of the server/CLI path).
    // Gated on three things, in order of precedence:
    //
    // 1. **Attach mode** — when a CLI daemon at `:9741` is the source
    //    of truth, IT owns the `KnowledgeViewManager` (see
    //    `sovereign-cli/src/daemon_cmd.rs:697`). Constructing one here
    //    too means: a duplicate observer fires on every conversation
    //    write, two debouncers race to ingest the same view, and two
    //    enrichment loops compete for the chat slot. Skip entirely.
    //    Note: this means landscape digests are NOT spliced into
    //    prompts on the desktop side in attach mode — the daemon has
    //    the digest data but no HTTP endpoint exposes it yet. TODO:
    //    add `/v1/knowledge/landscape_digest` on the daemon and a
    //    thin client-side `LandscapeDigestProvider` impl that fetches
    //    over HTTP, then wire that into the runtime here.
    //
    // 2. **Settings → Knowledge → Enable KnowledgeView** — when the
    //    user has explicitly disabled the feature, Sovereign behaves
    //    exactly as it did before KnowledgeView existed.
    //
    // 3. Otherwise (Local / CliSetup mode, feature on) build the
    //    manager. The Runtime gets a landscape-digest provider; the
    //    observer wires the manager into SQLite writes.
    //
    // The toggle is read once at startup — changes to the Settings
    // toggle or to bootstrap mode require a desktop restart because
    // the Runtime is built once with or without the provider.
    let knowledge_view_manager = if state.is_attach_mode() {
        tracing::info!(
            "knowledge_view: attach mode — CLI daemon owns enrichment, \
             skipping desktop-side construction. Landscape digests in \
             chat splice are deferred until the daemon exposes an HTTP \
             endpoint."
        );
        None
    } else if config.knowledge_view_enabled {
        let knowledge_view_db_path = config.data_dir.join("sovereign.db");
        // Resolve local_only skill ids from the registry loaded above.
        // Mirror of the server/CLI paths.
        let local_only_skill_ids = skills.local_only_skill_ids();
        tracing::info!(
            local_only_skills = ?local_only_skill_ids,
            "knowledge_view: enabled; skills excluded from conversational corpus"
        );
        // Project-local ATOS paths — same `.sovereign/` layout as the
        // CLI / server bootstraps. Optional; the splice path's
        // strategic block falls through gracefully when either path
        // is missing.
        let project_sov_dir = std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .join(".sovereign");
        let features_db_path = project_sov_dir.join("features.db");
        let project_toml_path = project_sov_dir.join("project.toml");
        let mut mgr = sovereign_tools::knowledge_view::KnowledgeViewManager::new(
            Arc::clone(&corpus_engine),
            inference_fn.clone(),
            knowledge_view_db_path,
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
        if let Some(concrete) = state.sqlite_store.read().await.as_ref() {
            concrete.set_observer(
                mgr.clone() as sovereign_core::observer::SharedStateStoreObserver,
            );
        } else {
            tracing::warn!(
                "KnowledgeView: desktop store was not SQLite-backed; \
                 observer not installed (memory-mode fallback?)"
            );
        }
        Some(mgr)
    } else {
        tracing::info!(
            "knowledge_view: disabled via Settings — landscape digests \
             skipped, no ingest will run"
        );
        None
    };
    // No auto-backfill on launch.
    //
    // KnowledgeView ingest used to fire on a 30 s timer here, on the
    // theory that the user wouldn't notice if it kicked off "after
    // they opened the app." In practice, on a CPU-only fast slot a
    // single Phase 2 entity-extraction call against an 8 K-token
    // prompt takes ~4 minutes, and the phase fans out 4 of them. The
    // calls serialise through the slot mutex, so a user who opens
    // the app to chat lands behind a 16-minute queue.
    //
    // Phase 2 is now resumable (see corpus-engine
    // `_phase_1b_parsed.jsonl`), so partial work survives a kill —
    // but we still don't auto-trigger here. The debouncer (spawned
    // during `KnowledgeViewManager::new` in Local mode) picks up
    // `ConversationTouched` / `MemoryTouched` events, so new
    // conversations land in the index incrementally. Backfill of
    // existing-but-unfinished views is an explicit user action —
    // call `KnowledgeViewManager::enrich(view_id)` from a UI button
    // or a CLI command when the user wants a sweep.
    let _ = knowledge_view_manager.as_ref();

    // Hand the engine to the embedded Commonwealth daemon so that
    // when a user creates or joins a mesh, `/v1/knowledge/search` on
    // port 9741 can search our local corpora and peers gossip-probe
    // our `hosted_corpora` over `/internal/knowledge/search`. Must
    // happen BEFORE `try_resume` below — if resume fires first and
    // starts gossiping, our first few rounds would advertise empty
    // `hosted_corpora` and peers wouldn't know we host anything.
    // Only set the engine on an in-process EmbeddedDaemon (Local mode).
    // In Attach mode the CLI daemon owns this state and already has
    // its own CorpusEngine wired up.
    if let Some(mesh) = state.mesh.as_ref() {
        mesh.set_corpus_engine(Arc::clone(&corpus_engine)).await;
    }

    // Lazy-stamp canonical fingerprints for any installed canonicals
    // that don't yet carry one. Mirrors the daemon-mode bootstrap so
    // a Local/CliSetup desktop install gets the same legacy-corpus
    // upgrade pass. Spawned so it doesn't block startup.
    {
        let engine_for_stamp = Arc::clone(&corpus_engine);
        tokio::spawn(async move {
            engine_for_stamp.lazy_stamp_legacy_fingerprints().await;
        });
    }

    // Also hand over our RAW `InferenceProvider` (the
    // `EmbeddedLlamaCpp`, NOT the mesh-wrapped one) so when a peer
    // POSTs `/v1/chat/completions` to our `:9741`, we serve it from
    // our local model without re-entering the mesh-routing wrapper
    // and ping-ponging the request back out. This is what makes
    // "Joiner's synthesis runs on Founder's beefy model" physically
    // possible: Joiner's `MeshInferenceProvider` POSTs to the
    // Founder, the Founder's daemon invokes THIS adapter, which
    // runs inference locally and returns.
    if let Some(mesh) = state.mesh.as_ref() {
        mesh.set_inference_provider(Arc::clone(&raw_inference)).await;
    }

    // ── Wire the embedded daemon's full HTTP surface for CLI-setup mode ────────
    // In Local/CliSetup mode this process IS the sovereign daemon on :9741.
    // Earlier code only wired set_corpus_engine + set_inference_provider, which
    // leaves three surfaces dark when the HTTP listener starts at try_resume:
    //   • /v1/models  — needs set_setup_config so register_local_model_slots runs
    //   • /mcp        — needs set_mcp (ToolRegistry + NoteStore + session id)
    //   • /v1/projects — needs install_project_http_router + a live Reindexer
    //
    // Clone the Arc and config upfront so no borrows cross the .await points.
    let cli_setup_wiring = match (&state.bootstrap_mode, state.mesh.as_ref()) {
        (
            crate::bootstrap::BootstrapMode::Local {
                source: crate::bootstrap::ConfigSource::CliSetup(cfg),
            },
            Some(mesh),
        ) => Some((Arc::clone(mesh), cfg.clone())),
        _ => None,
    };
    if let Some((daemon_arc, cli_cfg)) = cli_setup_wiring {
        let data_dir = cli_cfg.data.dir.clone();
        let indexes_dir = data_dir.join("indexes");
        let _ = std::fs::create_dir_all(&indexes_dir);

        // 1. /v1/models — set_setup_config causes register_local_model_slots
        //    to fire inside start_daemon (called by try_resume below).
        daemon_arc.set_setup_config(cli_cfg.clone()).await;

        // 2. /mcp — ToolRegistry backed by the already-loaded CorpusEngine.
        let notes_path = data_dir.join("notes.db");
        // Open the NoteStore once at the top so both the MCP arm
        // (consumes it via set_mcp) and the reindexer commit
        // harvester (Phase 7.1, configured below) can share the
        // same connection pool. None on failure means MCP doesn't
        // mount AND no commit harvesting — same graceful-degrade
        // posture as before.
        let notes_for_harvester: Option<Arc<corpus_engine::NoteStore>> =
            match corpus_engine::NoteStore::open(&notes_path) {
                Ok(s) => Some(Arc::new(s)),
                Err(_) => None,
            };
        match corpus_engine::NoteStore::open(&notes_path) {
            Ok(notes_store) => {
                let notes = Arc::new(notes_store);
                let mut mcp_tools = ToolRegistry::new();
                // Code-intel tools — reuse the already-loaded CorpusEngine.
                mcp_tools.register(Box::new(sovereign_tools::SymbolLookupTool::new(
                    Arc::clone(&corpus_engine),
                )));
                mcp_tools.register(Box::new(sovereign_tools::CodeSearchTool::new(
                    Arc::clone(&corpus_engine),
                )));
                mcp_tools.register(Box::new(sovereign_tools::RecentChangesTool::new(
                    Arc::clone(&corpus_engine),
                )));
                // Call-graph tools with initial merged SCIP state.
                let initial_graph = corpus_engine::ScipGraph::open_in_memory("merged")
                    .expect("in-memory ScipGraph for MCP call-graph tools");
                if let Ok(rd) = std::fs::read_dir(&indexes_dir) {
                    for de in rd.flatten() {
                        if !de.path().is_dir() {
                            continue;
                        }
                        let scip_path = de.path().join("scip_graph.db");
                        if scip_path.exists() {
                            let _ = initial_graph.import_from_path(&scip_path).await;
                        }
                    }
                }
                let graph_handle: sovereign_mesh::reindexer::ScipGraphHandle =
                    Arc::new(arc_swap::ArcSwap::from_pointee(initial_graph));
                let hc = Arc::new(sovereign_tools::IndexHealthChecker::new(
                    Arc::clone(&graph_handle),
                ));
                mcp_tools.register(Box::new(
                    sovereign_tools::FindCallersTool::new(
                        Arc::clone(&corpus_engine),
                        Arc::clone(&graph_handle),
                    )
                    .with_health_checker(Arc::clone(&hc)),
                ));
                mcp_tools.register(Box::new(
                    sovereign_tools::FindCalleesTool::new(
                        Arc::clone(&corpus_engine),
                        Arc::clone(&graph_handle),
                    )
                    .with_health_checker(Arc::clone(&hc)),
                ));
                mcp_tools.register(Box::new(
                    sovereign_tools::BlastRadiusTool::new(Arc::clone(&graph_handle))
                        .with_health_checker(Arc::clone(&hc)),
                ));
                // Notes tools.
                mcp_tools.register(Box::new(sovereign_tools::WriteNoteTool::new(
                    Arc::clone(&notes),
                )));
                mcp_tools.register(Box::new(sovereign_tools::ReadNotesTool::new(
                    Arc::clone(&notes),
                )));
                mcp_tools.register(Box::new(sovereign_tools::DeleteNoteTool::new(
                    Arc::clone(&notes),
                )));
                mcp_tools.register(Box::new(sovereign_tools::SessionReflectionTool::new(
                    Arc::clone(&notes),
                )));
                mcp_tools.register(Box::new(sovereign_tools::CheckDocPathsTool::new()));
                let session_id = format!("desktop-{}", uuid::Uuid::new_v4());
                tracing::info!(tools = mcp_tools.count(), "desktop daemon: wiring /mcp");
                daemon_arc
                    .set_mcp(Arc::new(mcp_tools), Arc::clone(&notes), session_id)
                    .await;
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "desktop daemon: notes.db unavailable — /mcp will not be mounted"
                );
            }
        }

        // 3. Mesh HTTP + admin HTTP API (enables /v1/mesh/* and /v1/admin/reload).
        daemon_arc
            .install_mesh_http_router(sovereign_mesh::mesh_http::mesh_router(
                Arc::clone(&daemon_arc),
            ))
            .await;
        daemon_arc
            .install_admin_http_router(sovereign_mesh::admin_http::admin_router(
                Arc::clone(&daemon_arc),
            ))
            .await;

        // 4. /v1/projects — project freshness pipeline.
        let merged_for_indexer = corpus_engine::ScipGraph::open_in_memory("merged")
            .expect("in-memory ScipGraph for project pipeline");
        let merged_handle: sovereign_mesh::reindexer::ScipGraphHandle =
            Arc::new(arc_swap::ArcSwap::from_pointee(merged_for_indexer));
        let mut reindexer = sovereign_mesh::reindexer::Reindexer::new(
            indexes_dir.clone(),
            merged_handle,
        );
        // Phase 7.1: hook the commit-message harvester so the
        // desktop daemon's git-HEAD poll persists committed-source
        // notes alongside the SCIP rebuild. The harvester opens
        // its own NoteStore handle (`notes_for_harvester` above) —
        // the MCP arm's `notes` is moved into set_mcp and out of
        // scope here. Same DB file, separate Arc handles.
        if let Some(notes) = notes_for_harvester.as_ref() {
            sovereign_mesh::reindexer::Reindexer::with_commit_harvester(
                &mut reindexer,
                Arc::clone(notes),
            );
        }
        daemon_arc
            .install_project_http_router(sovereign_mesh::project_http::project_router(
                Arc::clone(&reindexer),
            ))
            .await;
        // Resume any previously-registered projects so FS watchers restart.
        let registry = sovereign_mesh::projects::Registry::load().unwrap_or_else(|e| {
            tracing::warn!(
                error = %e,
                "desktop daemon: project registry unavailable; starting empty"
            );
            sovereign_mesh::projects::Registry::default()
        });
        for entry in registry.entries() {
            reindexer.register(entry.clone()).await;
            tracing::info!(corpus = %entry.corpus_id, "desktop daemon: resumed project");
        }
        // The project router's Extension holds an Arc<Reindexer> keeping
        // watchers alive for the process lifetime — the local clone can drop.
        drop(reindexer);

        tracing::info!(
            "desktop daemon: /v1/models, /mcp, and /v1/projects are now wired"
        );
    }

    // Startup dimension guard: probe the loaded embed model's actual output
    // size and compare against every installed corpus index. A mismatch means
    // the user swapped embed models after building their library — retrieval
    // will silently return wrong results unless they rebuild.
    //
    // The probe also gives us the real dimension count for `EmbedModelInfo`,
    // which the collaborative ingestion planner uses to validate that peers
    // are embedding with the same model before assigning them a partition.
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

                // Derive pooling and normalization from the embed family quirks
                // (set at model-load time). Unknown/mean-pool models have no
                // quirks entry and correctly default to Mean + Application.
                let embed_quirks = config.embed_family.default_quirks().embed;
                let pooling = embed_quirks
                    .as_ref()
                    .map(|q| q.pooling)
                    .unwrap_or(PoolingStrategy::Mean);
                let normalization = embed_quirks
                    .as_ref()
                    .map(|q| q.normalize)
                    .unwrap_or(NormalizationStrategy::Application);
                let embed_info = EmbedModelInfo {
                    model_id: embed_model_name.clone(),
                    dimensions: dims,
                    pooling,
                    normalization,
                };
                tracing::info!(
                    model_id = %embed_info.model_id,
                    dims,
                    pooling = ?embed_info.pooling,
                    "embed model info: advertising to mesh peers"
                );
                if let Some(mesh) = state.mesh.as_ref() {
                    mesh.set_embed_model_info(embed_info).await;
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

    // Mesh knowledge client — talks to our OWN local Commonwealth
    // daemon at 127.0.0.1:9741. When no mesh is active, reqwest
    // immediately gets ECONNREFUSED and the client returns an empty
    // vec — so installing this unconditionally is safe: the Runtime
    // path stays identical to the no-mesh world in that case, and
    // "flips on" automatically the moment the user creates/joins.
    //
    // Client construction is infallible in practice (builder only
    // errors on TLS config, which doesn't apply to localhost HTTP);
    // if something truly goes wrong, skip mesh injection and log —
    // the local-only retrieval path is still functional.
    let mesh_knowledge: Option<
        Arc<dyn sovereign_core::traits::MeshKnowledgeSource>,
    > = match sovereign_mesh::knowledge_client::MeshKnowledgeClient::new(
        "http://127.0.0.1:9741",
    ) {
        Ok(c) => {
            tracing::info!(
                "mesh knowledge client: wired to http://127.0.0.1:9741"
            );
            Some(Arc::new(c))
        }
        Err(e) => {
            tracing::warn!(error = %e, "mesh knowledge client build failed; local-only retrieval");
            None
        }
    };

    // Snapshot the local-only skill ids BEFORE the registry is
    // consumed by `Runtime::new`. The attach-mode landscape-digest
    // client below needs them to resolve `active_is_local_only`
    // before sending each request to the daemon.
    let local_only_skill_ids_for_digests = skills.local_only_skill_ids();

    let mut runtime = Runtime::new(
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
    // Landscape-digest provider wiring. Three branches:
    //
    // 1. **Local mode + KnowledgeView enabled** — install the local
    //    `KnowledgeViewManager` (already constructed above).
    // 2. **Attach mode** — fetch digests from the daemon's
    //    `POST /v1/knowledge/landscape_digest` endpoint via
    //    `MeshLandscapeDigestClient`. The desktop sends the
    //    caller-resolved `active_is_local_only` so the daemon
    //    doesn't have to introspect the skill registry.
    // 3. **KnowledgeView disabled** — `Runtime.landscape_digests`
    //    stays `None`, the splice path is a no-op (identical to
    //    pre-KnowledgeView behaviour).
    if let Some(ref mgr) = knowledge_view_manager {
        runtime = runtime.with_landscape_digests(
            Arc::clone(mgr)
                as Arc<dyn sovereign_core::traits::LandscapeDigestProvider>,
        );
    } else if state.is_attach_mode() && config.knowledge_view_enabled {
        match sovereign_mesh::landscape_digest_client::MeshLandscapeDigestClient::new(
            "http://127.0.0.1:9741",
            local_only_skill_ids_for_digests,
        ) {
            Ok(client) => {
                tracing::info!(
                    "knowledge_view: attach mode — landscape digest client wired \
                     to http://127.0.0.1:9741/v1/knowledge/landscape_digest"
                );
                runtime = runtime.with_landscape_digests(
                    Arc::new(client)
                        as Arc<dyn sovereign_core::traits::LandscapeDigestProvider>,
                );
            }
            Err(e) => tracing::warn!(
                error = %e,
                "knowledge_view: attach-mode landscape digest client build \
                 failed; chat splice will run without digests this session"
            ),
        }
    }
    if let Some(m) = mesh_knowledge {
        runtime = runtime.with_mesh_knowledge(m);
    }
    // PR2 — install the Tauri routing-events sink so the runtime can
    // fire interpretation-proposed / clarification-request /
    // turn-narration back to the desktop UI.
    runtime = runtime.with_routing_events(
        Arc::clone(&state.routing_events)
            as Arc<dyn sovereign_core::traits::RoutingEventSink>,
    );

    *state.runtime.write().await = Some(Arc::new(runtime));

    // Auto-resume a previously-persisted mesh so the founder sees
    // their mesh on restart and existing joiners pick up where they
    // left off. Fails soft — a missing or corrupt mesh.json never
    // blocks startup.
    //
    // Attach mode skips this: the CLI daemon has already resumed its
    // own mesh state before we probed `:9741`. Running `try_resume`
    // from this process would either be a no-op (if our `mesh` is
    // None) or fight the CLI daemon for the same mesh.json.
    if let Some(mesh) = state.mesh.as_ref() {
        match mesh.try_resume().await {
            Ok(true) => tracing::info!("mesh: resumed from persisted state"),
            Ok(false) => tracing::debug!("mesh: no persisted state, starting fresh"),
            Err(e) => tracing::warn!(error = %e, "mesh: try_resume failed"),
        }
    } else {
        tracing::info!("mesh: attach mode — CLI daemon owns mesh state");
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
