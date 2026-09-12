// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use corpus_engine::CorpusEngine;

use sovereign_core::insight::InsightService;
use sovereign_core::traits::{InferenceProvider, StateStore};
use sovereign_store::sqlite::SqliteStateStore;
use tokio_util::sync::CancellationToken;

// Desktop config (DesktopConfig + defaults + load/save) lives in a
// submodule; re-exported so callers keep using `crate::state::DesktopConfig`.
mod config;
pub use config::*;

// Construction helpers for `bootstrap_with_progress` (§3.3).
mod builders;

// The live wire turns + the prompts they parked (sv-surface RB5).
mod wire_turns;
pub use wire_turns::{PendingPrompts, TurnWires};

/// The ONE web-search registry for every desktop surface — chat tools, the
/// conversation tool builder, and the deep-research loop.
///
/// Re-exported, not defined: it moved to `sovereign_tools::bundles` on
/// 2026-08-26 so `CoreTurnTools` could build `search` through the same
/// orchestrator this surface always did. It was defined here and reachable
/// only from here, which is how an operator-configured Tavily key reached the
/// desktop and nothing else (ARCH §10.6). `desktop.toml`'s `[search_backend]`
/// is migrated into `[search]` once on load
/// (`DesktopConfig::migrate_legacy_search_backend`).
pub use sovereign_tools::bundles::effective_search_registry;

/// Run ONE web search through the desktop's configured backend, past the
/// egress boundary, and hand back the formatted results block.
///
/// **Why this exists as a function.** `search_web` (`commands/models.rs`) used
/// to reach `runtime.tools.get("search")` — the only thing in the desktop that
/// still required a commissioned `Runtime` handle rather than a readiness
/// gate. The daemon exposes no route that runs a named tool: `/mcp`'s
/// `tools/call` is allowlisted by `sovereign_tools::mcp_surface` (which does
/// not carry `"search"`) over a registry that holds only code-intel and notes
/// tools, and the turn's `intent: SimpleAction { tool }` is accepted on the
/// wire but discarded by both dispatchers (`sovereign-core/src/runtime/
/// authority_guard.rs:386-394`). So the tool registry was not a capability the
/// desktop could stop holding by pointing somewhere else.
///
/// It did not need to hold one. `submit_information_search`
/// (`commands/conversation.rs:509-565`) has performed exactly this search with
/// NO Runtime since it landed, through the registry re-exported above. This is
/// that path, lifted so the two surfaces share it rather than agreeing
/// (ARCH principle 8). **Owed: `conversation.rs` still spells its own copy;
/// converting it is a one-call-site change in a file this session does not
/// own.**
///
/// EGRESS CUSTODY is why this stays in the app rather than becoming a daemon
/// route — see the `DEFAULTS_LEDGER` row. The query leaves this machine for a
/// third-party search provider, and the release gate that authorises it reads
/// `user_formed: true`, which is a fact only the surface that took the
/// keystrokes can assert.
pub async fn web_search_once(
    query: &str,
    config: &DesktopConfig,
) -> Result<sovereign_tools::web::search::OrchestratedSearch, String> {
    use sovereign_tools::web::search::{SearchOrchestrator, SearchPrivacy, SelectInputs};

    let orchestrator = SearchOrchestrator::new(Arc::new(effective_search_registry()));
    let client = sovereign_contracts::egress::search_client()
        .map_err(|e| format!("egress boundary search client build: {e}"))?;
    let provider_static: &'static str = match config.search_backend.provider.as_str() {
        "tavily" => "tavily",
        "brave" => "brave",
        _ => "duckduckgo",
    };
    sovereign_contracts::egress::verify(
        &sovereign_contracts::egress::EgressPayload {
            privacy: SearchPrivacy::External {
                provider: provider_static,
            },
            custody: sovereign_contracts::types::Custody::Personal,
            what: "query",
            target: provider_static,
            detail: query,
            user_formed: true,
        },
        None,
    )
    .map_err(|r| format!("web search refused: {r}"))?;
    let prefer = match config.search_backend.provider.as_str() {
        "tavily" => &["tavily", "duckduckgo"][..],
        "brave" => &["brave", "duckduckgo"][..],
        _ => &["duckduckgo"][..],
    };
    // Glassbox: the backend decision and the query LENGTH, never its text.
    tracing::info!(
        provider = %config.search_backend.provider,
        query_len = query.len(),
        "web_search_once: dispatching"
    );
    let out = orchestrator
        .search(
            &client,
            SelectInputs {
                query,
                max_results: 5,
                max_privacy: SearchPrivacy::External {
                    provider: "duckduckgo",
                },
                prefer,
            },
        )
        .await;
    // Absence is REPORTED (ARCH principle 6). An empty result set and a
    // refused egress are different facts and neither is "no results found",
    // which is what the tool-registry path returned for both.
    if out.results.is_empty() {
        return Err(format!(
            "web search returned 0 results via {} (DDG may be bot-blocking; \
             try a tighter query)",
            out.backend_id,
        ));
    }
    Ok(out)
}

// ─── App State ───────────────────────────────────────────────

pub struct AppState {
    /// Sink for the `interpretation-proposed`, `clarification-request` and
    /// `turn-narration` Tauri events.
    ///
    /// Since 10b26809d `pump_wire_frames` (`commands/chat.rs`) re-emits all
    /// three from the wire frames it reads, so the sink is the ONE emitter
    /// of these events and nothing else in this process raises them.
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
    /// How this process bootstrapped. Used by mesh_commands and the UI
    /// badge to decide whether to drive mesh via Rust or HTTP.
    ///
    /// Read the MODE through [`AppState::is_attach_mode`], never by
    /// matching this field for anything but a port: sv-surface B4 can
    /// turn a `Local` boot into a client after this field is set, and
    /// only the accessor folds that in.
    pub bootstrap_mode: crate::bootstrap::BootstrapMode,
    /// The write halves of the LIVE wire turns (sv-surface R5), keyed by
    /// conversation (RB5): parked by the streaming commands when they open
    /// a turn socket, so `cancel_stream` and the `submit_*` commands can
    /// reach the right turn from any task while its drain keeps reading
    /// (the split the client crate's G11 bought). Empty between turns — a
    /// submit then falls back to the local desk, which still serves the
    /// in-process turn shapes that have not converted yet.
    pub turn_wire: TurnWires,
    /// Prompts the live wire turns have put to this surface and that no
    /// answer has resolved yet — the wire-side form of the local desk's
    /// `has_pending_information` guard, so the search-now affordance can
    /// still fail fast on a stale card without spending a search budget.
    /// Each entry names its conversation (the `submit_*` commands arrive
    /// with the card's `key` alone) and keeps the card, so a `WrongKind`
    /// `ResolveAck` can re-raise it.
    pub pending_prompts: PendingPrompts,
    /// QuerySession id -> conversation id, recorded as the wire delivers
    /// the routing cards (their payloads carry both). `redirect_turn`
    /// arrives with only a session id; the daemon owns the session store,
    /// so this map is the surface's own knowledge of which conversation a
    /// card belonged to.
    pub session_conversations: RwLock<std::collections::HashMap<String, String>>,
    /// Shuts down anything this app still spawns for the window's life.
    /// It outlived the health monitor it was named for (2026-09-12) and is
    /// kept because `main`'s exit path cancels it; the monitor, the insight
    /// service, the local-corpus manager and the NER handle that were
    /// declared here are all gone, each having had zero readers.
    pub health_shutdown: CancellationToken,
}

impl AppState {
    /// True. Always, since svt-3 — kept as a function because a constant
    /// would not say WHY, and because every caller is a fork that still has
    /// to come out.
    ///
    /// This used to be a real question with three answers folded into one
    /// decider: the boot probe found a daemon (`BootstrapMode::Attach`), or
    /// the run-lock claim was refused by a holder that then answered the port
    /// (sv-surface B4), or this process was about to become the daemon
    /// itself. The third is gone — `bootstrap` commissions no daemon and
    /// loads no weights — and with it the only way the answer could be
    /// `false`. The desktop is a client of a daemon it does not own; the only
    /// remaining question is which port, which [`Self::client_port`] answers.
    ///
    /// Callers do not have to change to be correct, which is the point: each
    /// one already had an attach arm, and that arm is now the only arm.
    pub fn is_attach_mode(&self) -> bool {
        true
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
        app_handle: tauri::AppHandle,
        mode: crate::bootstrap::BootstrapMode,
    ) -> Self {
        let config = DesktopConfig::load();
        // The daemon is NOT constructed here. In Local mode `bootstrap`
        // commissions it at the end, once the engine, provider, tool mount and
        // routers it needs actually exist — persisting its running-mesh state
        // into `<config.data_dir>/mesh.json` so a create/join survives an app
        // restart. In Attach mode it is never constructed at all;
        // `is_attach_mode()` answers "should there ever be one?" so nobody
        // has to read `mesh.is_none()` as an answer to it.

        // The routing event sink — the one emitter of the three
        // antifragile-routing events (`commands/chat.rs` feeds it from
        // the wire frames). It used to borrow the AppHandle off the
        // in-process approval channel; that channel went with the
        // Runtime (2026-09-11), so the handle comes in directly.
        let routing_events = Arc::new(crate::routing_events::TauriRoutingEventSink::new(
            app_handle,
        ));

        Self {
            routing_events,
            config: RwLock::new(config),
            inference: RwLock::new(None),
            store: RwLock::new(None),
            sqlite_store: RwLock::new(None),
            corpus_engine: RwLock::new(None),
            install_progress: RwLock::new(HashMap::new()),
            bootstrap_mode: mode,
            health_shutdown: CancellationToken::new(),
            turn_wire: TurnWires::default(),
            pending_prompts: PendingPrompts::default(),
            session_conversations: RwLock::new(std::collections::HashMap::new()),
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
///
/// **Five of the seven have no emitter after svt-3b**, each for the same
/// reason and each saying so on its own variant: they narrated work this
/// process performed and the daemon performs now. They are kept for one
/// commit because `setup_flow.rs`'s match over them is another worker's file
/// this session, and a concurrent edit there is a collision rather than a fix.
/// Deleting the five and their arms is OWED. What survives is what this
/// bootstrap still does: `OpeningDatabase` and `WiringKnowledge`.
#[derive(Debug, Clone, Copy)]
pub enum BootstrapPhase {
    /// **Nothing emits this since svt-3**, and the variant survives only
    /// because `setup_flow`'s match over these phases is another worker's
    /// file this session. It named the crash-isolation subprocess that
    /// guarded an in-process GGUF load; this process performs no such load,
    /// so there is nothing to smoke-test. Deleting it and its arm is owed.
    SmokeTesting,
    /// **Nothing emits this since svt-3** — same reason as [`Self::SmokeTesting`]
    /// above, and the same owed deletion. It named
    /// `EmbeddedLlamaCpp::load_full_with_families` mmapping a GGUF in this
    /// process; the daemon is what does that now, and the wait a user sees is
    /// the daemon's readiness, not this process's.
    LoadingModel,
    /// About to open the SQLite store and run migrations.
    OpeningDatabase,
    /// **No emitter since svt-3b.** The router classifier stack is the shared
    /// recipe's work and this process no longer commissions a turn; the
    /// daemon assembles it, and this splash cannot narrate a wait happening
    /// in another process. Same owed deletion as the two above.
    AssemblingRouter,
    /// **No emitter since svt-3b** — the same router, on the cache-miss path
    /// that re-embeds ~277 exemplars. It is still a real minutes-long wait on
    /// a cold CPU embed slot; it is just the DAEMON's wait now, and narrating
    /// it needs a readiness signal off `/status`, not a variant here.
    RebuildingRouterEmbeddings,
    /// About to wire tools, corpus engine, local-corpus manager and
    /// knowledge view (lance index opens scale with installed corpora).
    WiringKnowledge,
    /// **No emitter since svt-3b.** It named the turn's enrichment lane —
    /// atlases, the wiki link graph, the reranker, GLiNER — and was emitted by
    /// the shared recipe through this file's `SplashProgress` adapter. There
    /// is no recipe call and no adapter; the lane is built by the daemon.
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

    // Glassbox: the mode this boot resolved, at the top of the spine
    // that behaves differently because of it. `bootstrap::detect` logs
    // the PROBE's conclusion; this logs what the state object actually
    // carries, including the B4 run-lock attach below, which is the
    // value every later branch reads (sv-surface D9).
    tracing::info!(
        target: "bootstrap",
        attach = state.is_attach_mode(),
        client_port = state.client_port(),
        internal_port = state.internal_port(),
        "bootstrap: boot mode resolved"
    );

    // Model-slot paths live in `SetupConfig` (`~/.svrnmesh/config.toml`) —
    // the single source of truth, shared with the daemon. Resolve them once
    // here; the CPU-compat policy may mutate this in memory, and the
    // inference builder loads from it.
    let slots = ResolvedModelSlots::load()
        .map_err(|e| format!("No model configuration found ({e}). Complete setup first."))?;

    if slots.fast.as_os_str().is_empty() || !slots.fast.exists() {
        return Err(format!(
            "Model not found: {}. Place a GGUF model file at this path.",
            slots.fast.display()
        ));
    }

    // ── A serving host, or a named refusal ───────────────────────────────
    //
    // This process holds no weights and commissions no daemon (svt-3), so a
    // boot that cannot reach one has nothing to attach to. Reporting that is
    // the whole handling: a desktop that carried on would build an HTTP
    // provider pointed at a dark port and report itself ready (ARCH
    // principle 6).
    //
    // Asked HERE and not only in `main` because `main`'s look runs BEFORE the
    // setup wizard writes `config.toml` — on a first launch there is no port
    // to probe and no models for a daemon to load, so `ensure_reachable`
    // returns `None` by construction and says so. The `ResolvedModelSlots`
    // load above is `SetupConfig::load()`, so reaching this line means a
    // config is on disk: this is the first moment the question can be
    // answered, which is what `setup_flow::relaunch_after_setup` means when
    // it says the relaunch exists only because the look happened too early.
    //
    // It is a REACH, not a start: `ensure_reachable` retains no handle, no
    // retry and no policy, and a host it did not bring up is the common case.
    match crate::serving_host::ensure_reachable().await {
        Some(reached) => tracing::info!(
            target: "bootstrap",
            ?reached,
            client_port = state.client_port(),
            "bootstrap: a serving host answered — this desktop is its client"
        ),
        None => {
            // Two refusals, because they ask the operator for different
            // things and a single sentence would send half of them looking in
            // the wrong place.
            let host = crate::launch_mode::daemon_host();
            return Err(match host {
                sovereign_contracts::launch::DaemonHost::InProcess(_) => format!(
                    "this launch asked THIS process to run the weights ({why}), and the app can \
                     no longer do that: it loads no models and starts no daemon. Unset that \
                     variable and launch again, or start a daemon yourself with `svrn daemon \
                     start`.",
                    why = host.as_str(),
                ),
                sovereign_contracts::launch::DaemonHost::SupervisedChild => format!(
                    "no daemon is serving :{port} and this app could not bring one up. Start one \
                     with `svrn daemon start`, or reinstall so the bundled backend is beside the \
                     app. The app is a client; it does not run the models itself.",
                    port = state.client_port(),
                ),
            });
        }
    }

    // ── The CPU/arch compatibility gate is GONE from here, and that is a fix ──
    //
    // `builders::model_compat::apply_cpu_compat_policy` ran at this line. On a
    // CPU-only machine whose configured chat model is a recurrent arch that
    // SIGSEGVs in ggml's CPU prefill (qwen35, Mamba/SSM, RWKV) it substituted a
    // dense model IN MEMORY — mutating `slots`, never `config.toml` — and
    // raised a `model-notice` banner saying so.
    //
    // That was correct while THIS process loaded the weights. It has been
    // wrong in attach mode the whole time it has existed, and svt-3 makes
    // attach the only mode: the substitution cannot reach the daemon (it is
    // in-memory, and the daemon reads the file), so all it did was change
    // which model id `build_daemon_provider` derives — the desktop asking
    // `/v1/chat/completions` for a slot the daemon never loaded — while
    // telling the user a swap had happened that had not (ARCH principle 6).
    //
    // Deciding which weights are safe to load is the job of whoever loads
    // them. `sovereign_inference::cpu_compat` is already a shared crate, so
    // the decider does not move — only its caller does, to the daemon's slot
    // build. Until it lands the guard has NO owner: `sovereign/DEFAULTS_LEDGER.md`
    // carries the row, with the flip condition and a review-by.

    // Inference is the DAEMON's, over HTTP, and there is one provider rather
    // than two. `load_inference` returns the pair because the tiered-enrichment
    // builders below take an owned handle each; both halves are now the same
    // `Arc` (`SplitInferenceProvider`, `builders/inference.rs`).
    //
    // What used to stand here was the split that in-process hosting needed: a
    // `raw_inference` for peers POSTing `/v1/chat/completions` at our own
    // `:9741`, and a `MeshInferenceProvider` wrapper routing THIS user's
    // Slow-slot work to a beefier peer. Both belonged to the daemon this
    // process was; it is no longer one, and peer routing is served by the
    // daemon at the other end (svt-3).
    let (raw_inference, inference) =
        builders::inference::load_inference(&state.inference, &slots, &emit).await?;

    // Open database.
    let store: Arc<dyn StateStore> =
        builders::store::open_store(&state.store, &state.sqlite_store, &config, &emit).await?;

    // The recipe-author `notes.db` + `features.db` opens stood here and went
    // with the commission (svt-3b). They had NO reader on the command surface
    // — measured, and it is why they survived D9b: their only consumer was
    // the `RecipeAuthoringTools` bundle handed to the recipe. The daemon opens
    // the same two files for its own commission, and `recipe_author_commands`
    // + `lessons` already reach them over `/v1/features/*` and `/v1/notes`.

    // ── This bootstrap no longer assembles a turn ────────────────────────
    //
    // Everything between here and the corpus engine used to build the things
    // a turn needs and hand them to the shared runtime recipe's `common_parts`
    // -> `commission`: a `SkillRegistry` loaded from built-ins plus the user's
    // skills dir, eleven `ToolBundle`s, the merged SCIP graph the code-intel
    // tools hold, the mesh knowledge client, the landscape-digest provider and
    // a `KnowledgeViewManager`. The daemon commissions all of it through the
    // SAME recipe, and every turn has crossed the wire since sv-surface R5 —
    // so the desktop's copy answered no question any surface asked. It is
    // gone, and with it the `sovereign-runtime-recipe` dependency.
    //
    // What this phase still opens is what the desktop's OWN surfaces read: the
    // corpus engine (the corpus pane, focused-passage augmentation), the
    // local-corpus manager (Local Knowledge), the state store and the
    // enrichment stack. Those are lance opens that scale with installed
    // corpora, which is why the phase is announced.
    emit(BootstrapPhase::WiringKnowledge);

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
    // Resolve the daemon's node_id so partition_path() returns a
    // directory name this app's engine and the daemon both agree on
    // (`<corpus>-partition-node-<hex>`). Without this the engine defaults
    // to `self_node_id = "local"` and `in_progress_ingestions` silently
    // misses partition-of-self directories, leaving the UI stuck on
    // "Install" for corpora that are actively being ingested on disk.
    //
    // ASKED OF THE DAEMON, not read off its disk. Until svt-3 this read
    // `<data_dir>/node_id`, then `mesh.json`, then GENERATED an id and
    // wrote the file — a client minting the daemon's identity (ARCH
    // principle 12), and a second minter of it beside the daemon's own
    // `persist::load_or_generate_self_node_id`. `GET /status` carries the
    // id in the same `NodeId` Display form `partition_path` keys on.
    //
    // The host answered `ensure_reachable` above, so a failure HERE is a
    // host that is up and will not say who it is. That is a refusal of the
    // boot, in the host's words — never a locally generated id: an engine
    // partitioned under an invented id would report every in-flight ingest
    // as absent and every partition as someone else's (ARCH principle 6).
    let self_node_id = sovereign_turn_client::TurnClient::new(state.client_base_url())
        .daemon_status::<sovereign_contracts::daemon_wire::DaemonIdentity>()
        .await
        .map_err(|e| {
            format!(
                "the serving host on :{port} answered the reachability probe but not \
                 `GET /status` ({e}); this app partitions its corpus engine by the \
                 host's node id and will not invent one",
                port = state.client_port(),
            )
        })?
        .node_id;

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
            .with_self_node_id(self_node_id);
    if let Some(tiered_provider) = folder_tiered_provider {
        engine_builder = engine_builder.with_tiered_provider(tiered_provider);
    }
    if let Some(extractor) = chunk_entity_extractor.clone() {
        engine_builder = engine_builder.with_chunk_entity_extractor(extractor);
        // A SECOND handle on the same model — a `LazyGlinerExtractor` as
        // `dyn EntityExtractor` — was published to `state.entity_extractor`
        // here for document ingest's skeleton pass. It is gone: that pass is
        // the daemon's since 2d5b569f6, which hands its manager
        // `runtime.lane().gliner`, and the slot had ZERO readers on this side
        // afterwards.
    }
    // A custom acquirer must be registered on EVERY engine that can
    // ingest a recipe naming it, or the install fails at acquire time
    // with "No custom acquirer registered for kind 'sec_edgar'". The
    // desktop's embedded daemon is one of those engines.
    sovereign_tools::sec_edgar::register(&engine_builder);
    let corpus_engine = Arc::new(engine_builder);
    *state.corpus_engine.write().await = Some(Arc::clone(&corpus_engine));

    // The LocalCorpusManager stood here and is gone (thin-desktop, 2026-09-12).
    // It had ZERO readers: `state.local_corpus` was written once at the end of
    // this block and never read again, because `lc_pre_scan` (6e47d0abe) was
    // the last command to hold a manager and it took the wire. What remained
    // was ~115 lines commissioning a manager, its enrichment defaults and its
    // tiered-enrichment deps so that nothing could ask it anything — a count
    // that had reached zero with the ability fully intact on the daemon side
    // (ARCH principle 12). `lc_http` serves all fifteen routes over the
    // daemon's OWN manager.

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

    // Startup dimension guard: probe the loaded embed model's actual output
    // size and compare against every installed corpus index. A mismatch means
    // the user swapped embed models after building their library — retrieval
    // will silently return wrong results unless they rebuild.
    //
    // It no longer ADVERTISES that dimension: `EmbedAdvertisement` was what a
    // node tells its peers about its embedding model, and this process is not
    // a node any more — the daemon it attaches to advertises its own (svt-3).
    if slots.has_embed() {
        // Err => embed not configured or failed — skip validation.
        let t_embed_probe = std::time::Instant::now();
        if let Ok(probe_vec) = inference.embed("probe").await {
            substep("embed_probe", t_embed_probe);
            let dims = probe_vec.len();
            // Arm clause ST-8's geometry gate. Until this fires, `open_index`
            // cannot tell a 768-dim corpus from a 1024-dim one and admits
            // both; `oicp-types` on the maintainer's host is exactly that
            // case, recording the SAME model name as the compatible corpora.
            corpus_engine.set_expected_embedding_dimensions(dims);
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
        }
    }

    // The health monitor stood here and is gone (thin-desktop, 2026-09-12).
    // `state.health_monitor` was written by the builder and never read: the
    // app renders health from the daemon's own `/status` and
    // `/{corpus}/health`. A monitor polling a store, an engine and an
    // inference provider inside a client, whose verdict nothing could ask
    // for, is the same zero-reader shape as the three slots beside it.

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

/// Re-run bootstrap after a config change. **There is no Runtime to rebuild
/// any more** — the name is kept for one commit because its three callers live
/// in `commands/config_setup.rs`, another worker's file this session, and a
/// rename there is a collision rather than a fix. Owed: fold it into
/// [`bootstrap`], which is now byte-for-byte what it does (ARCH principle 8 —
/// two names for one decider).
///
/// What the callers actually want is unchanged and still happens: they set
/// `state.inference` to `None` first, so bootstrap builds a fresh provider
/// against whatever port and model ids the new config names.
pub async fn rebuild_runtime(state: &AppState) -> Result<(), String> {
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
