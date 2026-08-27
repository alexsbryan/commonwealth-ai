// SPDX-License-Identifier: AGPL-3.0-or-later
mod activity;
mod approval;
mod auth;
mod busy;
mod config;
#[cfg(feature = "dev-routes")]
mod corpus_upload;
mod iroh_access;
mod narration;
mod reciprocity;
mod routes;
mod routes_documents;
// MCP dispatches through `routes_tdd::TddState`, so the two travel together.
#[cfg(feature = "dev-routes")]
mod routes_mcp;
#[cfg(feature = "dev-routes")]
mod routes_tdd;
mod scheduler;
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

use sovereign_core::traits::StateStore;
use sovereign_core::SkillRegistry;
use sovereign_inference::embedded::EmbeddedLlamaCpp;
use sovereign_inference::health::HealthTracker;
use sovereign_inference::hybrid::HybridProvider;
use sovereign_inference::remote::RemoteApiProvider;
use sovereign_inference::selector::BackendEntry;
use sovereign_store::sqlite::SqliteStateStore;

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
            sovereign_inference::fast_exit_skip_destructors(1);
        }
    };

    let config = match ServerConfig::load(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Configuration error: {e}");
            sovereign_inference::fast_exit_skip_destructors(1);
        }
    };

    // Fail-closed exposure check — before any model load, so a
    // misconfigured remote bind errors instantly rather than after the
    // weights are in memory.
    let auth_enabled = config.auth.mode == "api_key" && !config.auth.keys.is_empty();
    if let Err(e) = config::validate_exposure(&config.server, auth_enabled) {
        eprintln!("Configuration error: {e}");
        sovereign_inference::fast_exit_skip_destructors(1);
    }

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
                // `sovereign-server` carries its OWN config type, which has
                // not grown a per-slot key. `uniform` names that honestly
                // rather than leaving it indistinguishable from a host that
                // simply forgot to split its windows.
                sovereign_inference::embedded::SlotWindows::uniform(config.inference.context_size),
                None,
            ) {
                Ok(p) => Arc::new(p),
                Err(e) => {
                    eprintln!("Failed to load model: {e}");
                    sovereign_inference::fast_exit_skip_destructors(1);
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
                            sovereign_inference::embedded::SlotWindows::uniform(bc.context_size),
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
                sovereign_inference::fast_exit_skip_destructors(1);
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
            sovereign_inference::fast_exit_skip_destructors(1);
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

    // Build components. The router classifier stack, the planner, the turn's
    // tool registry and the whole enrichment lane are the shared recipe's
    // (`sovereign_runtime_recipe::common_parts`, called below). This file held
    // near-copies of all four until 2026-08-26, and the copies had drifted in
    // both directions: the recipe carried `lane.bridge` and the cross-corpus
    // bridge boost, this host did not; this host carried twenty tools the
    // recipe had no way to express until the `ToolBundle` seam landed.
    //
    // Construct a shared CorpusEngine for the epistemic tools.
    let home = sovereign_contracts::rebrand::svrnmesh_root();
    let recipes_dir = home.join("recipes");
    let indexes_dir = home.join("indexes");
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
    // Same invariant as the daemon and the desktop: a recipe naming a
    // custom acquirer kind can only be ingested by an engine that has
    // that kind registered.
    sovereign_tools::sec_edgar::register(&corpus_engine);
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
        // Memory-pool RAPTOR rebuild (T3 tiered-retrieval memory port):
        // rides the debouncer's MemoryTouched window alongside the
        // personal view.
        mgr.install_memory_atlas(Arc::clone(&store), Arc::clone(&inference))
            .await;
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

    // ── The tool families this hub carries ───────────────────────────────
    //
    // Composed, not registered. What stood here was 31 `tools.register` calls
    // over 230 lines, and the reason it stayed that way is worth naming: the
    // shared recipe used to OWN its tool list, so adopting it read as "lose
    // twenty capabilities" and the phase stalled on a question that looked
    // like policy and was structure (TOPOLOGY §10 phase 7b). A bundle inverts
    // it — this host names the families it can provide, the recipe folds them,
    // and neither has to hold the other's list.
    //
    // Two capabilities arrive with the baseline that this host did not have:
    // `knowledge_lookup` (the unified corpus + memory + notes envelope) and
    // `attached_document_search`. Both were measured as CLI-only by
    // `sovereign-core/tests/turn_tool_census.rs` and are the divergence that
    // census exists to close.

    // SCIP call graph database. The call-graph tools take
    // `Arc<ArcSwap<ScipGraph>>` so the CLI's `project serve` can hot-swap the
    // graph when the on-disk DB changes; this server opens a single on-disk DB
    // at a fixed path and does not swap, but the tool signature still requires
    // the wrapper. Built here because `CodeIntelTools` cannot be constructed
    // without it — the privilege is the handle, so this host can only offer
    // code intel over an index it actually owns.
    let scip_db_path = home.join("indexes").join("_scip_graph.db");
    let scip_graph =
        corpus_engine_scip::ScipGraph::open(&scip_db_path, "default").expect("SCIP graph database");
    let scip_graph: sovereign_tools::ScipGraphHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(scip_graph));

    // Working notes — one open store, read by two families: the general note
    // tools and `knowledge_lookup`'s third evidence channel.
    let notes_db_path = home.join("notes.db");
    let note_store_for_runtime: Option<Arc<corpus_engine_notes::NoteStore>> =
        match corpus_engine_notes::NoteStore::open(&notes_db_path) {
            Ok(store) => Some(Arc::new(store)),
            Err(e) => {
                tracing::warn!(error = %e, "notes.db unavailable — note tools disabled");
                None
            }
        };
    let features_store = note_store_for_runtime.as_ref().and_then(|_| {
        let features_db = home.join("features.db");
        match sovereign_store::recipe_project_store::RecipeProjectStore::open(&features_db) {
            Ok(f) => Some(Arc::new(f)),
            Err(e) => {
                tracing::warn!(error = %e, "features.db unavailable — recipe-author checkpoint/capability tools disabled");
                None
            }
        }
    });

    let tool_bundles: Vec<Box<dyn sovereign_contracts::tool_bundle::ToolBundle>> = {
        use sovereign_tools::bundles as fam;
        // `--no-default-features` keeps corpus search and drops the open-web
        // fallback: corpus search IS the product, and reaching the open web is
        // a separate capability a zero-egress deployment cannot have. The
        // refusal is a value that travels into the bundle report, not a
        // missing line (ARCH §18.3).
        #[cfg(feature = "net-tools")]
        let web = fam::WebReach::Granted(
            sovereign_core::egress::search_client().expect("egress boundary search client build"),
        );
        #[cfg(not(feature = "net-tools"))]
        let web = fam::WebReach::Withheld("built without the `net-tools` feature");

        let mut b =
            sovereign_runtime_recipe::baseline_bundles(sovereign_runtime_recipe::BaselineDeps {
                store: &store,
                inference: &inference,
                corpus_engine: &corpus_engine,
                note_store: note_store_for_runtime.as_ref(),
                web,
                // A multi-tenant hub does not spend a tenant's turn on an
                // un-asked-for web search. The user-in-loop escalation card
                // stays available.
                escalation: fam::WebEscalation::Disabled,
            });

        // `wikipedia_fetch` reaches en.wikipedia.org, so it follows the same
        // feature switch as the rest of egress.
        #[cfg(feature = "net-tools")]
        b.push(Box::new(fam::WikipediaTools::new(Arc::clone(
            &corpus_engine,
        ))));
        #[cfg(not(feature = "net-tools"))]
        b.push(Box::new(sovereign_contracts::tool_bundle::Withheld::new(
            "wikipedia",
            "built without the `net-tools` feature",
        )));

        // Shell is a developer capability. Its approval grant is cached
        // store-wide (executor.rs) and a pending approval blocks a scheduler
        // permit with no timeout, so on a shared box it is both a privilege
        // and an availability risk.
        #[cfg(feature = "dev-routes")]
        b.push(Box::new(fam::ShellTools));
        #[cfg(not(feature = "dev-routes"))]
        b.push(Box::new(sovereign_contracts::tool_bundle::Withheld::new(
            "shell",
            "a shared hub does not run commands as the invoking user; built \
             without the `dev-routes` feature",
        )));

        b.push(Box::new(fam::ComputeTools));
        b.push(Box::new(fam::DocumentOperations::new(
            Arc::clone(&store),
            Arc::clone(&inference),
            // An HTTP hub has no window to narrate map-reduce phases to.
            fam::DocumentProgress::Silent,
        )));
        b.push(Box::new(fam::CodeIntelTools::new(
            Arc::clone(&corpus_engine),
            Arc::clone(&inference),
            Arc::clone(&scip_graph),
        )));
        match note_store_for_runtime.as_ref() {
            Some(ns) => b.push(Box::new(fam::NotesTools::new(Arc::clone(ns)))),
            None => b.push(Box::new(sovereign_contracts::tool_bundle::Withheld::new(
                "notes",
                "notes.db would not open on this host",
            ))),
        }
        // Recipe- and workflow-authoring, driven over the conversation API by
        // a conversation tagged `skill_id = "recipe-author"` /
        // `"workflow-author"`. The narrowed catalog only surfaces these when
        // `active_mode` matches, so generic chat is unaffected.
        b.push(Box::new({
            let mut ra = fam::RecipeAuthoringTools::new();
            if let Some(ns) = note_store_for_runtime.as_ref() {
                ra = ra.with_notes(Arc::new(
                    sovereign_tools::recipe_notes_adapter::NoteStoreRecipeNotes::new(Arc::clone(
                        ns,
                    )),
                )
                    as Arc<dyn sovereign_contracts::recipe::notes::RecipeNotes>);
            }
            if let Some(fs) = features_store.as_ref() {
                ra = ra.with_features(Arc::clone(fs));
            }
            ra
        }));
        b.push(Box::new(sovereign_workflow_host::WorkflowAuthoringTools));
        b
    };

    // Approval channel.
    let (approval_channel, _event_rx) = ServerApprovalChannel::new();
    let approval = Arc::new(approval_channel);

    // Glassbox progress: republish the runtime's turn narration on a
    // broadcast channel so each WS turn can forward its own stage frames
    // (retrieval / synthesis / gap-check / tool calls) to the client.
    // The `Sender` is layered as an Extension for `ws::stream_turn`.
    let (narration_sink, narration_tx) = crate::narration::BroadcastRoutingEventSink::new();

    // Install the landscape-digest provider only when KnowledgeView is
    // enabled. When disabled the splice path stays a no-op — identical to
    // pre-KnowledgeView behaviour. Captured here, applied at the commission
    // below: `landscape_digests` is one of the five capabilities §3.5 has
    // LEAVING the Runtime (a per-connection wire concern; the core holds no
    // sink), so it is deliberately not a lane member.
    let landscape_digests: Option<Arc<dyn sovereign_core::traits::LandscapeDigestProvider>> =
        knowledge_view_manager
            .as_ref()
            .map(|mgr| Arc::clone(mgr) as Arc<dyn sovereign_core::traits::LandscapeDigestProvider>);

    // ── The hub commissions through THE shared recipe ────────────────────
    //
    // The router's authority probe, the MCP connections, the atlas manager and
    // its bump flusher, the wiki graph, the reranker, GLiNER and the
    // meta-atlas were all written out here, in this order, as a near-copy of
    // the recipe's. Near-copies drift in both directions and both directions
    // had already happened: this host never wired `lane.bridge` (so the
    // cross-corpus bridge boost was dark on the surface with the most corpora)
    // and never wired `conv_tiered` (so per-conversation RAPTOR signposts
    // never reached a hub turn), while the recipe had no way to express the
    // twenty tools above until the bundle seam landed.
    let common = sovereign_runtime_recipe::common_parts(
        sovereign_runtime_recipe::RecipeInputs {
            inference: Arc::clone(&inference),
            store: Arc::clone(&store),
            // The same `SqliteStateStore` opened at boot also impls
            // `ConvTieredReader` (spec CONV_TIERED_PORT.md).
            conv_tiered: Some(Arc::clone(&store_concrete)
                as Arc<dyn sovereign_core::conv_tiered::ConvTieredReader>),
            corpus_engine: Arc::clone(&corpus_engine),
            // Commitment persistence for the CommissiveQuery handler, and
            // `knowledge_lookup`'s notes channel — one store, both readers.
            note_store: note_store_for_runtime.clone(),
            skills: Arc::clone(&skills),
            approval: approval.clone() as Arc<dyn sovereign_core::traits::ApprovalChannel>,
            // Honour the configured response-length budget ([inference]
            // max_tokens) instead of the 2048 default — the server-side
            // equivalent of the desktop "Response length" setting. All other
            // knobs keep their core defaults.
            inference_config: sovereign_core::types::InferenceConfig {
                max_tokens: config.inference.max_tokens,
                ..sovereign_core::types::InferenceConfig::default()
            },
            indexes_dir: indexes_dir.clone(),
            embed_model: config
                .inference
                .model
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string(),
            tool_bundles,
            // No per-user tool switches on a hub: what a tenant may call is
            // decided by the narrowed catalog and the tenancy resolver, not by
            // a settings panel this process does not have.
            switches: sovereign_runtime_recipe::ToolSwitches::Ungoverned,
            // This host HAS a config file of its own, and its `[mcp]` section
            // was the only one it read until 2026-08-26.
            mcp_extra: config.mcp.servers.clone(),
            // A service must reach `listening` promptly, and on a shared hub a
            // boot that blocks on a ~1 GB parse holds up every tenant. This
            // host loaded the meta-atlas EAGERLY until 2026-08-26 — the one
            // place the desktop's 2026-06 conclusion had not reached.
            warmth: sovereign_runtime_recipe::LaneWarmth::Deferred,
            // The provider here does not own a rerank slot, so a standalone
            // cross-encoder from `SOVEREIGN_RERANK_MODEL_PATH` is the only way
            // this surface gets one. The VRAM pre-flight inside that loader
            // matters most here: a slot that does not fit is discovered by the
            // OOM killer, and on a hub that takes every tenant's in-flight
            // turn with it (note `b57b0cd5`).
            rerank: sovereign_runtime_recipe::RerankWiring::Standalone,
        },
        &sovereign_runtime_recipe::TracingProgress,
    )
    .await;
    let tools = Arc::clone(&common.parts.tools);
    let _mcp_manager = common.mcp;

    // ── Commission ───────────────────────────────────────────────────────
    // The enrichment stack above is complete, so the Runtime is built once,
    // total. This call used to sit ~140 lines higher and be mutated on the
    // way down.
    // Three slots, and only three. Everything else came back from the recipe
    // already filled, so what is written here is exactly what makes this host
    // a multi-tenant hub rather than a desktop or a daemon.
    let runtime = sovereign_runtime_recipe::commission(sovereign_core::RuntimeParts {
        routing_events: std::sync::Arc::new(narration_sink),
        // Scope corpus retrieval per tenant (multi-user hub isolation): the
        // resolver maps a `"{tenant}:{conv}"` conversation id to its owning
        // principal, and `build_context` then hides other principals' Private
        // corpora from this turn's evidence. The server is the ONLY host that
        // resolves a principal.
        corpus_principal: Some(std::sync::Arc::new(tenant::TenantPrincipalResolver)),
        landscape_digests,
        // Named absences: no compaction worker, no mesh-knowledge source (the
        // server IS the shared hub), no sensitivity oracle, no folder metadata.
        ..common.parts
    });

    // Auth state (`auth_enabled` was decided at startup, next to the
    // exposure check that depends on it).
    let auth_state = if auth_enabled {
        tracing::info!("Auth: API key ({} keys configured)", config.auth.keys.len());
        AuthState::new(config.auth.keys.clone())
    } else {
        tracing::info!("Auth: disabled");
        AuthState::disabled()
    };

    // Fair turn scheduler — bounds concurrent inference turns with a
    // weighted-fair queue + per-origin cap. Saturation surfaces as a live
    // queue position (WS) or `503 + Retry-After` (REST). Replaces the flat
    // busy semaphore and shares its policy core with the mesh peer-admission
    // gate (`commonwealth-api`), so both gates are fair by identical rules.
    let scheduler = scheduler::FairScheduler::new(
        config.server.max_concurrent_turns,
        config.server.max_per_user,
        config.server.max_queue_depth,
        config.server.retry_after_secs,
    );
    tracing::info!(
        max_concurrent_turns = config.server.max_concurrent_turns,
        max_per_user = config.server.max_per_user,
        max_queue_depth = config.server.max_queue_depth,
        reciprocity_k = config.server.reciprocity_k,
        "Fair scheduler configured"
    );
    // Reciprocity weights — a contributing peer's turns rank up. Populated
    // out-of-band from the Commonwealth ledger (`/internal/contribution/view`);
    // neutral until the first refresh, and if the mesh is absent.
    let reciprocity = reciprocity::ReciprocityTable::new();
    reciprocity::spawn_refresh(
        config.commonwealth.url.clone(),
        config.server.reciprocity_k,
        Arc::clone(&reciprocity),
    );

    // Build Axum router. The `/v1/*` API goes through the auth
    // middleware; the MCP routes do not — MCP is local-only and
    // enforced via `ConnectInfo<SocketAddr>` inside the handlers.
    //
    // NOTE for anyone putting a reverse proxy in front of this server:
    // "local-only" is decided by the peer address, so a proxy on the same
    // host satisfies it for EVERY caller. That is why `/mcp` rides the
    // `dev-routes` feature rather than an operator's proxy config.
    let authed = axum::Router::new()
        .route("/v1/conversations", post(routes::create_conversation))
        // ─ core client surface ─
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
        .merge(routes_documents::document_router());

    // Authoring surfaces — corpus path-ingest and the TDD solver. Both
    // assume the caller owns the box: the solver hands a client-supplied
    // `test_command` to `sh -c`, and the upload route ingests an absolute
    // server-side path. Compiled out by `--no-default-features`.
    #[cfg(feature = "dev-routes")]
    let authed = authed
        .merge(corpus_upload::corpus_upload_router())
        .merge(routes_tdd::tdd_router());

    let authed = authed
        .layer(middleware::from_fn(auth::auth_middleware))
        .layer(Extension(auth_state));

    // Build the TDD ChatBackend once at startup. Provider URL =
    // the server's own bind address by default — the daemon hosts
    // chat completions and the solver loop posts to them. Operators
    // who run an external provider (Anthropic, OpenAI-compat
    // backend) can override via SOVEREIGN_TDD_PROVIDER_URL until
    // the dedicated config section lands.
    #[cfg(feature = "dev-routes")]
    let tdd_state = {
        let tdd_provider_url = std::env::var("SOVEREIGN_TDD_PROVIDER_URL")
            .unwrap_or_else(|_| format!("http://{}", config.server.bind));
        let tdd_backend: Arc<dyn commonwealth_tdd::ChatBackend> = Arc::new(
            commonwealth_tdd::ReqwestChatBackend::new(format!("{tdd_provider_url}/v1")),
        );
        routes_tdd::TddState(Arc::clone(&tdd_backend))
    };

    // Bind the HTTP listener BEFORE assembling the router: the iroh
    // access path (and its `/status` surface) needs the bound port to
    // forward into.
    let bind_addr = &config.server.bind;
    tracing::info!("Listening on {bind_addr}");

    let listener = match tokio::net::TcpListener::bind(bind_addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Failed to bind to {bind_addr}: {e}");
            sovereign_inference::fast_exit_skip_destructors(1);
        }
    };
    let http_port = listener.local_addr().map(|a| a.port()).unwrap_or(8080);

    // Dial-by-key access (Track M): off unless `[iroh] enabled`.
    // Failure here never blocks the tailnet path.
    let iroh_access: Arc<Option<iroh_access::IrohAccess>> =
        Arc::new(iroh_access::IrohAccess::start(&config, http_port).await);
    let iroh_for_status = Arc::clone(&iroh_access);

    // MCP rides `dev-routes` for two reasons: it sits outside the auth
    // layer, and its localhost gate is a peer-address check that any
    // same-host reverse proxy satisfies on behalf of remote callers. It
    // also carries `TddState`, so the two are removed together.
    #[cfg(feature = "dev-routes")]
    let authed = authed
        .merge(routes_mcp::mcp_router())
        .layer(Extension(tdd_state));

    let app = authed
        // Unauthenticated liveness probe (added at the `app` level so it sits
        // OUTSIDE the `/v1/*` auth layer). A supervisor — the desktop's
        // Mobile-access toggle, the CLI, or systemd — can poll `GET /health`
        // for a 200 to know the host is up, without holding a tenant token.
        .route("/health", get(|| async { "ok" }))
        // Unauthenticated status + pairing surface. `iroh.dial` is the
        // string a phone stores as its endpoint_kind='iroh' host
        // address (public key material + relay URL — nothing secret;
        // membership still requires a tenant token).
        .route(
            "/status",
            get(move || {
                let iroh = Arc::clone(&iroh_for_status);
                async move {
                    axum::Json(serde_json::json!({
                        "status": "ok",
                        "iroh": iroh.as_ref().as_ref().map(|a| a.status_json()),
                    }))
                }
            }),
        )
        .layer(Extension(Arc::clone(&runtime)))
        // The server's OWN handles, layered alongside the Runtime rather
        // than reached through it. Same `Arc`s the Runtime was built with
        // (see `Runtime::new` above), so nothing about what a route reads
        // changes — only which object it asks. Phase 0 of daemon
        // convergence: a route that only lists conversations or describes
        // tools must not name `Runtime`.
        .layer(Extension(Arc::clone(&store)))
        .layer(Extension(Arc::clone(&tools)))
        .layer(Extension(approval))
        .layer(Extension(scheduler))
        .layer(Extension(reciprocity))
        .layer(Extension(narration_tx));

    // Browser CORS follows the auth posture ("auto"): permissive only when
    // a bearer key guards `/v1/*`, so an unauthenticated server never
    // invites cross-origin browser calls — the classic exposed-local-LLM
    // drive-by. CORS is a browser-only gate; native and mobile clients are
    // unaffected either way. Operators can pin `[server] cors`.
    let cors_permissive = match config.server.cors.as_str() {
        "permissive" => true,
        "off" => false,
        "auto" => auth_enabled,
        other => {
            tracing::warn!(value = other, "unknown [server] cors value; using \"auto\"");
            auth_enabled
        }
    };
    let app = if cors_permissive {
        app.layer(CorsLayer::permissive())
    } else {
        app
    };
    tracing::info!(cors_permissive, "CORS posture resolved");

    // Startup UX — mesh peer count from Commonwealth (non-fatal if daemon
    // isn't running).
    if let Some(ref commonwealth_url) = config.commonwealth.url {
        startup::print_mesh_status(commonwealth_url).await;
    }

    // Use `into_make_service_with_connect_info` so MCP handlers can
    // extract `ConnectInfo<SocketAddr>` to enforce localhost-only
    // access. Without this the extractor fails and every MCP request
    // is rejected.
    let service = app.into_make_service_with_connect_info::<SocketAddr>();
    if let Err(e) = axum::serve(listener, service).await {
        eprintln!("Server error: {e}");
        sovereign_inference::fast_exit_skip_destructors(1);
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
