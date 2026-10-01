// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bootstrap phases extracted verbatim from `run_daemon` so the orchestrator
//! reads as a table of contents instead of a 1,900-line scroll. Each `fn`
//! here is one self-contained startup phase; `mod.rs::run_daemon` calls them
//! in order. Behaviour-preserving — these are code moves, not rewrites.

use std::path::Path;
use std::sync::Arc;

use crate::startup::daemon_pid_path;
use crate::EmbeddedDaemon;
use corpus_engine_atlas_reader::ports::AtlasPort;
use corpus_index::ingest_port::daemon::IngestPort;
use corpus_index::types::{NodeRoster, RosterEntry};
use kernel_types::NodeId;
use sovereign_core::model_family::{
    EmbedModelInfo, ModelFamily, NormalizationStrategy, PoolingStrategy,
};
use sovereign_core::setup_config::SetupConfig;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::ToolRegistry;

/// Resolve this node's persistent id using the same precedence
/// `EmbeddedDaemon::start_daemon` applies on resume: the `node_id` file, then
/// the id baked into `mesh.json`, then generate-and-persist. Shared by the
/// work-atlas store id and the engine's `self_node_id` so a partition-of-self
/// lookup matches the daemon's own id.
pub fn resolve_self_node_id(data_dir: &Path) -> NodeId {
    sovereign_contracts::node_identity::resolve_self_node_id(data_dir)
}

/// Project the mesh's roster into the one the NoteStore uses to name note
/// authors, read through the daemon's `MembershipReader`: cw-rails' roster,
/// the one roster since the flip (phase-b-80; seat phase-b-84). svrn's own
/// `mesh.json` is not read: nothing writes it since the flip, so its names
/// would be frozen at cutover and a member who joined later would render as
/// a raw id, silently.
///
/// Returns `None` when the reader names no member (cw-rails down, or a node
/// composed with no roster reader) — attribution then degrades to raw node
/// ids under a named warn, which is honest, rather than to "assume it's us".
///
/// Ids are stored FULL (`NodeId::to_hex`, 32 chars) even though notes
/// carry the truncated `Display` form, because the truncation is lossy
/// and only the full id makes the prefix match unambiguous. Resolution
/// and ambiguity handling live in `NodeRoster::resolve`.
pub async fn build_node_roster<D: Clone + Send + Sync + 'static>(
    membership: &dyn sovereign_contracts::membership::MembershipReader<Dial = D>,
    self_node_id: NodeId,
) -> Option<NodeRoster> {
    let members = membership.members().await;
    if members.is_empty() {
        tracing::warn!(
            target = "notes",
            "notes: the mesh roster named no member (cw-rails unreachable, or no roster \
             reader composed) — note authors will render as raw node ids"
        );
        return None;
    }

    let mut self_node = None;
    let mut peers = Vec::new();
    for member in &members {
        let entry = RosterEntry {
            id_hex: member.node_id.to_hex(),
            name: member.name.clone(),
        };
        if member.node_id == self_node_id {
            self_node = Some(entry);
        } else {
            peers.push(entry);
        }
    }

    if self_node.is_none() && peers.is_empty() {
        return None;
    }
    Some(NodeRoster::new(self_node, peers))
}

/// The T1 embed and T2 GLiNER hooks code's note store is wired with when a
/// distribution composes code into this process: svrn's embed slot and its
/// loaded GLiNER session, as values (pb-notes-memory). The store is code's,
/// so code sets them ([`crate::hosted_code::CodeHost`]); `None` for GLiNER
/// leaves T2 on author-supplied symbols and files only.
pub fn notes_tier_fns(
    provider: &Arc<dyn InferenceProvider>,
    gliner_raw: &Option<Arc<dyn sovereign_contracts::ner::LabeledEntityExtractor>>,
) -> (
    corpus_index::types::EmbedFn,
    Option<corpus_index::types::GlinerFn>,
) {
    // The SAME embed slot as the engine's, adapted at the boundary so
    // `corpus-engine-notes` stays dep-free of `corpus-engine` per `ARCH §8.3`
    // (one-way edge).
    let provider_for_notes = Arc::clone(provider);
    let notes_embed: corpus_index::types::EmbedFn = Arc::new(move |text: &str| {
        let p = Arc::clone(&provider_for_notes);
        let text = text.to_string();
        Box::pin(async move {
            p.embed(&text).await.map_err(|e| {
                corpus_index::Error::Io(std::io::Error::other(format!("notes embed: {e}")))
            })
        })
    });
    let notes_gliner = gliner_raw.as_ref().map(|gliner| {
        let gliner_clone = Arc::clone(gliner);
        let f: corpus_index::types::GlinerFn = Arc::new(move |text: &str| {
            let g = Arc::clone(&gliner_clone);
            let text = text.to_string();
            Box::pin(async move {
                // `extract_mentions` is sync on both backends (Mutex-locked
                // ONNX session). Run on the blocking pool so we don't park
                // the async runtime for ~tens of ms.
                tokio::task::spawn_blocking(move || g.extract_mentions(&text))
                    .await
                    .map_err(|e| {
                        corpus_index::Error::Io(std::io::Error::other(format!(
                            "notes gliner: join error {e}"
                        )))
                    })?
                    .map(|mentions| {
                        mentions
                            .into_iter()
                            .map(|m| (m.text, m.label))
                            .collect::<Vec<_>>()
                    })
                    .map_err(|e| {
                        corpus_index::Error::Io(std::io::Error::other(format!("notes gliner: {e}")))
                    })
            })
        });
        f
    });
    (notes_embed, notes_gliner)
}

/// The `--rpc-worker` parse and its default bind are the launch contract's
/// (`sovereign_contracts::launch`). The role → RPC env translation is the
/// loader's (`sovereign_compute::distributed_role`), which the distribution
/// hands svrn through `HostedServe::env_contract` (pb-serve-distributes).
pub use sovereign_contracts::launch::rpc_worker_flag;

/// Spawn the deferred slot-alias push onto the router that ranks this node's
/// turns, through its sink; `None`: nothing ranks here, nothing to push.
pub fn spawn_slot_alias_push(
    daemon: Arc<EmbeddedDaemon>,
    sink: Option<crate::serve_client::SlotAliasSink>,
) {
    let Some(sink) = sink else {
        tracing::info!(target: "serving_path", "slot aliases: no router here, so none are pushed");
        return;
    };
    // Push slot aliases from AppState into the mesh provider once
    // the daemon's setup phase has registered model slots. Without
    // this, the mesh layer can't resolve `commonwealth/primary` →
    // local GGUF in its Local-serving branch, and the deferred
    // resolution path (routes_inference passes the alias through
    // for mesh routing) never lands on a real slot. Done on a
    // spawned task because `daemon.app_state()` only returns
    // `Some` after `start()` transitions DaemonState to Running.
    //
    // The in-flight gauge needs no install here: the bootstrap minted it
    // before the provider and handed the same handle to both, so
    // `AppState` already holds the router's counter
    // (`quality/DAEMON_CORE.md` §4.2 "Where an install slot breaks a cycle").
    let daemon_for_alias_push = Arc::clone(&daemon);
    let mesh_for_alias_push = sink;
    // Supervised one-shot: the alias push is idempotent, so a panic-restart
    // just retries the wiring (DAEMON_RESILIENCE.md P0.4).
    crate::supervise::spawn_supervised("slot_alias_push", move || {
        let daemon_for_alias_push = Arc::clone(&daemon_for_alias_push);
        let mesh_for_alias_push = mesh_for_alias_push.clone();
        async move {
            // Poll briefly for the AppState to be available. The
            // setup transition usually completes within a few
            // hundred ms; cap at 30s so a stuck setup never hangs
            // this spawn.
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
            loop {
                if let Some(state) = daemon_for_alias_push.app_state().await {
                    let snapshot = state.inner.serving.slot_aliases.current();
                    let map: std::collections::HashMap<String, String> = snapshot
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();
                    if !map.is_empty() {
                        tracing::info!(
                            count = map.len(),
                            "daemon_cmd: pushing slot aliases into mesh provider"
                        );
                        mesh_for_alias_push(map);
                        break;
                    }
                }
                if tokio::time::Instant::now() >= deadline {
                    tracing::warn!(
                        "daemon_cmd: slot-alias push timed out after 30s — \
                         mesh layer will serve aliases as plain model ids"
                    );
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }
    });
}

/// Spawn the vector-index readiness sweep over every installed corpus.
///
/// **This ran in the desktop until svt-6 (2026-09-12) and ran nowhere else.**
/// It is a self-heal of the index's own on-disk `IndexMeta.vector_index_built`:
/// `corpus_index::index::create::is_vector_index_ready` calls
/// `mark_vector_index_built` when LanceDB reports a complete index the meta
/// had not recorded. The ONE reader of that field is
/// `crate::corpus_catalog_http::catalog`, which prefers it over the
/// state store's flag — so with no sweep, a corpus whose index finished but
/// whose meta predates the field reports FTS-only forever.
///
/// It belongs here because this process owns the indexes root and serves the
/// catalogue that reads the result (ARCH principle 12). Idempotent on every
/// boot: a corpus already marked built is a read and no write.
pub fn spawn_vector_index_readiness_sweep(engine: Arc<dyn IngestPort>) {
    // Supervised one-shot: idempotent per the contract above —
    // DAEMON_RESILIENCE.md P0.4.
    crate::supervise::spawn_supervised("vector_index_readiness_sweep", move || {
        let engine = Arc::clone(&engine);
        async move {
            let Ok(indexes) = engine.installed_indexes().await else {
                tracing::debug!("bootstrap:index_readiness_sweep_skipped_unreadable_indexes_dir");
                return;
            };
            for info in indexes {
                let Ok(idx) = engine.open_index(&info.path).await else {
                    continue;
                };
                if idx.is_vector_index_ready().await {
                    tracing::info!(corpus = %info.corpus_id, "Vector index ready");
                } else {
                    // Transient, self-resolving: a corpus whose vector index
                    // is still building (common on fresh installs) is served
                    // FTS-only until the build completes. This fires once per
                    // not-ready corpus on every boot, so it is info, not a
                    // warning — nothing is broken and no user action is needed.
                    tracing::info!(
                        corpus = %info.corpus_id,
                        "Vector index not built yet — KnowledgeQuery will use \
                         FTS-only search until it finishes"
                    );
                }
            }
        }
    });
}

/// Spawn the tier-2 enrichment resume scan for unfinished workspaces after a restart.
pub fn spawn_tier2_enrichment_resume(data_dir: &Path) {
    // Tier-2 enrichment resume: find any `<...>-tier2` workspace
    // under `<data_dir>/enrichment/` whose checkpoint is incomplete
    // and re-spawn `enrich extract --resume` for each. Picks up
    // unfinished work after a daemon restart / host reboot. Safe
    // to fire on every boot — already-complete workspaces no-op,
    // and `--resume` skips chapters already in the checkpoint.
    let enrich_dir = data_dir.join("enrichment");
    let idx_dir = data_dir.join("indexes");
    // Supervised one-shot: safe on every boot per the contract above,
    // so a panic-restart just rescans (DAEMON_RESILIENCE.md P0.4).
    crate::supervise::spawn_supervised("tier2_enrichment_resume", move || {
        let enrich_dir = enrich_dir.clone();
        let idx_dir = idx_dir.clone();
        async move {
            let cli_binary =
                std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("sovereign"));
            tracing::info!(
                enrichment_dir = %enrich_dir.display(),
                "tier-2 resume: scanning for unfinished workspaces"
            );
            let outcomes = sovereign_tools::atlas_postinstall::resume_inflight_tier2(
                enrich_dir, idx_dir, cli_binary,
            )
            .await;
            for o in outcomes {
                use sovereign_tools::atlas_postinstall::Tier2LaunchOutcome;
                match o {
                    Tier2LaunchOutcome::Spawned {
                        workspace_id,
                        log_path,
                        pid,
                    } => tracing::info!(
                        workspace = %workspace_id,
                        log = %log_path.display(),
                        pid,
                        "tier-2 resume: re-spawned"
                    ),
                    Tier2LaunchOutcome::AlreadyComplete { .. } => {}
                    // Resume scan never passes peer advice — this
                    // arm is unreachable in practice but the
                    // exhaustiveness check requires us to cover it.
                    Tier2LaunchOutcome::DeferredToPeer { .. } => {}
                    Tier2LaunchOutcome::InitFailed { reason }
                    | Tier2LaunchOutcome::SpawnFailed { reason } => {
                        tracing::warn!(reason, "tier-2 resume: re-spawn failed")
                    }
                }
            }
        }
    });
}

/// Probe the embed slot and advertise this node's embed-model fingerprint to mesh
/// peers — gates whether peers route collaborative ingestion here.
pub async fn advertise_embed_model(
    provider: Arc<dyn InferenceProvider>,
    config: &SetupConfig,
    resolved_embed_family: ModelFamily,
) -> crate::EmbedAdvertisement {
    // Publish this node's embed model fingerprint so peers can filter
    // us in/out of collaborative ingestion.
    //
    // Without this wiring, `corpus_collaborate` returns 503
    // "embed model not configured on this node — cannot plan
    // collaboration" even though the embed slot is loaded and
    // working. The desktop does the same publication in
    // `sovereign-desktop/src-tauri/src/state.rs:885`; the CLI daemon
    // just didn't mirror it.
    //
    // Probe the provider for the real output dimensions rather than
    // trusting a hardcoded value — gets us the same ground truth the
    // corpus-engine uses for its dimension-mismatch guard.
    // ── A node that holds no embed slot advertises none ─────────────────
    //
    // Before the probe, because on a terminal the probe SUCCEEDS: the provider
    // forwards to the entry node, so a probe that was written to ask "is my
    // embed slot working" instead answers "is someone else's". Advertising on
    // the strength of it publishes the entry node's model as this node's own
    // capability — and `capabilities.rs`'s own doc says the
    // collaborative-ingestion planner filters candidates by exact match on this
    // field, so the terminal gets partitioned work it can only proxy straight
    // back to the machine the planner was spreading load off (§18.3).
    //
    // `Unavailable` rather than a silent skip: the type exists precisely so
    // "declines to advertise" and "probe failed" do not look alike, and a
    // terminal is the first case, permanently and by configuration.
    let Some(advertised_model_id) = config.advertised_embed_model_id() else {
        let reason = match config.node_class() {
            sovereign_core::setup_config::NodeClass::Terminal => format!(
                "terminal node: holds no embed slot of its own and forwards \
                 embeddings to its entry node ({})",
                config
                    .node
                    .binding()
                    .map(|b| b.describe())
                    .unwrap_or_else(|| "unset".to_string()),
            ),
            _ => "no embed model is configured on this node".to_string(),
        };
        tracing::info!(
            %reason,
            "embed model info: NOT advertising to mesh peers — this node holds no embed slot"
        );
        return crate::EmbedAdvertisement::Unavailable { reason };
    };

    match provider.embed("probe").await {
        Ok(probe_vec) => {
            // `model_id` = bare filename stem (e.g.
            // `qwen-embedding-0.6b`). Peers compare EmbedModelInfo
            // for exact equality, so the string has to match what
            // the desktop/other CLI daemons advertise for the same
            // GGUF. File-stem is the stable shared handle.
            // Bound by the guard above, so there is no fallback literal here
            // and no second chain to drift from it. This is a LOCAL stem by
            // construction — never the entry node's id.
            let model_id = advertised_model_id;
            // Resolve the embed family from the bundled manifest so
            // pooling + normalisation match whatever the desktop
            // path would advertise for the same GGUF. Without this,
            // CLI daemons serving Qwen3-Embedding would have
            // silently mismatched peers running the desktop build
            // (Qwen3-Embedding is Last + Server, not Mean +
            // Application) — collaborative ingestion would never
            // plan across them.
            //
            // BYOM paths that don't match any manifest row fall
            // through to `ModelFamily::Unknown` → Mean + Application
            // (safe default for generic mean-pool BERT embedders).
            // Reuse `resolved_embed_family` from provider construction
            // (above) — same manifest lookup, same answer. Keeping a
            // single source of truth prevents the slot loader and the
            // mesh advertiser drifting apart on pooling defaults.
            let embed_family = resolved_embed_family;
            let embed_quirks = embed_family.default_quirks().embed;
            let pooling = embed_quirks
                .as_ref()
                .map(|q| q.pooling)
                .unwrap_or(PoolingStrategy::Mean);
            let normalization = embed_quirks
                .as_ref()
                .map(|q| q.normalize)
                .unwrap_or(NormalizationStrategy::Application);
            // Query-side instruction prefix (OICP v0.4 §4). Part of
            // the embed bit-compat identity: Qwen3-Embedding prepends a
            // "represent this query" instruction to *query* text before
            // embedding, so a peer reconstructing a query embedding must
            // use the same prefix or land in a different space. Resolved
            // from the same manifest that drives pooling/normalisation
            // above, keeping the slot loader and mesh advertiser on one
            // source of truth.
            let query_instruction_prefix = sovereign_core::models_manifest::DEFAULT_MANIFEST
                .embed_query_instruction(&model_id);
            let embed_info = EmbedModelInfo {
                model_id: model_id.clone(),
                dimensions: probe_vec.len(),
                pooling,
                normalization,
                query_instruction_prefix,
            };
            tracing::info!(
                model_id = %embed_info.model_id,
                dims = embed_info.dimensions,
                family = ?embed_family,
                pooling = ?pooling,
                normalization = ?normalization,
                "embed model info: advertising to mesh peers"
            );
            crate::EmbedAdvertisement::Advertised(embed_info)
        }
        Err(e) => crate::EmbedAdvertisement::Unavailable {
            reason: format!("embed probe failed: {e}"),
        },
    }
}

/// Build the `/mcp` mount for the daemon's [`crate::ServingCapability`].
///
/// It used to also install the mesh, admin, reading and solve routers, the
/// provider factory and the setup config — six separate calls, each of which a
/// host could omit. The first four are now built by the daemon itself or
/// declared on the headless variant; only the tool mount is genuinely
/// host-specific, because only the host knows which tools it registered.
pub fn build_mcp_surface(
    tools: ToolRegistry,
    notes_store: Arc<sovereign_store::sqlite::SqliteStateStore>,
    code: Option<Arc<dyn host_kit::mcp::McpMountedTools>>,
) -> crate::McpSurface {
    let session_id = format!("daemon-{}", uuid::Uuid::new_v4());
    crate::McpSurface::Mounted(crate::McpMount {
        tools: Arc::new(tools),
        notes: notes_store,
        session_id,
        code,
    })
}

/// Build `POST /v1/knowledge/landscape_digest`, returned so the caller names
/// it in the daemon's variant, which is what makes "the daemon serves a
/// knowledge digest" a fact of the type rather than of whether this function
/// ran. The project-freshness Reindexer and `/v1/projects/*` built beside it
/// until pb-code-daemon-exit are the code program's.
pub async fn build_knowledge_view_http(
    data_dir: &Path,
    engine: Option<Arc<dyn IngestPort>>,
    notes: Arc<dyn sovereign_contracts::notes::AgentNotes>,
) -> axum::Router {
    // The digest reads ingest's corpora; svrn alone answers the route with
    // the named absence (pb-ingest-dial-daemon).
    let Some(engine) = engine else {
        tracing::info!("knowledge_view: no ingest program in this process; the digest is absent");
        return crate::hosted_ingest::landscape_digest_absent_router();
    };
    // Knowledge-view HTTP surface — POST /v1/knowledge/landscape_digest.
    //
    // Built read-only at this stage: the daemon holds a
    // KnowledgeViewManager so an attached desktop can fetch
    // assembled digest blocks via HTTP, but the enrichment loop
    // (observer → debouncer → atlas writes) is NOT wired here.
    // That requires the daemon to own a SQLite state store with an
    // installed observer, which is the next architectural pass.
    // Today's behaviour: the daemon serves whatever digest can be
    // built from existing on-disk skeletons. If no enrichment has
    // been run, the digest is empty — the desktop's
    // `MeshLandscapeDigestClient` treats that identically to
    // KnowledgeView=off (empty splice, no prompt impact).
    //
    // `local_only_skill_ids` is empty here; the desktop's HTTP
    // client resolves `active_is_local_only` against ITS own skill
    // registry and passes the bool in the request. See
    // `MeshLandscapeDigestClient::new` and
    // `LandscapeDigestRequest.active_is_local_only`.
    let knowledge_view_db_path = data_dir.join("sovereign.db");
    let knowledge_view_manager = Arc::new(
        sovereign_tools::knowledge_view::KnowledgeViewManager::new(
            engine.clone(),
            knowledge_view_db_path,
            Vec::new(),
        )
        .await,
    );
    // svrn's memory notes, where the commitments its relational digest
    // annotates live (pb-notes-memory).
    knowledge_view_manager.install_notes(notes).await;
    crate::landscape_digest_http::landscape_digest_router(Arc::clone(&knowledge_view_manager))
}

/// Build the watched-folder reconciliation subsystem (LocalCorpusManager +
/// enrichment defaults + tiered deps) and spawn its scheduler; returns the held
/// subsystem handle.
pub async fn setup_watched_folders(
    ingest: Option<(Arc<dyn IngestPort>, Arc<dyn AtlasPort>)>,
    state_store: Arc<dyn sovereign_core::traits::StateStore>,
    data_dir: &Path,
    config: &SetupConfig,
    folder_tiered_deps: Option<sovereign_tools::local_corpus::watched::enrich::TieredDeps>,
    enrich_config: Option<Arc<dyn corpus_index::ingest_port::enrich_config::EnrichConfigPort>>,
) -> Option<crate::watched_folder_setup::WatchedSubsystem> {
    // ── Watched-folder reconciliation scheduler ─────────────────
    //
    // Constructs the LocalCorpusManager + per-corpus registry,
    // re-populates the registry from the persisted corpora list
    // (auto-resume on daemon restart), then spawns the dispatcher
    // loop. The scheduler walks each registered watched-folder
    // corpus on its configured cadence (default 120 s, floored at
    // 60 s) and applies the diff through CorpusUpdater.
    //
    // The local-corpus subsystem touches the store on `remove`
    // (delete_corpus_state). It was handed a fresh `InMemoryStateStore`
    // until daemon-convergence Phase 3, on the reasoning that the
    // persistent source of truth for corpus metadata is
    // `{data_dir}/local-corpora/*.json` — true, and it made
    // `delete_corpus_state` a no-op against an empty map, so removing a
    // watched folder left its state rows behind in the real db forever.
    // The daemon now has ONE state store (§10.6, one decider one name) and
    // this is it, so the delete lands where the rows actually are.
    // Watched-folder reconciliation subsystem. The full wiring (build
    // registry → resume corpora → install runtime singleton → mount
    // HTTP routes → spawn scheduler) is factored into
    // `crate::watched_folder_setup` so the desktop's
    // embedded daemon can call the same path.
    // Critical: pass the same `recipes_dir` ingest's engine was
    // constructed with (`<data_dir>/recipes`, corpus-engine's
    // `face::compose`). Otherwise the manager
    // writes its generated recipe TOMLs into a directory the
    // engine never reads from, and the first sweep's apply step
    // errors `No registry entry for corpus '<id>'`.
    // Watched folders ingest through ingest's port; svrn alone installs no
    // manager, and the routes answer 503 naming why (pb-ingest-dial-daemon).
    let Some((engine, atlas)) = ingest else {
        tracing::info!(
            "watched_folder: no ingest program in this process; watched folders are not served"
        );
        return None;
    };
    let lc_recipes_dir = data_dir.join("recipes");
    match sovereign_tools::local_corpus::LocalCorpusManager::init_with_recipes_dir(
        engine.clone(),
        state_store,
        None,
        data_dir.to_path_buf(),
        data_dir.join("vault-snapshots"),
        lc_recipes_dir,
    )
    .await
    {
        Ok(manager) => {
            // Folder-ingest v1 §3.3 — install enrichment
            // defaults so the watched-folder driver can
            // synthesise an EnrichConfig for "Enable
            // enrichment" requests. Pull model ids from the
            // daemon's resolved chat / embed slots; on a
            // fresh setup with no models picked, fall back
            // to empty strings so the driver returns a clear
            // "defaults not installed" error to the UI.
            // Empty on a terminal, which the `!is_empty()` guard below
            // already treats as "don't wire enrichment defaults".
            let chat_model = config.primary_model_stem().unwrap_or_default();
            let embed_model = config.embed_model_stem().unwrap_or_default();
            if !chat_model.is_empty() && !embed_model.is_empty() {
                let base_url = format!("http://127.0.0.1:{}", config.daemon.client_port);
                manager
                    .set_enrichment_defaults(
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
                    "watched_folder:enrichment_defaults_skipped — \
                         chat_model or embed_model not configured; \
                         per-folder enrichment will return an error \
                         until models are picked"
                );
            }
            // Ingest's enrichment-config port, when the distribution composed
            // ingest; without it the config sites report ingest absent.
            if let Some(port) = enrich_config {
                manager.set_enrich_config(port).await;
            }
            if let Some(deps) = folder_tiered_deps.clone() {
                manager.set_tiered_deps(deps).await;
                tracing::info!(
                    "watched_folder:tiered_deps_installed — \
                         enable_enrichment will route through the \
                         in-process tiered driver"
                );
            }
            // ontology-v1 P0.4 installed an in-process atlas orchestrator
            // here; fp-19 (five-programs) dials it away — the driver spawns
            // the ingest program's `sovereign enrich build` and reports the
            // named absence when the CLI is not found (FIVE_PROGRAMS §12 D2).
            tracing::info!(
                "watched_folder:atlas_build_dialed — \
                     atlas builds spawn the ingest program's `sovereign enrich build`"
            );
            // Headless OCR. `corpus watch --ocr` sets `with_ocr: true` on
            // the folder config, but the sweep only takes the OCR path when
            // an `OcrCtx` is also installed here — otherwise scanned PDFs
            // are reported as `scanned_no_text` and never indexed
            // (`watched/worker.rs`). Until now only the desktop installed
            // one, so a server could enable OCR and get nothing.
            //
            // `cleanup_model` is the same chat-slot file stem the
            // enrichment defaults use: the daemon registers each loaded
            // slot under its file stem, so a slot ALIAS like "fast" would
            // 503 and silently degrade every page to raw OCR text.
            super::ocr_install::install_ocr_ctx(
                &manager,
                data_dir,
                format!("http://127.0.0.1:{}", config.daemon.client_port),
                config.primary_model_stem().unwrap_or_default(),
            )
            .await;
            // Living trigger: workflows attached to a watched folder
            // (`run_on_changes`) run on the daemon when a sweep changes files.
            // Routed back through the daemon's own loopback so `model:`/`embed:`
            // steps use the already-loaded slots.
            let trigger_runtime: Option<
                Arc<dyn sovereign_tools::local_corpus::watched::workflow_trigger::WorkflowTriggerRuntime>,
            > = Some(Arc::new(
                super::workflow_trigger::DaemonWorkflowRuntime::new(
                    format!("http://127.0.0.1:{}", config.daemon.client_port),
                    atlas,
                ),
            ));
            Some(
                crate::watched_folder_setup::WatchedSubsystem::install(
                    engine.clone(),
                    Arc::new(manager),
                    config.watched_folders.max_concurrent_sweeps,
                    trigger_runtime,
                )
                .await,
            )
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "watched_folder:manager_init_failed — scheduler not spawned"
            );
            None
        }
    }
}

/// Install the AppState foreground-yield hook on the code program's lint/test
/// watchers, through its setter, so their cargo subprocesses back off under
/// chat-slot memory pressure. `None`: no code program in this process.
pub fn install_foreground_yield_hook(
    daemon: Arc<EmbeddedDaemon>,
    yield_to: Option<Box<dyn Fn(Arc<dyn corpus_engine_yield::YieldHook>) + Send + Sync>>,
) {
    // ── Foreground back-pressure for lint/test watchers ─────────────
    //
    // The lint and test runners burst memory (workspace cargo check
    // ≈ 2-4 GB peak; 22-crate `cargo test` higher) and historically
    // ran without coordination with the chat slot. Combined with a
    // 35B chat slot ≈ 30 GB resident, that crosses jetsam threshold
    // on memory-tight boxes and SIGTERMs the daemon mid-request.
    //
    // Install `AppStateYieldHook` on each watcher so its subprocess
    // runner waits until `should_yield()` returns false before
    // spawning cargo. Late-bind: the code program's watchers are built
    // before EmbeddedDaemon exists; `daemon.app_state()` returns Some only
    // after start_daemon completes. Poll with a deadline.
    let Some(yield_to) = yield_to else {
        tracing::debug!("foreground-yield: no code program here, so no watcher to hook");
        return;
    };
    {
        let daemon_for_hook = Arc::clone(&daemon);
        tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
            loop {
                if let Some(hook) = daemon_for_hook.build_yield_hook().await {
                    yield_to(hook);
                    return;
                }
                if tokio::time::Instant::now() >= deadline {
                    tracing::warn!(
                        "foreground-yield: watcher hook wire-up timed out \
                         — lint/test will not yield to chat (memory \
                         contention possible)"
                    );
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        });
    }
}

/// Write the daemon pidfile (so `daemon stop` can find us) and return its path +
/// our pid for the shutdown path.
pub fn write_pidfile() -> (std::path::PathBuf, u32) {
    // ── Pidfile ───────────────────────────────────────────────────
    //
    // `svrn daemon stop` keys off `~/.svrnmesh/daemon.pid` to
    // know which process to SIGTERM. Previously only `daemon start`
    // (the detached-child launcher) wrote that file, so any other
    // launch path — `svrn daemon run` from a shell, `cargo run
    // -- daemon run`, systemd's `ExecStart` — left no pidfile and
    // `stop` silently fell back to `systemctl/launchctl stop`, which
    // is a no-op for daemons launched outside the service manager.
    //
    // Writing the pidfile here from `run_daemon` itself makes the
    // file an accurate property of "a daemon is running" rather than
    // "the daemon was launched via `start`". The bind has already
    // succeeded above, so any pre-existing pidfile is stale and can
    // be overwritten safely (the live owner of :9741 is us).
    let pid_path = daemon_pid_path();
    if let Some(parent) = pid_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let self_pid = std::process::id();
    if let Err(e) = std::fs::write(&pid_path, format!("{self_pid}\n")) {
        tracing::warn!(
            path = %pid_path.display(),
            error = %e,
            "could not write daemon pidfile — `daemon stop` will need lsof/launchctl fallback"
        );
    }
    (pid_path, self_pid)
}

// Moved to a sibling file: inline, these put this file past its arch-gate
// slack (ARCH §3.1). `#[path]`, so the names are unchanged.
#[cfg(test)]
#[path = "tests/bootstrap_advertise.rs"]
mod advertise_tests;

#[cfg(test)]
#[path = "tests/bootstrap_roster.rs"]
mod roster_tests;
