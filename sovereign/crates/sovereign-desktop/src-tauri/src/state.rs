// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use corpus_engine::CorpusEngine;
use corpus_engine_notes::NoteStore;
use sovereign_store::recipe_project_store::RecipeProjectStore;

use sovereign_core::health_monitor::HealthMonitor;
use sovereign_core::insight::InsightService;
use sovereign_core::model_family::{EmbedModelInfo, NormalizationStrategy, PoolingStrategy};
use sovereign_core::planner::LlmPlanner;
use sovereign_core::runtime::Runtime;
use sovereign_core::traits::{InferenceProvider, StateStore};
use sovereign_core::types::InferenceConfig;
use sovereign_core::{SkillRegistry, ToolRegistry};
use sovereign_store::sqlite::SqliteStateStore;
use sovereign_tools::local_corpus::LocalCorpusManager;
use sovereign_tools::shell::ShellTool;
use tokio_util::sync::CancellationToken;

use crate::approval::TauriApprovalChannel;
use crate::supervisor_setup::SupervisedDaemon;

// Built-in skills live in a submodule (§3.3 state.rs decomposition).
mod builtin_skills;
#[cfg(debug_assertions)]
use builtin_skills::dev_workspace_skills_dir;
use builtin_skills::register_builtin_skills;

// Desktop config (DesktopConfig + defaults + load/save) lives in a
// submodule; re-exported so callers keep using `crate::state::DesktopConfig`.
mod config;
pub use config::*;

// Construction helpers for `bootstrap_with_progress` (§3.3).
mod builders;

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
    /// Two distinct `None`s, and neither is a half-wired daemon:
    ///
    /// - **Attach mode** — the CLI (`sovereign daemon run` under
    ///   launchd/systemd) already owns `:9741`. Mesh mutations route through
    ///   its HTTP API (`/v1/mesh/create/join/rotate/leave`). Stays `None`
    ///   forever; ask [`AppState::is_attach_mode`] to tell the two apart.
    /// - **Local mode, before `bootstrap`** — the daemon's services (engine,
    ///   provider, tool mount, routers) do not exist yet, so neither does the
    ///   daemon. It used to be constructed empty here and filled in by 17
    ///   setters, which is why a request arriving mid-bootstrap could reach a
    ///   daemon that answered 404 for half its surface.
    ///
    /// Read it through [`AppState::mesh`]; `bootstrap` commissions it once.
    pub mesh: RwLock<Option<Arc<sovereign_mesh::EmbeddedDaemon>>>,
    /// The single-writer claim on the data root the in-process daemon writes.
    ///
    /// `Some` only in in-process Local mode, and only once `bootstrap` has
    /// taken it — holding it here is what keeps it alive for the process's
    /// lifetime, since dropping a [`RunLock`] releases it. `None` in Attach
    /// mode (the daemon at the other end holds its own) and in supervised
    /// Local (the child holds it), which together are every shipped default.
    ///
    /// It gates `mesh`: bootstrap commissions no daemon when the claim is
    /// refused, because a second writer on one root is store corruption, a
    /// pidfile naming the wrong process, and double model RAM.
    pub run_lock: RwLock<Option<sovereign_contracts::run_lock::RunLock>>,
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
    /// Watched-folder reconciliation subsystem. The handle inside
    /// holds the scheduler's JoinHandle alive — dropping `AppState`
    /// stops the dispatcher loop. `None` in Attach mode (the
    /// standalone daemon owns the scheduler) and before the embedded
    /// daemon's wire-up completes in Local mode.
    pub watched_subsystem: RwLock<Option<sovereign_mesh::watched_folder_setup::WatchedSubsystem>>,
    /// Recipe-author project layer needs both notes (decision log,
    /// research findings, capability requests, checkpoints) and
    /// features (RecipeAuthoring-state FeatureRow per project).
    /// Both opened during bootstrap from `<data_dir>/notes.db` and
    /// `<data_dir>/features.db`; `None` until bootstrap completes
    /// or when those DBs failed to open.
    pub notes: RwLock<Option<Arc<NoteStore>>>,
    pub features: RwLock<Option<Arc<RecipeProjectStore>>>,
    /// Child-process daemon supervisor. Populated when
    /// `supervisor_setup::maybe_start` spawned a daemon child — the
    /// DEFAULT Local-mode boot since the W1 flip (DAEMON_RESILIENCE.md
    /// P0.1; opt-outs: `SOVEREIGN_USE_SUPERVISOR=0`,
    /// `SOVEREIGN_FORCE_LOCAL=1`). `None` for the in-process fallback
    /// and for Attach mode (an externally-owned daemon). The
    /// `supervisor_reconnect` / `supervisor_active` commands
    /// (`commands/supervisor_ctl.rs`) surface it to the frontend.
    ///
    /// Holds the supervisor AND its run-loop handle together
    /// ([`SupervisedDaemon`]) because stopping the child needs both: the
    /// app's `RunEvent::Exit` takes the pair out of here and hands it to
    /// `supervisor_setup::shutdown`, which signals the loop and then
    /// waits for the child to be reaped. `_exit(0)` runs no destructors,
    /// so this deliberate stop is the ONLY thing that keeps quit from
    /// orphaning the daemon child.
    pub supervisor: RwLock<Option<SupervisedDaemon>>,
    /// Supervise-task handle for the opt-in **Mobile access**
    /// `sovereign-server` child (the phone-facing host). `Some` while the host
    /// runs; aborting the handle drops the run future and the in-flight child's
    /// `kill_on_drop(true)` SIGKILLs `sovereign-server` — that's the toggle-off
    /// path. `None` when Mobile access is off. See [`crate::mobile_host_setup`].
    pub mobile_host_supervisor: RwLock<Option<tauri::async_runtime::JoinHandle<()>>>,
    /// Manager for external MCP servers loaded at bootstrap (the
    /// `[[mcp_servers]]` array of the canonical config). Held only for its
    /// connection statuses, which the Settings → MCP pane reads via
    /// `mcp_list_servers`; the live HTTP transports are owned by the tools it
    /// registered into the Runtime's registry. `None` until bootstrap runs.
    pub mcp_servers: RwLock<Option<Arc<sovereign_tools::mcp::McpServerManager>>>,
    /// Local NER model (GLiNER) for document-ingest entity extraction.
    ///
    /// The same `Arc` handed to `Runtime::with_gliner` for retrieval, kept
    /// here so the document-ingest commands can drive it too: the T2
    /// skeleton entity pass swaps a 4B LLM call for this NER model
    /// (−70% of ingest prompt tokens; see `DocumentAssetManager::
    /// with_entity_extractor`). `None` when the model isn't installed —
    /// ingest then falls back to the LLM path, exactly as before.
    ///
    /// It's a `LazyGlinerExtractor`: empty until its background load
    /// finishes (~1s post-boot), which `build_skeleton` treats as
    /// fall-through-to-LLM per window — so a document attached in that
    /// first second degrades gracefully rather than losing entities.
    pub entity_extractor: RwLock<Option<Arc<dyn sovereign_core::traits::EntityExtractor>>>,
}

impl AppState {
    /// True when this process is talking to an external CLI daemon at
    /// `:9741` rather than running its own embedded daemon.
    ///
    /// **Never ask `mesh().is_none()` instead.** Since the daemon is
    /// commissioned at the END of bootstrap, `None` also means "Local mode,
    /// not yet built"; only this accessor answers "should there ever be one?".
    pub fn is_attach_mode(&self) -> bool {
        matches!(
            self.bootstrap_mode,
            crate::bootstrap::BootstrapMode::Attach { .. }
        )
    }

    /// Client port (`/v1/*`) of the daemon this desktop talks to. Attach: the
    /// port the standalone daemon bound. Local: the CliSetup config's port, else
    /// 9741 by convention.
    pub fn client_port(&self) -> u16 {
        use crate::bootstrap::{BootstrapMode, ConfigSource};
        match &self.bootstrap_mode {
            BootstrapMode::Attach { client_port, .. } => *client_port,
            BootstrapMode::Local {
                source: ConfigSource::CliSetup(c),
            } => c.daemon.client_port,
            BootstrapMode::Local { .. } => 9741,
        }
    }
    /// Internal port (`/internal/*`) of that daemon. Same resolution as client_port; 9742 by convention.
    pub fn internal_port(&self) -> u16 {
        use crate::bootstrap::{BootstrapMode, ConfigSource};
        match &self.bootstrap_mode {
            BootstrapMode::Attach { internal_port, .. } => *internal_port,
            BootstrapMode::Local {
                source: ConfigSource::CliSetup(c),
            } => c.daemon.internal_port,
            BootstrapMode::Local { .. } => 9742,
        }
    }
    pub fn client_base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.client_port())
    }
    pub fn internal_base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.internal_port())
    }

    /// Typed accessor for the chat `Runtime` (§2D-3 DesktopError). Returns
    /// `DesktopError::not_ready` while bootstrap is still loading it, so a
    /// handler can `state.runtime().await?` instead of the stringly
    /// `require_runtime!` macro. Additive — the `runtime` field and the
    /// macro both keep working during the incremental migration. Cloning
    /// the `Arc` also avoids holding the `RwLock` read guard across the
    /// handler's later `.await` points.
    pub async fn runtime(&self) -> Result<Arc<Runtime>, crate::error::DesktopError> {
        self.runtime
            .read()
            .await
            .as_ref()
            .map(Arc::clone)
            .ok_or_else(|| crate::error::DesktopError::not_ready("The assistant is still loading."))
    }

    /// Typed accessor for the desktop's OWN state store — the same
    /// `Arc<dyn StateStore>` `builders::store::open_store` returns and
    /// hands to `Runtime::new` further down this file, so both point at
    /// one `sovereign.db`.
    ///
    /// Non-chat database work (conversation list/rename/delete, memory
    /// tombstones, message search, answer export) reaches the store
    /// through here rather than through `Runtime.store`. That is
    /// daemon-convergence Phase 0: the desktop's dependency on
    /// `sovereign_core::Runtime` narrows to the ports that actually
    /// answer a turn, so the Runtime can later move into the daemon
    /// without dragging the desktop's DB access with it.
    ///
    /// The store opens EARLIER in bootstrap than the Runtime is
    /// installed, and survives a Runtime rebuild — so a caller that
    /// switched from `runtime()`/`require_runtime!` to this accessor
    /// stops reporting "still loading" during those two windows and
    /// answers from the database instead. That is the one intended
    /// behavioural delta of the repoint.
    pub async fn store(&self) -> Result<Arc<dyn StateStore>, crate::error::DesktopError> {
        self.store
            .read()
            .await
            .as_ref()
            .map(Arc::clone)
            .ok_or_else(|| crate::error::DesktopError::not_ready("The database is still loading."))
    }

    /// The in-process daemon, once `bootstrap` has commissioned it. `None` in
    /// Attach mode (permanently) and in Local mode until bootstrap completes.
    pub async fn mesh(&self) -> Option<Arc<sovereign_mesh::EmbeddedDaemon>> {
        self.mesh.read().await.clone()
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
        supervisor: Option<SupervisedDaemon>,
    ) -> Self {
        let config = DesktopConfig::load();
        // The daemon is NOT constructed here. In Local mode `bootstrap`
        // commissions it at the end, once the engine, provider, tool mount and
        // routers it needs actually exist — persisting its running-mesh state
        // into `<config.data_dir>/mesh.json` so a create/join survives an app
        // restart. In Attach mode it is never constructed at all;
        // `is_attach_mode()` answers "should there ever be one?" so nobody
        // has to read `mesh.is_none()` as an answer to it.

        // Mint a routing event sink from the same AppHandle the
        // approval channel uses. Constructing it here (rather than
        // plumbing a second AppHandle through the constructor) keeps
        // the call sites tight and reuses the handle clone.
        let routing_events = Arc::new(crate::routing_events::TauriRoutingEventSink::new(
            approval.app_handle(),
        ));

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
            mesh: RwLock::new(None),
            run_lock: RwLock::new(None),
            bootstrap_mode: mode,
            health_monitor: RwLock::new(None),
            health_shutdown: CancellationToken::new(),
            insight_service: RwLock::new(None),
            local_corpus: RwLock::new(None),
            watched_subsystem: RwLock::new(None),
            notes: RwLock::new(None),
            features: RwLock::new(None),
            supervisor: RwLock::new(supervisor),
            mobile_host_supervisor: RwLock::new(None),
            mcp_servers: RwLock::new(None),
            entity_extractor: RwLock::new(None),
        }
    }
}

/// Per-phase progress signal for callers that want to narrate
/// bootstrap as it advances. The desktop's `complete_setup_auto`
/// flow maps these into its `setup-progress` Tauri events; the CLI
/// daemon's first-boot path ignores them.
///
/// Variants are intentionally coarse — bootstrap is a long
/// monolithic chain and finer-grained signals would be noise. The
/// three points below are the user-perceptible "I've started doing
/// X" moments.
#[derive(Debug, Clone, Copy)]
pub enum BootstrapPhase {
    /// About to spawn the smoke-test subprocess (a 1-token decode
    /// in an isolated child to detect Metal/CUDA backend crashes
    /// before we load the model in-process).
    SmokeTesting,
    /// About to call `EmbeddedLlamaCpp::load_full_with_families`,
    /// which mmaps the GGUF and brings the model online.
    LoadingModel,
    /// About to open the SQLite store and run migrations.
    OpeningDatabase,
    /// About to assemble the router classifier stack (4 embed-based
    /// classifiers; ~ms when the exemplar embed cache is warm,
    /// seconds when cold or after an embed-model swap).
    AssemblingRouter,
    /// The router-embed cache missed (first launch, a cleared
    /// `~/.svrnmesh`, or a genuinely swapped embed model), so the four
    /// classifiers are re-embedding ~277 exemplars — minutes on a CPU-only
    /// embed slot. Surfaced so the splash is honest instead of looking hung.
    RebuildingRouterEmbeddings,
    /// About to wire tools, corpus engine, local-corpus manager and
    /// knowledge view (lance index opens scale with installed corpora).
    WiringKnowledge,
    /// About to construct the Runtime itself (cheap; last phase
    /// before `backend-ready`).
    BuildingRuntime,
}

/// Optional progress callback for `bootstrap_with_progress`. The
/// callback is invoked once per phase, in the order the phases
/// occur (smoke test → model load → DB open).
pub type BootstrapProgressCb = Box<dyn Fn(BootstrapPhase) + Send + Sync + 'static>;

/// Bootstrap the Runtime from the current config. Thin wrapper
/// over `bootstrap_with_progress` for callers that don't need
/// progress narration (legacy `complete_setup`, internal restarts).
pub async fn bootstrap(state: &AppState) -> Result<(), String> {
    bootstrap_with_progress(state, None).await
}

/// Bootstrap the Runtime, optionally narrating phase transitions
/// via `on_progress`. See `BootstrapPhase` for the emission points.
pub async fn bootstrap_with_progress(
    state: &AppState,
    on_progress: Option<BootstrapProgressCb>,
) -> Result<(), String> {
    let emit = |phase: BootstrapPhase| {
        if let Some(ref cb) = on_progress {
            cb(phase);
        }
    };

    // Glassbox sub-phase timing. The `WiringKnowledge` and `BuildingRuntime`
    // phases each bundle several loads; without per-step timing those two
    // splash phases are opaque (a 2026-06-29 trace found ~17s + ~19s hiding
    // inside them). `substep` logs each remaining critical-path step's
    // duration at info on the `bootstrap` target so a slow boot keeps
    // self-attributing even after the heavy loads moved to background warms.
    let substep = |name: &str, started: std::time::Instant| {
        tracing::info!(
            target: "bootstrap",
            substep = name,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "boot substep"
        );
    };

    let config = state.config.read().await.clone();

    // Model-slot paths live in `SetupConfig` (`~/.svrnmesh/config.toml`) —
    // the single source of truth, shared with the daemon. Resolve them once
    // here; the CPU-compat policy may mutate this in memory, and the
    // inference builder loads from it.
    let mut slots = ResolvedModelSlots::load()
        .map_err(|e| format!("No model configuration found ({e}). Complete setup first."))?;

    if slots.fast.as_os_str().is_empty() || !slots.fast.exists() {
        return Err(format!(
            "Model not found: {}. Place a GGUF model file at this path.",
            slots.fast.display()
        ));
    }

    // CPU/arch compatibility gate. Before loading anything, substitute a dense
    // model (or fail with a clear, in-app explanation) when the configured chat
    // model is a recurrent architecture that SIGSEGVs in ggml's CPU prefill —
    // so a model the machine can't run degrades gracefully instead of crashing
    // the app on the first message. No-op on GPU machines. The swap is
    // IN-MEMORY only (mutates `slots`, never rewrites `config.toml`). See
    // `builders::model_compat`.
    builders::model_compat::apply_cpu_compat_policy(&mut slots, &state.approval.app_handle())?;

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
    // In Local mode this process IS the daemon, and the daemon it will be is
    // commissioned at the bottom of this function. `MeshInferenceProvider`
    // needs a peer source NOW, and the daemon needs the provider — a genuine
    // cycle. `DeferredDaemon` is the one late binding left in the assembly and
    // it carries no capability: before `bind` it answers "no peers", which is
    // what a constructed-but-stopped daemon answered before.
    let deferred_daemon: Option<Arc<sovereign_mesh::DeferredDaemon>> = match &state.bootstrap_mode {
        crate::bootstrap::BootstrapMode::Attach { .. } => None,
        crate::bootstrap::BootstrapMode::Local { .. } => {
            Some(Arc::new(sovereign_mesh::DeferredDaemon::new()))
        }
    };
    let (raw_inference, inference) = builders::inference::load_inference(
        &state.inference,
        deferred_daemon.as_ref(),
        &slots,
        &config,
        &emit,
    )
    .await?;

    // Open database.
    let store: Arc<dyn StateStore> = builders::store::open_store(
        &state.store,
        &state.sqlite_store,
        &state.insight_service,
        &config,
        &inference,
        &emit,
    )
    .await?;

    // Open the recipe-author backing stores in BOTH bootstrap modes.
    // The CLI live-trial uses these too; desktop must mirror them so
    // the recipe-author workspace works whether or not the user has a
    // separately-running CLI daemon. Failures here are warned-and-
    // skipped — the rest of the desktop should not 503 because notes
    // or features couldn't open.
    if state.notes.read().await.is_none() {
        let notes_path = config.data_dir.join("notes.db");
        match NoteStore::open(&notes_path) {
            Ok(s) => {
                *state.notes.write().await = Some(Arc::new(s));
                tracing::info!(path = %notes_path.display(), "recipe-author: NoteStore opened");
            }
            Err(e) => tracing::warn!(
                path = %notes_path.display(),
                error = %e,
                "recipe-author: NoteStore open failed; recipe-author workspace will be disabled",
            ),
        }
    }
    if state.features.read().await.is_none() {
        let features_path = config.data_dir.join("features.db");
        match RecipeProjectStore::open(&features_path) {
            Ok(s) => {
                *state.features.write().await = Some(Arc::new(s));
                tracing::info!(path = %features_path.display(), "recipe-author: RecipeProjectStore opened");
            }
            Err(e) => tracing::warn!(
                path = %features_path.display(),
                error = %e,
                "recipe-author: RecipeProjectStore open failed; recipe-author workspace will be disabled",
            ),
        }
    }

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
    // Activate configured skills (or all background skills if none
    // specified). Both paths skip Workspace skills — those are
    // navigation-scoped surfaces, never globally activated from config
    // (see SkillRegistry::activate_configured / activate_all).
    if config.active_skills.is_empty() {
        skills.activate_all();
    } else {
        skills.activate_configured(&config.active_skills);
    }
    tracing::info!("Skills: {} loaded", skills.list().len());
    let skills = Arc::new(skills);

    // Router classifier stack — built through the shared `router_bootstrap`
    // helper so the desktop app wires the SAME embed router + scope + effort +
    // current-info classifiers as the CLI/bench and served daemon. Before this,
    // the desktop built a BARE LlmRouter with no classifiers, so desktop chat
    // never ran the deep-routing the benches validate ("desktop kind of sucks
    // even as the benches improve"). Parity is now by construction
    // (sovereign-core/router_bootstrap.rs). Exemplars are baked into the binary,
    // so this works inside the packaged `.app` with no on-disk files present.
    emit(BootstrapPhase::AssemblingRouter);
    // Be up front about a cold router-embed cache. If it MISSES (first launch, a
    // cleared ~/.svrnmesh, or a genuinely swapped embed model), the four
    // classifiers re-embed ~300 exemplars — minutes on a CPU-only embed slot.
    // `build_llm_router` opens the cache once and fires this hook exactly when
    // that re-embed is imminent, so we surface a distinct phase instead of a
    // frozen splash without paying a second open + sentinel probe here.
    let (llm_router, router_report) = sovereign_core::router_bootstrap::build_llm_router(
        Arc::clone(&inference),
        Arc::clone(&store),
        Arc::clone(&skills),
        &sovereign_core::router_bootstrap::ExemplarOverrides::from_env_and_repo(),
        || emit(BootstrapPhase::RebuildingRouterEmbeddings),
    )
    .await;
    tracing::info!(
        embed = router_report.embed_router.is_some(),
        scope = router_report.scope.is_some(),
        effort = router_report.effort.is_some(),
        current_info = router_report.current_info.is_some(),
        all_wired = router_report.all_wired(),
        "router classifier stack assembled"
    );
    // Boxed AFTER tool registration completes (below): the authority
    // probe (FINANCIAL_CORPORA §7.3) needs the finished registry.

    emit(BootstrapPhase::WiringKnowledge);

    // Planner.
    let planner = LlmPlanner::new(Arc::clone(&inference), Arc::clone(&skills));

    // Tools. Tier 4 of tool-framework expansion: wire a shared
    // ToolResultCache so idempotent tool calls (knowledge_lookup,
    // future code-intel reads) hit a per-conversation cache
    // instead of re-doing the corpus + memory + note fan-out on
    // every follow-up. Cache TTL defaults to 5 turns (configurable
    // via `with_max_age`); per-conversation scoping walls
    // inner-work / default-chat slices apart by `conversation_id`.
    let tool_cache = Arc::new(sovereign_core::tool_result_cache::ToolResultCache::new());
    let mut tools = ToolRegistry::new().with_cache(Arc::clone(&tool_cache));
    let enabled = &config.enabled_tools;

    if enabled.iter().any(|t| t == "shell") {
        tools.register(Box::new(ShellTool));
    }
    if enabled.iter().any(|t| t == "document") {
        tools.register(Box::new(sovereign_tools::document::DocumentTool::new(
            Arc::clone(&store),
            Arc::clone(&inference),
        ).declared()));
        let approval_for_doc = Arc::clone(&state.approval);
        tools.register(Box::new(
            sovereign_tools::DocumentOperationTool::new(Arc::clone(&store), Arc::clone(&inference))
                .with_progress(Arc::new(move |p| {
                    approval_for_doc.emit_event("document-progress", &p);
                })).declared(),
        ));
    }
    if enabled
        .iter()
        .any(|t| t == "search" || t == "knowledge" || t == "web_search")
    {
        // Phase 6 of PRODUCTION_SEARCH_INTEGRATION.md: build a
        // WebSearchRegistry from operator config + a SearchOrchestrator,
        // and hand it to SearchTool. The orchestrator path applies the
        // privacy + budget + fallback-chain invariants that the legacy
        // direct-enum dispatch never had. The legacy path stays
        // available via SearchTool::with_web for the seven other call
        // sites still using it.
        use sovereign_tools::web::search::{BraveBackendImpl, DuckDuckGoBackendImpl, SearchOrchestrator, TavilyBackendImpl, WebSearchBackend, WebSearchRegistry};

        let mut registry = WebSearchRegistry::new();
        // DuckDuckGo is always available (zero-config fallback).
        registry.register(Arc::new(DuckDuckGoBackendImpl::new()));
        // The operator-chosen provider, if any, gets registered
        // alongside. Both stay in the registry; the orchestrator
        // picks via the operator preference order (Tavily/Brave
        // first when configured, DuckDuckGo as the fallback).
        let preferred: Box<dyn WebSearchBackend> = match config.search_backend.provider.as_str() {
            "tavily" => {
                config
                    .search_backend
                    .api_key
                    .as_ref()
                    .map(|key| -> Box<dyn WebSearchBackend> {
                        Box::new(TavilyBackendImpl::new(key.clone()))
                    })
            }
            "brave" => {
                config
                    .search_backend
                    .api_key
                    .as_ref()
                    .map(|key| -> Box<dyn WebSearchBackend> {
                        Box::new(BraveBackendImpl::new(key.clone()))
                    })
            }
            _ => None,
        }
        .unwrap_or_else(|| Box::new(DuckDuckGoBackendImpl::new()));
        // Convert the Box to Arc so the registry's Arc-of-trait
        // shape is happy. DuckDuckGo's `register` above sets up the
        // fallback; this `register` may replace it with the same id
        // when the operator's provider is also DuckDuckGo (the
        // registry warn-logs the replacement, which is the right
        // signal — operator wanted DDG and they got it).
        registry.register(Arc::from(preferred));

        let orchestrator = Arc::new(SearchOrchestrator::new(Arc::new(registry)));
        tools.register(Box::new(
            sovereign_tools::search::SearchTool::with_orchestrator(
                Arc::clone(&store),
                Arc::clone(&inference),
                // Client built by the egress boundary (order
                // deep-research-t2a): tools-base is contract-only and
                // must not construct an egress-capable HTTP client
                // itself.
                sovereign_core::egress::search_client()
                    .expect("egress boundary search client build"),
                orchestrator,
            ),
        ));
    }
    if enabled.iter().any(|t| t == "web_fetch") {
        tools.register(Box::new(sovereign_tools::web::WebFetchTool::new()));
    }
    if enabled.iter().any(|t| t == "knowledge_lookup") {
        // Tool-Mastery Phase 5 — unified evidence front door
        // (corpus + memory + notes). The notes channel is wired
        // only when the recipe-author NoteStore opened cleanly
        // earlier in bootstrap; that's the same store the
        // dossier write hook reads/writes, so chat-side outcome
        // history and notes-channel evidence are coherent.
        let mut tool =
            sovereign_tools::KnowledgeLookupTool::new(Arc::clone(&store), Arc::clone(&inference));
        if let Some(ref ns) = *state.notes.read().await {
            tool = tool.with_notes(Arc::clone(ns));
        }
        // Tier 3 web escalation. Wired only when the operator
        // opted in via `DesktopConfig.auto_escalate_to_web`.
        // Builds a dedicated SearchOrchestrator mirroring the
        // `search` tool's construction (DDG always-on + the
        // operator-preferred backend). Costs one extra registry
        // instance vs. sharing — kept duplicate for scope-locality
        // (the search-tool block above is its own gated branch).
        if config.auto_escalate_to_web {
            use sovereign_tools::web::search::{BraveBackendImpl, DuckDuckGoBackendImpl, SearchOrchestrator, TavilyBackendImpl, WebSearchBackend, WebSearchRegistry};
            let mut registry = WebSearchRegistry::new();
            registry.register(Arc::new(DuckDuckGoBackendImpl::new()));
            let preferred: Box<dyn WebSearchBackend> =
                match config.search_backend.provider.as_str() {
                    "tavily" => config.search_backend.api_key.as_ref().map(
                        |key| -> Box<dyn WebSearchBackend> {
                            Box::new(TavilyBackendImpl::new(key.clone()))
                        },
                    ),
                    "brave" => config.search_backend.api_key.as_ref().map(
                        |key| -> Box<dyn WebSearchBackend> {
                            Box::new(BraveBackendImpl::new(key.clone()))
                        },
                    ),
                    _ => None,
                }
                .unwrap_or_else(|| Box::new(DuckDuckGoBackendImpl::new()));
            registry.register(Arc::from(preferred));
            let orchestrator = Arc::new(SearchOrchestrator::new(Arc::new(registry)));
            tool = tool
                .with_web_orchestrator(orchestrator)
                .with_auto_escalate(true);
            tracing::info!(
                "knowledge_lookup: auto_escalate_to_web ENABLED — \
                 thin local results will fall back to web search"
            );
        }
        tools.register(Box::new(tool.declared()));
    }

    // Construct a shared CorpusEngine. This single instance backs both
    // the install/list/remove Tauri commands AND the in-runtime epistemic
    // tools — there's no second corpus subsystem.
    //
    // Built-in recipes (Wikipedia, SEP, OpenAlex, …) live in Rust source
    // via `corpus_engine::recipe::builtin_recipes()`. Users can drop
    // additional `.toml` files into `~/.svrnmesh/recipes` for custom
    // corpora; nothing is bundled at build time.
    let sovereign_root = sovereign_contracts::rebrand::svrnmesh_root();
    let recipes_dir = sovereign_root.join("recipes");
    let indexes_dir = sovereign_root.join("indexes");
    let embed_fn = sovereign_tools::corpus::inference_to_embed_fn(Arc::clone(&inference));
    let batch_embed_fn =
        sovereign_tools::corpus::inference_to_batch_embed_fn(Arc::clone(&inference));
    let inference_fn = sovereign_tools::corpus::inference_to_inference_fn(Arc::clone(&inference));
    // Derive the embedding model identifier from the configured file path
    // so `_corpus_meta.json` records the actual model rather than the
    // hardcoded `"qwen3-embedding-0.6b"` default. We use the filename
    // stem (without .gguf) as a stable, human-readable identifier.
    let embed_model_name = slots
        .embed
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
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
    // `rebrand::data_dir()` is the SSOT for the per-user data root and resolves
    // the legacy fallback itself; a read site must not re-derive it.
    let mesh_data_dir_resolved = sovereign_core::rebrand::data_dir();
    let self_node_id = match sovereign_mesh::persist::load_node_id(&mesh_data_dir_resolved) {
        Ok(Some(id)) => id,
        _ => match sovereign_mesh::persist::load(&mesh_data_dir_resolved) {
            Ok(Some(persisted)) => persisted.self_node_id,
            _ => sovereign_mesh::persist::load_or_generate_self_node_id(&mesh_data_dir_resolved),
        },
    };

    // In-process tiered-enrichment stack — parity with the standalone
    // daemon (`sovereign-cli-daemon` bootstrap). The embedded daemon used
    // to wire NEITHER the engine-side tiered provider NOR the folder
    // driver's deps, so `enable_enrichment` fell back to the legacy
    // `sovereign-cli enrich` subprocess: exit 127 in a shipped bundle, and
    // a build wedged at "Preparing to build the map" even in a dev tree.
    // The shared builder constructs the same FolderTieredProvider + GLiNER
    // extractor both daemons use. `gliner_raw` (the NoteStore T2 handle) is
    // unused here — desktop notes wiring is separate.
    let (_gliner_raw, chunk_entity_extractor) =
        sovereign_gliner::load_gliner_extractor(&config.data_dir);
    let folder_tiered_provider =
        sovereign_tools::enrichment_bootstrap::build_folder_tiered_provider(
            &config.data_dir,
            Arc::clone(&raw_inference),
        );
    let mut engine_builder =
        corpus_engine::CorpusEngine::new(recipes_dir.clone(), indexes_dir, embed_fn)
            .with_embedding_model(&embed_model_name)
            .with_batch_embed_fn(batch_embed_fn)
            .with_inference_fn(inference_fn.clone())
            .with_self_node_id(self_node_id.to_string());
    if let Some(tiered_provider) = folder_tiered_provider {
        engine_builder = engine_builder.with_tiered_provider(tiered_provider);
    }
    if let Some(extractor) = chunk_entity_extractor.clone() {
        engine_builder = engine_builder.with_chunk_entity_extractor(extractor);
    }
    // A custom acquirer must be registered on EVERY engine that can
    // ingest a recipe naming it, or the install fails at acquire time
    // with "No custom acquirer registered for kind 'sec_edgar'". The
    // desktop's embedded daemon is one of those engines.
    sovereign_tools::sec_edgar::register(&engine_builder);
    let corpus_engine = Arc::new(engine_builder);
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
        // Thread the ENGINE's recipes dir into the manager so its
        // synthesized recipe TOMLs land where the engine's
        // `fetch_recipe` reads them. Plain `init` defaults to
        // `local-corpus-recipes`, which the engine (overrides_dir =
        // `~/.svrnmesh/recipes`) can't see — every watched-folder sweep
        // then errors "No registry entry for corpus '<id>'". Matches the
        // standalone daemon's `init_with_recipes_dir` wiring.
        match LocalCorpusManager::init_with_recipes_dir(
            Arc::clone(&corpus_engine),
            store_for_lcm,
            Some(Arc::clone(&raw_inference)),
            config.data_dir.clone(),
            snapshot_root,
            recipes_dir.clone(),
        )
        .await
        {
            Ok(mgr) => {
                // Folder-ingest v1 §3.3 — install enrichment
                // defaults so the UI's "Enable enrichment" path
                // can synthesize an `EnrichConfig`. The chat /
                // embed model ids come from the daemon's active
                // config; without them, `enable_enrichment` fails
                // fast with a "defaults not installed" error
                // before touching the subprocess. Loopback URL
                // mirrors what the rest of the desktop uses.
                // Derive model ids from the configured paths.
                // The daemon's slot manager uses path file_stem
                // as the canonical model id elsewhere; mirror
                // that here so the synthesized EnrichConfig
                // points at the same slot the daemon is serving.
                fn id_from_path(p: &std::path::Path) -> String {
                    p.file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_string()
                }
                let chat_model = slots
                    .primary
                    .as_deref()
                    .map(id_from_path)
                    .unwrap_or_else(|| id_from_path(&slots.fast));
                let embed_model = if slots.has_embed() {
                    id_from_path(&slots.embed)
                } else {
                    String::new()
                };
                if !chat_model.is_empty() && !embed_model.is_empty() {
                    // Loopback URL — resolved from the bootstrap mode
                    // so a non-default client port works. Local mode
                    // runs the embedded daemon on the configured port
                    // (9741 by convention).
                    let base_url = state.client_base_url();
                    mgr.set_enrichment_defaults(
                        sovereign_tools::local_corpus::watched::enrich::EnrichmentDefaults {
                            chat_model,
                            embed_model,
                            base_url,
                            cli_path: None,
                        },
                    )
                    .await;
                } else {
                    tracing::info!(
                        "local_corpus enrichment defaults not installed — \
                         chat_model or embedding_model not configured; \
                         per-folder enrichment will return an error \
                         until the user picks models in Settings"
                    );
                }
                // Install the tiered-enrichment deps so `enable_enrichment`
                // routes folder / Obsidian "Make explorable" builds through
                // the in-process tiered driver (`start_tiered_build`) instead
                // of the legacy `sovereign-cli enrich` subprocess. Same stack
                // the standalone daemon installs via `set_tiered_deps`; shares
                // the GLiNER extractor already loaded for the engine above.
                match sovereign_tools::enrichment_bootstrap::build_folder_tiered_deps(
                    &config.data_dir,
                    Arc::clone(&raw_inference),
                    chunk_entity_extractor.clone(),
                ) {
                    Some(deps) => {
                        mgr.set_tiered_deps(deps).await;
                        tracing::info!(
                            "local_corpus: tiered enrichment deps installed — \
                             folder/obsidian 'Make explorable' runs in-process"
                        );
                    }
                    None => tracing::warn!(
                        "local_corpus: tiered deps unavailable (state store) — \
                         folder enrichment would fall back to the legacy subprocess"
                    ),
                }
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
    // Gating precedence (attach mode → Settings toggle → build) and the
    // full rationale (the attach-mode duplicate-observer hazard, the
    // deferred-digest TODO) are documented on `build_knowledge_view`.
    // The manager is consumed below as the Runtime's landscape-digest
    // provider; here we only construct it — installing the SQLite write
    // observer is the builder's only side effect.
    let knowledge_view_manager = builders::knowledge_view::build_knowledge_view(
        state.is_attach_mode(),
        &config,
        &skills,
        &corpus_engine,
        &inference_fn,
        &inference,
        &state.sqlite_store,
    )
    .await;
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

    // ── Wire the embedded daemon's full HTTP surface for CLI-setup mode ────────
    // In Local mode this process IS the sovereign daemon on :9741, and the
    // pieces built in this block become the daemon's declared capability at
    // the commissioning site near the end of bootstrap.
    //
    // **This used to key on `ConfigSource::CliSetup`, and that was a bug.**
    // `ConfigSource` is a snapshot taken by `bootstrap::detect()` at app
    // start; the setup wizard writes `config.toml` AFTER that probe and then
    // calls `state::bootstrap` with the mode still reading `Fresh`. On the
    // in-process completion path (`maybe_restart_into_supervised` returns
    // false: harness, kill switch, or spawn failure) the desktop therefore
    // came up with an engine, a provider and NO `/v1/mesh/*`, NO `/mcp` and
    // NO registered model slots — a shape no one designed and nothing
    // reported. Bootstrap hard-requires `ResolvedModelSlots::load()`, which is
    // `SetupConfig::load()`, so a config is on disk on EVERY path that reaches
    // here. Read it now rather than trusting the probe-time snapshot.
    let local_daemon_wiring: Option<(Arc<sovereign_mesh::DeferredDaemon>, sovereign_core::setup_config::SetupConfig)> =
        match (&state.bootstrap_mode, deferred_daemon.as_ref()) {
            (crate::bootstrap::BootstrapMode::Local { source }, Some(handle)) => {
                let cfg = match source {
                    crate::bootstrap::ConfigSource::CliSetup(c) => c.clone(),
                    // Probe-time snapshot predates the wizard's write; the
                    // file exists by now or we would not have got this far.
                    _ => sovereign_core::setup_config::SetupConfig::load().map_err(|e| {
                        format!("Local mode reached bootstrap with no readable config.toml: {e}")
                    })?,
                };
                Some((Arc::clone(handle), cfg))
            }
            _ => None,
        };

    // ── Single-instance guard for the IN-PROCESS daemon ────────────
    //
    // Local mode means THIS process becomes the writer of `cfg.data.dir` —
    // the same root a standalone `svrn daemon run` claims. Supervised Local
    // never reaches here: `supervisor_setup::maybe_start` has already flipped
    // the mode to Attach against its own child, and the child takes the lock
    // itself. So this covers exactly the in-process fallback
    // (`SOVEREIGN_USE_SUPERVISOR=0`, `SOVEREIGN_FORCE_LOCAL=1`, or a
    // supervisor that failed to start) — the one shape where the desktop
    // process itself owns a data root.
    //
    // A refusal means a daemon owns that root but was not answering `:9741`
    // when `bootstrap::detect()` probed it: starting up, unloading an 18GB
    // model on the way out, or wedged. Attaching is not possible (nothing is
    // serving) and becoming a second writer corrupts the store, so we
    // commission NO daemon and say which lock stopped us. Every desktop
    // surface that does not need the mesh keeps working; `AppState::mesh`
    // stays `None`, which callers already handle.
    let local_daemon_wiring = match local_daemon_wiring {
        Some((handle, cfg)) => {
            // Same classification the standalone daemon applies, from the
            // same decider — a desktop and a daemon that each drew the "is
            // this the right root" line for themselves is how the split-brain
            // arose. Stranded means starting fresh on top of live data
            // elsewhere; a Split is residue and only worth saying.
            let roots = sovereign_contracts::data_roots::classify(&cfg.data.dir);
            let claim = if roots.is_refusal() {
                Err(format!("{roots}"))
            } else {
                if !roots.others().is_empty() {
                    tracing::warn!(
                        target: "bootstrap",
                        root = %cfg.data.dir.display(),
                        "data roots: {roots}"
                    );
                }
                sovereign_contracts::run_lock::RunLock::acquire(&cfg.data.dir)
                    .map_err(|e| format!("{e}"))
            };
            match claim {
                Ok(lock) => {
                    tracing::debug!(
                        target: "bootstrap",
                        lock = %lock.path().display(),
                        enforced = lock.is_enforced(),
                        "run lock: desktop claimed the data root for its in-process daemon"
                    );
                    *state.run_lock.write().await = Some(lock);
                    Some((handle, cfg))
                }
                Err(why) => {
                    tracing::error!(
                        target: "bootstrap",
                        root = %cfg.data.dir.display(),
                        "desktop: not commissioning an in-process daemon — {why}"
                    );
                    None
                }
            }
        }
        None => None,
    };

    // Snapshot the compaction config out of the CliSetup wiring so
    // the Runtime construction below can spawn a worker even though
    // `cli_cfg` only survives inside the `if let` arm. CliSetup is
    // the only mode where this state.rs codepath builds a Runtime
    // with a load-bearing memory store; attach-mode leaves the
    // worker `None` (the daemon at the other end runs its own).
    let compaction_config_for_runtime: sovereign_core::memory_compaction::CompactionConfig =
        local_daemon_wiring
            .as_ref()
            .map(|(_, cfg)| cfg.memory.compaction.clone())
            .unwrap_or_default();
    // What the daemon will be commissioned with, once this block has built it.
    let mut daemon_services: Option<(
        Arc<sovereign_mesh::DeferredDaemon>,
        sovereign_core::setup_config::SetupConfig,
        sovereign_mesh::ServingCapability,
    )> = None;
    if let Some((daemon_handle, cli_cfg)) = local_daemon_wiring {
        let data_dir = cli_cfg.data.dir.clone();
        let indexes_dir = data_dir.join("indexes");
        let _ = std::fs::create_dir_all(&indexes_dir);

        // /mcp — ToolRegistry backed by the already-loaded CorpusEngine.
        // Absence is NAMED, not an empty Option: "this host serves no tools"
        // and "notes.db would not open" are different facts (ARCH §18.3), and
        // the daemon renders the reason in its startup log.
        let mcp_surface: sovereign_mesh::McpSurface;
        let notes_path = data_dir.join("notes.db");
        // Open the NoteStore once at the top so both the MCP arm
        // (consumes it via set_mcp) and the reindexer commit
        // harvester (Phase 7.1, configured below) can share the
        // same connection pool. None on failure means MCP doesn't
        // mount AND no commit harvesting — same graceful-degrade
        // posture as before.
        let notes_for_harvester: Option<Arc<corpus_engine_notes::NoteStore>> =
            match corpus_engine_notes::NoteStore::open(&notes_path) {
                Ok(s) => Some(Arc::new(s)),
                Err(_) => None,
            };
        match corpus_engine_notes::NoteStore::open(&notes_path) {
            Ok(notes_store) => {
                let notes = Arc::new(notes_store);
                let mut mcp_tools = ToolRegistry::new();
                // Call-graph tools with initial merged SCIP state.
                // Built before the code-intel tools below so
                // `SymbolLookupTool` can share the same handle —
                // exact-name lookup now reads from SCIP rather than
                // the Lance chunk projection.
                let initial_graph = corpus_engine_scip::ScipGraph::open_in_memory("merged")
                    .map_err(|e| format!("in-memory ScipGraph for MCP call-graph tools: {e}"))?;
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
                // Code-intel tools — reuse the already-loaded CorpusEngine.
                let hc = Arc::new(sovereign_tools::IndexHealthChecker::new(Arc::clone(
                    &graph_handle,
                )));
                mcp_tools.register(Box::new(
                    sovereign_tools::SymbolLookupTool::new(
                        Arc::clone(&corpus_engine),
                        Arc::clone(&graph_handle),
                    )
                    .with_health_checker(Arc::clone(&hc)).declared(),
                ));
                mcp_tools.register(Box::new(sovereign_tools::CodeSearchTool::new(Arc::clone(
                    &corpus_engine,
                )).declared()));
                mcp_tools.register(Box::new(sovereign_tools::RecentChangesTool::new(
                    Arc::clone(&corpus_engine),
                ).declared()));
                mcp_tools.register(Box::new(
                    sovereign_tools::FindCallersTool::new(
                        Arc::clone(&corpus_engine),
                        Arc::clone(&graph_handle),
                    )
                    .with_health_checker(Arc::clone(&hc)).declared(),
                ));
                mcp_tools.register(Box::new(
                    sovereign_tools::FindCalleesTool::new(
                        Arc::clone(&corpus_engine),
                        Arc::clone(&graph_handle),
                    )
                    .with_health_checker(Arc::clone(&hc)).declared(),
                ));
                mcp_tools.register(Box::new(
                    sovereign_tools::BlastRadiusTool::new(Arc::clone(&graph_handle))
                        .with_health_checker(Arc::clone(&hc)).declared(),
                ));
                // Notes tools.
                mcp_tools.register(Box::new(sovereign_tools::WriteNoteTool::new(Arc::clone(
                    &notes,
                )).declared()));
                mcp_tools.register(Box::new(sovereign_tools::ReadNotesTool::new(Arc::clone(
                    &notes,
                )).declared()));
                mcp_tools.register(Box::new(sovereign_tools::DeleteNoteTool::new(Arc::clone(
                    &notes,
                )).declared()));
                mcp_tools.register(Box::new(sovereign_tools::SessionReflectionTool::new(
                    Arc::clone(&notes),
                ).declared()));
                let session_id = format!("desktop-{}", uuid::Uuid::new_v4());
                tracing::info!(tools = mcp_tools.count(), "desktop daemon: wiring /mcp");
                mcp_surface = sovereign_mesh::McpSurface::Mounted(sovereign_mesh::McpMount {
                    tools: Arc::new(mcp_tools),
                    notes: Arc::clone(&notes),
                    session_id,
                });
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "desktop daemon: notes.db unavailable — /mcp will not be mounted"
                );
                mcp_surface = sovereign_mesh::McpSurface::Unavailable {
                    reason: format!("notes.db unavailable: {e}"),
                };
            }
        }

        // `/v1/mesh/*`, `/v1/admin/reload` and the reading surface are no
        // longer installed here. They are pure functions of the daemon, so the
        // daemon builds them itself at start — which is what dissolves the
        // measured desktop-vs-CLI router delta rather than papering it over.

        // /v1/projects — project freshness pipeline.
        let merged_for_indexer = corpus_engine_scip::ScipGraph::open_in_memory("merged")
            .map_err(|e| format!("in-memory ScipGraph for project pipeline: {e}"))?;
        let merged_handle: sovereign_mesh::reindexer::ScipGraphHandle =
            Arc::new(arc_swap::ArcSwap::from_pointee(merged_for_indexer));
        let mut reindexer =
            sovereign_mesh::reindexer::Reindexer::new(indexes_dir.clone(), merged_handle);
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
        let project_http = sovereign_mesh::project_http::project_router(Arc::clone(&reindexer));
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

        // /internal/corpus/watch/* — watched-folder reconciliation. The ROUTE
        // is mounted unconditionally below; only the runtime singleton behind
        // it is conditional, and its handlers answer 503 with a named reason
        // when it is absent rather than 404ing as an unmounted route did.
        if let Some(lc_mgr) = state.local_corpus.read().await.as_ref().cloned() {
            let max_concurrent = cli_cfg.watched_folders.max_concurrent_sweeps;
            let subsystem = sovereign_mesh::watched_folder_setup::WatchedSubsystem::install(
                Arc::clone(&corpus_engine),
                lc_mgr,
                max_concurrent,
                // Living trigger: desktop deferred to the recipe×workflow merge (v1).
                None,
            )
            .await;
            // Stash the subsystem on the AppState so its
            // JoinHandle outlives this scope. AppState is held in an
            // Arc for the desktop's lifetime, so the loop runs as
            // long as the desktop process does.
            *state.watched_subsystem.write().await = Some(subsystem);
            tracing::info!("desktop daemon: /internal/corpus/watch/* router + scheduler wired");
        }

        daemon_services = Some((
            daemon_handle,
            cli_cfg,
            sovereign_mesh::ServingCapability {
                mcp: mcp_surface,
                project_http,
                corpus_watch_http: sovereign_mesh::corpus_watch_http::corpus_watch_router(),
            },
        ));
    }

    // Startup dimension guard: probe the loaded embed model's actual output
    // size and compare against every installed corpus index. A mismatch means
    // the user swapped embed models after building their library — retrieval
    // will silently return wrong results unless they rebuild.
    //
    // The probe also gives us the real dimension count for `EmbedModelInfo`,
    // which the collaborative ingestion planner uses to validate that peers
    // are embedding with the same model before assigning them a partition.
    // What this node advertises to peers about its embedding model. An explicit
    // named value in both directions: a peer reading silence would otherwise
    // fall back to a default model id and partition collaborative ingestion
    // here anyway (ARCH §18.3).
    let mut advertise_embed = sovereign_mesh::EmbedAdvertisement::Unavailable {
        reason: "no embed model configured".to_string(),
    };
    if slots.has_embed() {
        // Err => embed not configured or failed — skip validation.
        let t_embed_probe = std::time::Instant::now();
        advertise_embed = sovereign_mesh::EmbedAdvertisement::Unavailable {
            reason: "embed probe failed".to_string(),
        };
        if let Ok(probe_vec) = inference.embed("probe").await {
            substep("embed_probe", t_embed_probe);
            let dims = probe_vec.len();
            let t_validate = std::time::Instant::now();
            if let Err(e) = corpus_engine.validate_corpus_readiness(dims).await {
                tracing::warn!(
                    "Corpus readiness issue detected at startup: {} \
                         Retrieval over the affected corpus is skipped (and the \
                         user prompted to rebuild) until it is fixed.",
                    e
                );
            }
            substep("validate_corpus_readiness", t_validate);

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
                query_instruction_prefix: String::new(),
            };
            tracing::info!(
                model_id = %embed_info.model_id,
                dims,
                pooling = ?embed_info.pooling,
                "embed model info: advertising to mesh peers"
            );
            advertise_embed = sovereign_mesh::EmbedAdvertisement::Advertised(embed_info);
        }
    }

    // ── Health Monitor ────────────────────────────────────────────────────────
    builders::health::build_health_monitor(
        &state.health_monitor,
        &state.health_shutdown,
        &config,
        &store,
        &corpus_engine,
        &inference,
        &embed_model_name,
    )
    .await;

    tools.register(Box::new(sovereign_tools::ClaimSearchTool::new(Arc::clone(
        &corpus_engine,
    )).declared()));
    tools.register(Box::new(sovereign_tools::EpistemicLandscapeTool::new(
        Arc::clone(&corpus_engine),
    ).declared()));
    // Typed SEC-filing figures with basis + accession, or first-class refusals.
    //
    // UNCONDITIONAL, never gated on `config.enabled_tools`: this tool is what
    // DECLARES authority over an installed SEC corpus (`authority_domains()`
    // -> `claim_stores()`), and the guard arms only when a registered tool
    // claims a corpus the answer drew on. An install-capable surface without
    // it is a FABRICATION surface, not a reduced one.
    // Pinned by `tests/authority_surface_census.rs`, which carries the
    // measured evidence.
    tools.register(Box::new(sovereign_tools::sec_facts::SecFactsTool::new(
        Arc::clone(&corpus_engine),
    ).declared()));
    // Code Intelligence tools. Build the merged SCIP handle first so
    // SymbolLookupTool can share it — exact-name lookup now reads
    // SCIP directly (Lance kept only embeddings/content/mtime).
    // `indexes_dir` was moved into `CorpusEngine::new` above; we have
    // to re-derive the path from `sovereign_root` rather than reuse the binding.
    // Code-intel SCIP graph. The merge imports every corpus's
    // `scip_graph.db` into one in-memory graph; for a repo-scale code
    // corpus that's hundreds of MB and ~17s (2026-06-29 boot trace). It
    // is NOT needed to make chat usable, so register the tools against an
    // EMPTY graph NOW and merge in the BACKGROUND, swapping the populated
    // graph into the same `ArcSwap` handle the tools already hold. Until
    // the swap lands the code-intel tools return empty (IndexHealthChecker
    // reports "not ready") — graceful, and off the path to `backend-ready`.
    let indexes_dir_for_scip = sovereign_root.join("indexes");
    let symbols_graph: sovereign_mesh::reindexer::ScipGraphHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(
            corpus_engine_scip::ScipGraph::open_in_memory("merged")
                .map_err(|e| format!("in-memory ScipGraph for symbols lookup: {e}"))?,
        ));
    {
        let warm_dir = indexes_dir_for_scip.clone();
        let warm_handle = Arc::clone(&symbols_graph);
        tokio::spawn(async move {
            let started = std::time::Instant::now();
            let merged = match corpus_engine_scip::ScipGraph::open_in_memory("merged") {
                Ok(g) => g,
                Err(e) => {
                    tracing::warn!(target: "bootstrap", error = %e, "scip-merge(bg): open failed; code-intel stays empty");
                    return;
                }
            };
            let mut imported = 0usize;
            if let Ok(rd) = std::fs::read_dir(&warm_dir) {
                for de in rd.flatten() {
                    if !de.path().is_dir() {
                        continue;
                    }
                    let scip_path = de.path().join("scip_graph.db");
                    if scip_path.exists() && merged.import_from_path(&scip_path).await.is_ok() {
                        imported += 1;
                    }
                }
            }
            warm_handle.store(Arc::new(merged));
            tracing::info!(
                target: "bootstrap",
                imported,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "scip-merge(bg): code-intel graph ready"
            );
        });
    }
    tools.register(Box::new(sovereign_tools::SymbolLookupTool::new(
        Arc::clone(&corpus_engine),
        Arc::clone(&symbols_graph),
    ).declared()));
    tools.register(Box::new(
        sovereign_tools::CodeSearchTool::new(Arc::clone(&corpus_engine))
            .with_inference(Arc::clone(&inference)).declared(),
    ));
    tools.register(Box::new(sovereign_tools::RecentChangesTool::new(
        Arc::clone(&corpus_engine),
    ).declared()));

    // ── Recipe Author workspace tools ────────────────────────────
    //
    // The recipe-author chat dispatch (sovereign-core
    // runtime::handlers::recipe_author) runs an agent loop that
    // expects these tools registered on the per-process runtime's
    // ToolRegistry. Without them, every tool call falls back to
    // `unknown tool` and the agent burns iterations without
    // progress.
    //
    // The non-store-backed tools (browse / read / write / validate /
    // test / probe_url) register unconditionally; the store-backed
    // tools (decision_log / checkpoint / capability_request /
    // research_finding) skip when their handle didn't open earlier
    // in bootstrap, so the rest of the chat surface stays healthy
    // even when notes.db / features.db are unavailable.
    {
        use sovereign_contracts::recipe::notes::RecipeNotes;
        use sovereign_tools::recipe_author::{maintainer_inbox_dir, CapabilityRequestTool, CheckpointTool, DecisionLogTool, ProbeUrlTool, RecipeReadTool, RecipeTestTool, RecipeValidateTool, RecipeWriteStructuredTool, RecipeWriteTool, RegistryBrowseTool, ResearchFindingTool};
        use sovereign_tools::recipe_notes_adapter::NoteStoreRecipeNotes;
        use sovereign_tools::recipe_tester_adapter::CorpusEngineRecipeTester;
        tools.register(Box::new(RegistryBrowseTool));
        tools.register(Box::new(RecipeReadTool::new()));
        tools.register(Box::new(RecipeWriteTool::new()));
        tools.register(Box::new(RecipeWriteStructuredTool::new(Arc::new(
            CorpusEngineRecipeTester::new(),
        ))));
        tools.register(Box::new(RecipeValidateTool::new(Arc::new(
            CorpusEngineRecipeTester::new(),
        ))));
        tools.register(Box::new(RecipeTestTool::new(Arc::new(
            CorpusEngineRecipeTester::new(),
        ))));
        tools.register(Box::new(ProbeUrlTool::new()));

        // Wrap the concrete NoteStore in the RecipeNotes seam adapter so the
        // recipe-author tools depend only on the contract.
        let notes_handle: Option<Arc<dyn RecipeNotes>> =
            state.notes.read().await.as_ref().map(|ns| {
                Arc::new(NoteStoreRecipeNotes::new(Arc::clone(ns))) as Arc<dyn RecipeNotes>
            });
        let features_handle = state.features.read().await.as_ref().map(Arc::clone);

        if let Some(ns) = notes_handle.clone() {
            tools.register(Box::new(DecisionLogTool::with_notes(Arc::clone(&ns))));
            tools.register(Box::new(ResearchFindingTool::with_notes(Arc::clone(&ns))));
        } else {
            tracing::warn!(
                "recipe-author: NoteStore unavailable; decision_log / research_finding \
                 tools are not registered and recipe-author turns that call them will \
                 see `unknown tool`."
            );
        }

        if let (Some(ns), Some(fs)) = (notes_handle.as_ref(), features_handle.as_ref()) {
            tools.register(Box::new(CheckpointTool::with_stores(
                Arc::clone(ns),
                Arc::clone(fs),
            )));
            let mut cap_tool = CapabilityRequestTool::with_stores(Arc::clone(ns), Arc::clone(fs));
            // Wire the inbox directory so submitted capability requests
            // land where `sovereign maintainer inbox` reads them — same
            // path the live-trial harness uses.
            if let Ok(dir) = maintainer_inbox_dir() {
                cap_tool = cap_tool.with_inbox_dir(dir);
            }
            tools.register(Box::new(cap_tool));
        } else {
            tracing::warn!(
                "recipe-author: NoteStore / RecipeProjectStore not both available; checkpoint \
                 and capability_request tools are not registered."
            );
        }
    }

    // Workflow-author tools — the workflow analog of the recipe-author set above.
    // The same generic agent loop drives workflow authoring (skill =
    // `workflow-author`), so a Workflow-kind project's chat can compose a workflow
    // via `workflow_write_structured` / `workflow_validate` / `workflow_test`.
    // Registered unconditionally (no store handles needed); the recipe sub-flow
    // tools above stay available so a workflow can author its `recipe:` ingest
    // stage. Without these, a workflow-author turn sees `unknown tool` and the
    // agent can't write a workflow.
    for t in sovereign_workflow_host::author_tools() {
        tools.register(t);
    }

    tracing::info!("Tools: {} registered", tools.count());

    let approval: Arc<dyn sovereign_core::traits::ApprovalChannel> =
        Arc::clone(&state.approval) as Arc<dyn sovereign_core::traits::ApprovalChannel>;

    let inference_config = {
        let cfg = state.config.read().await;
        InferenceConfig {
            // Measurement lever: SOVEREIGN_SYNTH_TEMP forces the synthesis
            // temperature (default 0.7 → high output variance) so a replay A/B
            // isolates a code change instead of drowning it in sampling noise.
            // Unset in production; set to 0 for deterministic measurement runs.
            temperature: std::env::var("SOVEREIGN_SYNTH_TEMP")
                .ok()
                .and_then(|v| v.parse::<f32>().ok())
                .unwrap_or(cfg.temperature),
            max_tokens: cfg.max_tokens as usize,
            think_budget: cfg.think_budget as usize,
            top_k: cfg.top_k,
            auto_collaborate: cfg.auto_collaborate,
            custom_instructions: cfg
                .custom_instructions
                .as_ref()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
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
    let mesh_client_base = state.client_base_url();
    let mesh_knowledge: Option<Arc<dyn sovereign_core::traits::MeshKnowledgeSource>> =
        match sovereign_mesh::knowledge_client::MeshKnowledgeClient::new(&mesh_client_base) {
            Ok(c) => {
                tracing::info!(base_url = %mesh_client_base, "mesh knowledge client: wired");
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

    // External MCP servers — connect over HTTP and register their tools into
    // the SAME registry the agent plans against (parity with `sovereign chat`
    // and `sovereign serve`, all three via the one shared loader). Falls under
    // the WiringKnowledge phase; warn-and-continue per server so a dead URL is
    // logged, never blocking boot (bounded by an 8s/server connect timeout).
    // The manager is held on AppState for the Settings → MCP pane's status;
    // the live transports are owned by the tools moved into the Runtime below.
    let mcp_manager = sovereign_tools::mcp::load_from_setup_config(&mut tools).await;
    *state.mcp_servers.write().await = Some(Arc::new(mcp_manager));

    // Atlas-grounded retrieval (atom-enum / overview-claim injection): wire the
    // per-process atlas context manager so the runtime's atom-enum path can
    // reach the corpus atlas GRAPHS (Claim/Entity atoms). Parity with
    // `sovereign chat` (chat_cmd/bootstrap.rs) and `sovereign serve` — the
    // desktop previously left `atlas_context_provider` unset, so the entire
    // atom-enum path (including the SOVEREIGN_ATOM_ENUM_OVERVIEW claim injection
    // for "what's the most important thing in X" questions) silently no-op'd
    // here. `graph()` lazy-PARSES a corpus's atoms on first use, but only for
    // dirs the `init_from_cache()` scan (below) registered in `graph_dirs` — so
    // that call is REQUIRED for the claim path, not optional. Cache-only (cold
    // embed work deferred), so it's a bounded, predictable boot cost.
    // `inference` must be cloned BEFORE `Runtime::new` consumes it below.
    let atlas_ctx_mgr = Arc::new(
        sovereign_tools::atlas_context_manager::AtlasContextManager::new(
            corpus_engine.index_dir().to_path_buf(),
            Arc::clone(&inference),
            embed_model_name.clone(),
        ),
    );

    emit(BootstrapPhase::BuildingRuntime);
    // Authority probe (FINANCIAL_CORPORA §7.3): the router consults the
    // registry's deterministic claims before intent classification.
    let tools = Arc::new(tools);
    let router: Box<dyn sovereign_core::traits::Router> =
        Box::new(llm_router.with_authority_probe(Arc::clone(&tools)));
    // ── The turn's enrichment stack, gathered BEFORE the Runtime ─────────
    //
    // daemon-convergence Phase 4b: `LaneSources` is a required argument to
    // `Runtime::new`, so the five providers below are named in one value
    // instead of installed by five `with_*` calls a host can forget. The
    // rerank comment immediately below is the record of what forgetting cost:
    // this host shipped baseline fusion ordering for months while the ledger
    // reported the capability available.
    //
    // The constructor now sits at the END of this gathering (search
    // "Commission" below) rather than at the top being mutated on the way
    // down. Boot ORDER is unchanged — only the point of construction moved.
    let mut lane = sovereign_core::runtime::lane::LaneSources::none();
    lane.atlas_context = Some(
        Arc::clone(&atlas_ctx_mgr) as Arc<dyn sovereign_core::atlas_context::AtlasContextProvider>
    );
    // Cross-encoder reranker (T1 A2). Until 2026-08-03 the ONLY
    // surface that installed one was the `svrn chat` CLI — the desktop
    // had zero rerank references, so it shipped baseline fusion
    // ordering and `SOVEREIGN_PPR_EXPAND` logged "lane dark" here for
    // want of the same `rerank_fn`. Opt-in via
    // `SOVEREIGN_RERANK_MODEL_PATH`; soft-fails to baseline.
    if let Some(reranker) = sovereign_inference::reranker_standalone::load_from_env() {
        lane.rerank.f = Some(sovereign_tools::corpus::inference_to_rerank_fn(reranker));
        lane.rerank.config = sovereign_tools::corpus::rerank_config_from_env();
    }
    // Discover + register each corpus's atlas dir (and warm any cached
    // contexts) so the atom-enum path's `graph()` can lazy-load a corpus's
    // atoms on first use — `graph()` only parses dirs this scan registered.
    // Cache-only: cold embed work is deferred to the post-install hook, so this
    // is a bounded, predictable boot cost (parity with the CLI/server).
    let t_atlas_init = std::time::Instant::now();
    atlas_ctx_mgr.init_from_cache().await;
    substep("atlas_init_from_cache", t_atlas_init);
    // NOTE (2026-06-26): a background atlas-graph pre-warm was tried here to hide
    // the ~38s first-query cold parse of a wiki-scale (1.6M-atom) atlas, but it
    // REGRESSED the racing first query: `graph()` parses synchronously on the
    // query thread, so a query arriving during the background parse double-parses
    // the same graph under CPU contention (measured first-query gap 139s vs the
    // 38s cold baseline). Coordinating the sync `graph()` hot path with an async
    // pre-warm over the tokio-RwLock graph cache (a shared per-corpus parse lock)
    // is the correct fix but not a small change; the lazy `graph()` default is
    // kept until that lands. The cold cost is also corpus-size-specific (only
    // wiki-scale atlases are slow; typical corpora parse in <1s).
    // Wikipedia link graph (Atlas Layer 0) + cross-corpus meta-atlas — parity
    // with the CLI/server bootstrap (chat_cmd/bootstrap.rs, server main.rs).
    // Both probe LOCAL build artifacts and no-op when absent, so they're safe
    // in attach mode (not daemon-owned). Without them the desktop dropped the
    // wiki graph-expansion and the cross-corpus articulation boost the benches
    // exercise. (Probe logic mirrors bootstrap.rs `load_wikipedia_graph`;
    // dedup to a shared crate is a follow-up.)
    let t_wiki = std::time::Instant::now();
    if std::env::var("SOVEREIGN_DISABLE_WIKI_GRAPH")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        tracing::debug!("wikipedia_graph: disabled via SOVEREIGN_DISABLE_WIKI_GRAPH");
    } else if let Ok(infos) = corpus_engine.installed_indexes().await {
        let idx_dir = corpus_engine.index_dir().to_path_buf();
        for info in infos {
            // WIKIPEDIA_ATLAS_V2 W3: the shared columnar-or-sqlite gate (the
            // "dedup to a shared crate" the comment above anticipated). The
            // columnar (v2) open is two lazy Lance `open_table`s (cheap); the
            // legacy SQLite path opens a multi-GB db — if this substep ever
            // shows seconds, that corpus is on the SQLite fallback and should
            // be migrated (or this open deferred like meta-atlas).
            if let Some(g) = corpus_engine::open_wikipedia_graph(&idx_dir, &info.corpus_id).await {
                lane.wikipedia_graph = Some(g);
                break;
            }
        }
    }
    substep("wikipedia_graph_open", t_wiki);
    // Cross-corpus meta-atlas is DEFERRED off the boot critical path:
    // `canonical_atoms.json` is ~900MB and its parse+index was the bulk of
    // the BuildingRuntime phase (2026-06-29 trace). The Runtime starts with
    // `meta_atlas = None` (boost short-circuits, retrieval byte-identical)
    // and a background warm attaches it via `install_meta_atlas` once the
    // app is already interactive — spawned just after the Runtime is shared
    // below. The first turns simply run without the cross-corpus boost
    // until the warm lands (seconds).
    // Tool-Mastery Layer 3 — NoteStore drives the per-conversation
    // tool_decision write hook (runtime.rs handle_message_stream's
    // post-gap-check spawn) and the Layer-2 dossier read at the
    // top of the next turn. Without this wiring the desktop's
    // chat surface gets dossier=None on every turn — the framework
    // becomes structurally invisible to the user. Reading the
    // already-opened store rather than re-opening keeps a single
    // sqlite handle (WAL-friendly).
    // Captured now, applied at the commission below. Not a lane member — a
    // NoteStore is something the process OWNS (§3.5), not something a turn is
    // enriched with.
    let note_store_for_runtime = state.notes.read().await.as_ref().map(Arc::clone);
    // GLiNER entity extractor for retrieval-over-history. Probe the
    // default model id; if installed, load it and wire it onto the
    // Runtime. Failures soft-fall-through to pure cosine + MMR — the
    // desktop chat path keeps working without GLiNER. See
    // `Runtime::maybe_retrieve_relevant_history`.
    {
        let t_gliner = std::time::Instant::now();
        let model_id = sovereign_gliner::gliner_ner::DEFAULT_MODEL_ID;
        if sovereign_gliner::gliner_ner::probe_model_available(model_id) {
            // Deferred load: the ~950ms model load runs on a background
            // thread (it was ~half the warm boot). The extractor installs
            // immediately and soft-falls-through to cosine+MMR until warm —
            // the same behaviour as an uninstalled model, and it's warm
            // within ~1s, before the first query. See `LazyGlinerExtractor`.
            let arc: Arc<dyn sovereign_core::traits::EntityExtractor> =
                Arc::new(sovereign_gliner::gliner_ner::LazyGlinerExtractor::new_default_deferred());
            // Share the one handle: retrieval (via the Runtime) and
            // document ingest (via `state.entity_extractor`) drive the
            // same warmed model. Stash before the move into the runtime.
            *state.entity_extractor.write().await = Some(Arc::clone(&arc));
            lane.gliner = Some(arc);
            tracing::info!(
                model = model_id,
                "desktop: GLiNER entity extractor installed (background warm)"
            );
        } else {
            tracing::debug!(
                model = model_id,
                "desktop: GLiNER model not installed; entity-aware retrieval disabled (falls back to cosine+MMR)"
            );
        }
        substep("gliner_load", t_gliner);
    }
    // Rolling-summary compaction worker. Spawn one per Runtime so
    // the save-time hook in `end_conversation` can fire-and-forget
    // a compaction pass without blocking the writer's turn. The
    // worker holds its own Arc<MemoryStore> + Arc<InferenceProvider>
    // and serialises passes across conversations via a single mpsc
    // consumer. Pre-2026-05-23 behaviour is preserved when the
    // operator sets `[memory.compaction] mode = "disabled"`.
    let compaction_worker = {
        sovereign_core::memory_compaction::CompactionWorker::spawn(
            Arc::clone(&store) as Arc<dyn sovereign_core::traits::MemoryStore>,
            // Was `Arc::clone(&runtime.inference)` — the same handle, read
            // from the local rather than back out of a Runtime that does not
            // exist yet at this point.
            Arc::clone(&inference),
            compaction_config_for_runtime.clone(),
        )
    };
    // Conv-tiered briefing reader (spec CONV_TIERED_PORT.md). The
    // concrete SqliteStateStore stashed at bootstrap also impls
    // ConvTieredReader; pass the same Arc so retrieval prompts can
    // surface per-conversation RAPTOR signposts beside raw chunks.
    if let Some(ss) = state.sqlite_store.read().await.as_ref() {
        lane.conv_tiered =
            Some(Arc::clone(ss) as Arc<dyn sovereign_store::sqlite::ConvTieredReader>);
    }

    // ── Commission ───────────────────────────────────────────────────────
    // Every provider this host enriches with exists by now, so the Runtime is
    // built ONCE, total. Nothing below this line can add one.
    let mut runtime = Runtime::new(
        Arc::clone(&inference),
        router,
        Box::new(planner),
        Arc::clone(&tools),
        Arc::clone(&store),
        skills,
        approval,
        inference_config,
        lane,
    )
    .with_corpus_engine(Arc::clone(&corpus_engine))
    .with_compaction(compaction_worker);
    if let Some(ns) = note_store_for_runtime {
        runtime = runtime.with_note_store(ns);
    }
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
            Arc::clone(mgr) as Arc<dyn sovereign_core::traits::LandscapeDigestProvider>
        );
    } else if state.is_attach_mode() && config.knowledge_view_enabled {
        let digest_base = state.client_base_url();
        match sovereign_mesh::landscape_digest_client::MeshLandscapeDigestClient::new(
            &digest_base,
            local_only_skill_ids_for_digests,
        ) {
            Ok(client) => {
                tracing::info!(
                    base_url = %digest_base,
                    "knowledge_view: attach mode — landscape digest client wired \
                     to {digest_base}/v1/knowledge/landscape_digest"
                );
                runtime = runtime
                    .with_landscape_digests(Arc::new(client)
                        as Arc<dyn sovereign_core::traits::LandscapeDigestProvider>);
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
    // Folder-ingest v1 §3.4 + §6.3: wire the watched-folder manager
    // as both the sensitive-corpus oracle (which corpora to drop
    // from ambient retrieval) and the folder-metadata oracle (the
    // user-typed display names + skipped/failed counters that the
    // synthesis prompt and chat coverage chip depend on). When the
    // manager isn't ready (init failed earlier), the runtime falls
    // back to no-op behaviour for both — same shape as the
    // landscape-digest wiring above.
    if let Some(mgr) = state.local_corpus.read().await.as_ref() {
        runtime = runtime.with_sensitive_corpora(
            Arc::clone(mgr) as Arc<dyn sovereign_core::traits::SensitiveCorpusOracle>
        );
        runtime = runtime.with_folder_metadata(
            Arc::clone(mgr) as Arc<dyn sovereign_core::traits::FolderMetadataOracle>
        );
    }
    // PR2 — install the Tauri routing-events sink so the runtime can
    // fire interpretation-proposed / clarification-request /
    // turn-narration back to the desktop UI.
    runtime =
        runtime
            .with_routing_events(Arc::clone(&state.routing_events)
                as Arc<dyn sovereign_core::traits::RoutingEventSink>);

    let runtime_arc = Arc::new(runtime);
    *state.runtime.write().await = Some(Arc::clone(&runtime_arc));

    // Background warm for the deferred meta-atlas (see the BuildingRuntime
    // comment above): load the ~900MB index off the boot path and attach it
    // to the now-shared Runtime. The parse is blocking + CPU-heavy, so it
    // runs on a blocking thread; `install_meta_atlas` then flips the
    // cross-corpus boost on for subsequent turns.
    {
        let warm_runtime = Arc::clone(&runtime_arc);
        tokio::spawn(async move {
            let started = std::time::Instant::now();
            let loaded = tokio::task::spawn_blocking(|| {
                let path = corpus_engine::meta_atlas::default_meta_atlas_path();
                corpus_engine::meta_atlas::MetaAtlasIndex::load(path.as_deref())
            })
            .await;
            match loaded {
                Ok(Ok(idx)) => {
                    let atoms = idx.len();
                    warm_runtime.install_meta_atlas(Arc::new(idx));
                    tracing::info!(
                        target: "bootstrap",
                        atoms,
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        "meta-atlas(bg): cross-corpus boost ready"
                    );
                }
                Ok(Err(e)) => tracing::warn!(
                    target: "bootstrap", error = %e,
                    "meta-atlas(bg): load failed; cross-corpus boost disabled"
                ),
                Err(e) => tracing::warn!(
                    target: "bootstrap", error = %e,
                    "meta-atlas(bg): warm task panicked; cross-corpus boost disabled"
                ),
            }
        });
    }

    // Auto-resume a previously-persisted mesh so the founder sees
    // their mesh on restart and existing joiners pick up where they
    // left off. Fails soft — a missing or corrupt mesh.json never
    // blocks startup.
    //
    // Attach mode skips this: the CLI daemon has already resumed its
    // own mesh state before we probed `:9741`. Running `try_resume`
    // from this process would either be a no-op (if our `mesh` is
    // None) or fight the CLI daemon for the same mesh.json.
    // ── Commission the in-process daemon ──────────────────────────────
    //
    // Everything it needs exists by now, so it is built ONCE, total, and
    // there is no window in which a request can reach a daemon that is
    // missing half its surface. Before daemon-convergence Phase 2 this was
    // an empty daemon constructed in `AppState::new_with_mode` and filled in
    // by 17 setters spread across this function.
    if let Some((daemon_handle, cli_cfg, capability)) = daemon_services {
        // ── Commission, through THE assembler ─────────────────────────
        //
        // The desktop no longer names its own variant. It hands its parts to
        // `sovereign_mesh::assemble` — the one exhaustive match over `Launch`
        // that constructs anything (`quality/TOPOLOGY.md` §10, Falsifier 3) —
        // together with the launch `main` published, and that match decides
        // what an in-process desktop daemon composes into. A refusal is fatal:
        // a daemon that came up as the wrong shape is the hazard itself.
        let services = sovereign_mesh::assemble(
            &crate::launch_mode::get(),
            sovereign_mesh::LaunchParts::Serving {
                // `headless: None` is a CLAIM, checked by the assembler: the
                // desktop has never carried a provider factory, a shared mesh
                // store or a convergence recorder, and since Phase 3 it is not
                // distinguished by a state store either.
                headless: None,
                serving: sovereign_mesh::ServingProfile {
                core: sovereign_mesh::ServingCore {
                    // The engine peers gossip-probe over
                    // `/internal/knowledge/search`, and that `/v1/knowledge/
                    // search` reads. Present BEFORE `try_resume`, so the
                    // first gossip round already advertises real
                    // `hosted_corpora`.
                    corpus_engine: Arc::clone(&corpus_engine),
                    // The RAW provider, not the mesh-wrapped one: a peer
                    // POSTing `/v1/chat/completions` here must be served
                    // from our local model, not re-entered into our own
                    // routing wrapper and ping-ponged back out.
                    inference_provider: Arc::clone(&raw_inference),
                    // Resolves `conversation-history` chunks back to their
                    // conversation for the reading surface; without it
                    // citations render with no title. THE SAME handle this
                    // process already opened — the desktop and its in-process
                    // daemon are one writer of one `sovereign.db`, which is
                    // the invariant `RunLock` keys on the data root to hold.
                    state_store: Arc::clone(&store),
                },
                    capability,
                    advertise_embed,
                },
            },
        )
        .unwrap_or_else(|refusal| {
            panic!("desktop: cannot commission the in-process daemon: {refusal}")
        });
        let daemon =
            sovereign_mesh::EmbeddedDaemon::new(config.data_dir.clone(), cli_cfg, services);
        daemon_handle.bind(Arc::clone(&daemon));
        *state.mesh.write().await = Some(Arc::clone(&daemon));

        match daemon.try_resume().await {
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
                let Ok(indexes) = verify_engine.installed_indexes().await else {
                    continue;
                };
                let Some(info) = indexes.iter().find(|i| i.corpus_id == cs.corpus_id) else {
                    continue;
                };
                let Ok(idx) = verify_engine.open_index(&info.path).await else {
                    continue;
                };
                let ready = idx.is_vector_index_ready().await;
                let _ = verify_store
                    .set_vector_index_ready(&cs.corpus_id, ready)
                    .await;
                if !ready {
                    // Transient, self-resolving: a corpus whose vector index
                    // is still building (common on fresh installs) is served
                    // FTS-only until the build completes. This fires once per
                    // not-ready corpus on every boot, so it's info, not a
                    // warning — nothing is broken and no user action is needed.
                    tracing::info!(
                        corpus = %cs.corpus_id,
                        "Vector index not built yet — KnowledgeQuery will use FTS-only search until it finishes"
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
        let cfg: DesktopConfig = toml::from_str(legacy).expect("legacy config should deserialize");
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
}
