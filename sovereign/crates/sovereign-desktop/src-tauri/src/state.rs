// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use sovereign_contracts::traits::InferenceProvider;

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
pub use sovereign_tools_base::web::search::effective_search_registry;

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
) -> Result<sovereign_tools_base::web::search::OrchestratedSearch, String> {
    use sovereign_tools_base::web::search::{SearchOrchestrator, SearchPrivacy, SelectInputs};

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
    // The `inference` slot stood here and is GONE (svt-7, 2026-09-12). It held
    // a `SplitInferenceProvider` over the daemon's `/v1` and had ZERO readers
    // — its only remaining touches were two `= None` resets, which is a count
    // that reached zero with the ability fully intact on the other side (ARCH
    // principle 12). Turns go over the wire through `TurnClient`; the app does
    // not hold a provider to ask.
    // The `store` and `sqlite_store` slots stood here and are GONE
    // (thin-desktop R2, 2026-09-12). They held this process's own handles on
    // a `sovereign.db` — a SECOND opener beside the daemon's, and on an
    // attached boot a different file from the one every turn is written to.
    // Their five readers each took a route: `get_conversation` ->
    // `GET /v1/conversations/{id}` (which grew `enabled_corpora` for it),
    // `search_web` and `explore_insights` ->
    // `POST /v1/conversations/{id}/messages/record`, `get_chat_activity` ->
    // `GET /v1/admin/chat-activity`, and `lc_reenrich_note`'s correction
    // ledger -> the widened `enrich/reenrich-note` body. The desktop opens
    // no database and runs no migration on the daemon's data root.
    // The `corpus_engine` slot stood here and is GONE (svt-6). It held a
    // FULL `CorpusEngine` over `~/.svrnmesh/{recipes,indexes}` — the
    // daemon's own root, opened a second time by a client, with every
    // `.with_*` in its builder chain paired one-for-one against
    // `sovereign-cli-daemon/src/daemon_cmd/bootstrap.rs:276-296`. Its last
    // reader was the recipe harness's rung-6 verify, which is
    // `POST /internal/corpus/recipes/harness` now; the corpus pane reads
    // `GET /internal/corpus/catalog` and has since 2a9a9e91e.
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

    // `AppState::store()` stood here and is GONE with the slots above
    // (thin-desktop R2). Its doc called it "daemon-convergence Phase 0: the
    // desktop's dependency on `sovereign_core::Runtime` narrows to the ports
    // that actually answer a turn, so the Runtime can later move into the
    // daemon without dragging the desktop's DB access with it." The Runtime
    // moved at svt-3b; this is the DB access following it.

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
            install_progress: RwLock::new(HashMap::new()),
            bootstrap_mode: mode,
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

    // The `substep` glassbox timer stood here and is GONE (svt-6). Its two
    // remaining callers were the embed probe and `validate_corpus_readiness`
    // below, both of which went with the engine; a closure with no call site
    // is a count that reached zero (ARCH principle 12). What it was for — a
    // slow boot self-attributing — is the daemon's boot now, and the daemon
    // times its own.

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
    // here; the CPU-compat policy may mutate this in memory.
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

    // Inference is the DAEMON's, over HTTP, and this process holds no handle
    // on it at all since svt-7 (see the slot's epitaph on `AppState`).
    //
    // What used to stand here was the split that in-process hosting needed: a
    // `raw_inference` for peers POSTing `/v1/chat/completions` at our own
    // `:9741`, and a `MeshInferenceProvider` wrapper routing THIS user's
    // Slow-slot work to a beefier peer. Both belonged to the daemon this
    // process was; it is no longer one, and peer routing is served by the
    // daemon at the other end (svt-3).
    // The provider construction stood here and is GONE (svt-7). It built a
    // `SplitInferenceProvider` over the daemon's `/v1`, filled
    // `state.inference` — and NOTHING read that slot. The count had reached
    // zero with the ability fully intact on the daemon side, which is the
    // shape ARCH principle 12 names: the app's two remaining writers only
    // ever reset it to `None`.
    //
    // The `?` on it carried one thing worth keeping — the boot's refusal when
    // no embedding model is configured — so that refusal is stated here
    // directly rather than surviving as a side effect of a construction with
    // no readers. Same sentence, same Settings pointer.
    if !slots.has_embed() {
        return Err("no embedding model configured (Settings → Embedding model)".to_string());
    }

    // The database open stood here and is GONE (thin-desktop R2). It was
    // `builders::store::open_store`, and the builder file went with it —
    // including its `busy_timeout` and its migration run, which is the half
    // worth naming: a client must not run migrations on a data root it does
    // not own, and this one did, on every boot, against the file the daemon
    // was serving from.

    // The recipe-author `notes.db` + `features.db` opens stood here and went
    // with the commission (svt-3b). They had NO reader on the command surface
    // — measured, and it is why they survived D9b: their only consumer was
    // the `RecipeAuthoringTools` bundle handed to the recipe. The daemon opens
    // the same two files for its own commission, and `recipe_author_commands`
    // + `lessons` already reach them over `/v1/features/*` and `/v1/notes`.

    // ── This bootstrap opens no knowledge engine ─────────────────────────
    //
    // Everything between here and the end of this function used to build the
    // things a turn needs and hand them to the shared runtime recipe's
    // `common_parts` -> `commission`, and then a FULL `CorpusEngine` beside
    // it. The recipe went at svt-3b; the engine goes here (svt-6), and it is
    // the same finding both times — the daemon builds the identical thing
    // over the identical data root. `state.rs:622-645` and
    // `sovereign-cli-daemon/src/daemon_cmd/bootstrap.rs:276-296` paired every
    // `.with_*`: the same recipes dir, the same indexes dir, the same
    // embedding model name, the same tiered provider, the same GLiNER
    // extractor, the same `sec_edgar` acquirer.
    //
    // What fell with it, and where each answer comes from now:
    //
    // - `sovereign_tools::corpus::inference_to_{embed,batch_embed,inference}_fn`
    //   — adapters that existed only to feed the engine's builder.
    // - `sovereign_gliner::load_gliner_extractor` and
    //   `sovereign_tools::enrichment_bootstrap::build_folder_tiered_provider`
    //   — the enrichment stack the engine held. The daemon holds its own.
    // - `sovereign_tools::sec_edgar::register` — a custom acquirer must be
    //   registered on every engine that can ingest a recipe naming it, and
    //   there is no engine here to register it on. The daemon registers it
    //   on the one that ingests (`bootstrap.rs`).
    // - The `GET /status` node-id read, which existed ONLY to partition this
    //   engine's directory names against the daemon's. No engine, no
    //   partition, no reason to ask. `ensure_reachable` above is still the
    //   boot's refusal when no host answers.
    // - `state.corpus_engine` itself. Its last reader was
    //   `commands/recipe_testing.rs`'s rung-6 verify, which is
    //   `POST /internal/corpus/recipes/harness` now.
    //
    // `load_inference` and the slot it filled went at svt-7; what it carried
    // that had a reason to live — the boot's refusal when no embedding model
    // is configured — is stated above as itself.
    emit(BootstrapPhase::WiringKnowledge);

    // The LocalCorpusManager stood here and is gone (thin-desktop, 2026-09-12).
    // It had ZERO readers: `state.local_corpus` was written once at the end of
    // this block and never read again, because `lc_pre_scan` (6e47d0abe) was
    // the last command to hold a manager and it took the wire. What remained
    // was ~115 lines commissioning a manager, its enrichment defaults and its
    // tiered-enrichment deps so that nothing could ask it anything — a count
    // that had reached zero with the ability fully intact on the daemon side
    // (ARCH principle 12). `lc_http` serves all fifteen routes over the
    // daemon's OWN manager.

    // The lazy canonical-fingerprint stamp stood here and is GONE (svt-6).
    // Its own comment said it "mirrors the daemon-mode bootstrap", and it
    // did, exactly: `bootstrap::spawn_lazy_stamp_fingerprints`
    // (`sovereign-cli-daemon/src/daemon_cmd/bootstrap.rs:1658`, called from
    // `daemon_cmd/mod.rs:872`) runs the same `lazy_stamp_legacy_fingerprints`
    // over the same `~/.svrnmesh/indexes` root, supervised. Two processes
    // racing one idempotent pass is not a second answer, it is a second
    // writer — and the one that owns the root keeps it (ARCH principle 8).

    // The startup dimension guard stood here and is GONE (svt-6). It probed
    // THIS process's embed provider — which is the daemon's, over HTTP — and
    // armed clause ST-8's geometry gate on an engine only this process held.
    // The daemon arms its own gate from its own probe, over the engine that
    // actually serves retrieval (`engine.set_expected_embedding_dimensions`,
    // `sovereign-cli-daemon/src/daemon_cmd/mod.rs:886`), so the arming that
    // matters was never this one.
    //
    // `validate_corpus_readiness` went with it, and it is the one thing here
    // that is a DELETE rather than a duplicate: `state.rs` was its only
    // caller in the workspace (`corpus-engine/src/engine/mod.rs:2141` is the
    // definition), and its whole effect was a `tracing::warn!` in a client's
    // log that no surface read. The geometry it warned about is refused at
    // `open_index` by the gate the daemon arms above.

    // The health monitor stood here and is gone (thin-desktop, 2026-09-12).
    // `state.health_monitor` was written by the builder and never read: the
    // app renders health from the daemon's own `/status` and
    // `/{corpus}/health`. A monitor polling a store, an engine and an
    // inference provider inside a client, whose verdict nothing could ask
    // for, is the same zero-reader shape as the three slots beside it.

    // The vector-index readiness sweep stood here and MOVED to the daemon
    // (svt-6): `bootstrap::spawn_vector_index_readiness_sweep`, called beside
    // the lazy stamp at `daemon_cmd/mod.rs`. It self-heals the index's own
    // on-disk `IndexMeta.vector_index_built` — `is_vector_index_ready` calls
    // `mark_vector_index_built` when LanceDB reports a complete index the
    // meta had not recorded — and its ONE reader in the workspace is
    // `corpus_catalog_http::catalog`, which prefers exactly that meta field
    // (`sovereign-mesh/src/corpus_catalog_http.rs:420-427`). Sweep and reader
    // now run in one process over one engine.
    //
    // It was MOVED, not deleted, and the difference is user-visible: with no
    // sweep anywhere, a corpus whose LanceDB index finished but whose meta
    // predates the field reports FTS-only forever, and the catalogue would
    // keep saying so. The old comment here called that "a named gap"; this is
    // the gap closed on the side that owns the root (ARCH principles 6, 12).

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
/// What the callers actually want is unchanged and still happens: bootstrap
/// re-reads `SetupConfig`, so the next turn rides whatever port and model ids
/// the new config names. (They used to also clear `state.inference`; that slot
/// went at svt-7 and the app holds no provider to clear.)
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
