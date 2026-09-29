// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bootstrap phases extracted verbatim from `run_daemon` so the orchestrator
//! reads as a table of contents instead of a 1,900-line scroll. Each `fn`
//! here is one self-contained startup phase; `mod.rs::run_daemon` calls them
//! in order. Behaviour-preserving — these are code moves, not rewrites.

use std::path::Path;
use std::sync::Arc;

use crate::startup::daemon_pid_path;
use crate::EmbeddedDaemon;
use corpus_engine::CorpusEngine;
use corpus_index::types::{EmbedFn, NodeRoster, RosterEntry};
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
    sovereign_mesh::persist::resolve_self_node_id(data_dir)
}

/// Project the persisted mesh into the roster the NoteStore uses to name
/// note authors.
///
/// Returns `None` when there is no mesh (solo node) or `mesh.json` can't
/// be read — attribution then degrades to raw node ids, which is honest,
/// rather than to "assume it's us".
///
/// Ids are stored FULL (`NodeId::to_hex`, 32 chars) even though notes
/// carry the truncated `Display` form, because the truncation is lossy
/// and only the full id makes the prefix match unambiguous. Resolution
/// and ambiguity handling live in `NodeRoster::resolve`.
pub fn build_node_roster(data_dir: &Path, self_node_id: NodeId) -> Option<NodeRoster> {
    let mesh = match sovereign_mesh::persist::load(data_dir) {
        Ok(Some(m)) => m,
        Ok(None) => return None,
        Err(e) => {
            tracing::warn!(
                target = "notes",
                error = %e,
                "notes: mesh.json unreadable — note authors will render as raw node ids"
            );
            return None;
        }
    };

    let mut self_node = None;
    let mut peers = Vec::new();
    for member in &mesh.members {
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

/// The process's NER handle and the per-chunk adapter over it. The handle is
/// the NER served kind's (`sovereign_compute::ner::served_ner`, loaded once per
/// process); it feeds the NoteStore T2 `GlinerFn` adapter. The adapter
/// (corpus-engine's `GlinerChunkExtractor`) feeds the engine's tiered runner
/// and the folder driver. Both `None` when the model isn't installed — tiered
/// ingest then falls back to RAPTOR-derived entities.
///
/// Generation is chosen inside the kind's loader
/// (`sovereign_gliner::configured_model_id`); nothing on this side of the
/// call knows or needs to know which backend it got.
pub fn load_gliner_extractor(
    store: Arc<dyn sovereign_core::daemon_wire::conv_tiered::ChunkEntityStore>,
) -> (
    Option<Arc<dyn sovereign_contracts::ner::LabeledEntityExtractor>>,
    Option<Arc<dyn corpus_index::ingest_port::tiered::ChunkEntityExtractor>>,
) {
    // The store is opened once by `run_daemon` and passed in, so the adapter
    // opens no second handle.
    let ner = sovereign_compute::ner::served_ner();
    let chunk = ner.as_ref().map(|extractor| {
        corpus_engine::enrichment::chunk_ner::GlinerChunkExtractor::new(
            store,
            Arc::clone(extractor),
        )
        .into_handle()
    });
    (ner, chunk)
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

/// Build the single shared `CorpusEngine` (powers `/mcp` tools AND
/// `corpus_collaborate` ingest). Wires a REAL embed slot through `provider`
/// (a zero-vector stub here once poisoned 4M chunks — see inline note), the
/// batch variant, the conv-tiered provider, and the shared GLiNER chunk
/// extractor.
pub fn build_corpus_engine(
    data_dir: &Path,
    provider: Arc<dyn InferenceProvider>,
    config: &SetupConfig,
    self_node_id: NodeId,
    chunk_entity_extractor: &Option<
        Arc<dyn corpus_index::ingest_port::tiered::ChunkEntityExtractor>,
    >,
) -> (Arc<CorpusEngine>, String) {
    // Returned alongside the engine rather than re-derived by the caller. The
    // `config.models.embed.file_stem()` expression below already had five
    // copies tree-wide (ARCH §10.6); the daemon's `Runtime` needs the same
    // string to key its atlas embedding cache, and a sixth copy is how the
    // cache and the shards start disagreeing about which model wrote them.
    let mut derived_embed_model = String::new();
    let engine: Arc<CorpusEngine> = {
        let indexes_dir = data_dir.join("indexes");
        let provider_for_embed = Arc::clone(&provider);
        let embed: EmbedFn = Arc::new(move |text: &str| {
            let p = Arc::clone(&provider_for_embed);
            let text = text.to_string();
            Box::pin(async move {
                p.embed(&text)
                    .await
                    .map_err(|e| corpus_index::Error::Embed(e.to_string()))
            })
        });
        let provider_for_batch = Arc::clone(&provider);
        let batch_embed: corpus_index::types::BatchEmbedFn = Arc::new(move |texts: &[String]| {
            let p = Arc::clone(&provider_for_batch);
            let texts = texts.to_vec();
            Box::pin(async move {
                p.embed_batch(&texts)
                    .await
                    .map_err(|e| corpus_index::Error::Embed(e.to_string()))
            })
        });

        // Derive the embed model identifier from the configured GGUF
        // path so `_corpus_meta.json` records the actual model rather
        // than failing the ingest pre-flight ("embedding model name not
        // configured"). Matches the wiring in `state.rs:717-723` and
        // every other call site (`main.rs:506`, `chat_cmd/bootstrap.rs`,
        // `code_cmd.rs`, `project_cmd.rs`); the standalone daemon was
        // the lone holdout, which is why the desktop's
        // `/internal/corpus/install` POST hits this engine and bombs at
        // the pre-flight before the first byte is downloaded.
        let embed_model_name = config
            .local_embed_model_id()
            .unwrap_or_else(|| "unknown-embed-model".to_string());
        // recipes_dir doubles as the registry's overrides_dir. Locally-
        // published recipes from `svrn recipe publish` land at
        // `~/.svrnmesh/recipes/<id>/recipe.toml` and only resolve when
        // the engine's overrides_dir points there. Earlier this passed
        // `indexes_dir` for the recipes argument, which made every
        // `corpus install` skip the local override and try the public
        // registry URL — the wikipedia-catalog dev variant could never
        // be installed because its data URL is not yet hosted.
        let recipes_dir = data_dir.join("recipes");
        // Recipe enrichment (`[enrichment] enabled = true, type = "atlas"`)
        // requires an InferenceFn — without one, `engine.ingest` logs
        // "no InferenceFn was provided to CorpusEngine — skipping" and
        // silently degrades to chunks-only ingest. The embedded daemon
        // was the lone holdout (every other call site —
        // `sovereign-server/src/main.rs:224`,
        // `sovereign-desktop/src-tauri/src/state.rs:1053`,
        // `sovereign-cli/src/main.rs:865`,
        // `chat_cmd/bootstrap.rs:242` — wires this); surface symptom
        // was conversations-personal landing 180 embedded chunks with
        // no atlas/atoms.json. Same provider already drives embed +
        // batch_embed above.
        let inference_fn = corpus_engine::enrichment::provider_inference::inference_to_inference_fn(
            Arc::clone(&provider),
        );
        // Conv-tiered enrichment provider — spec
        // `sovereign/docs/specs/CONV_TIERED_PORT.md`. Constructed by the
        // shared builder (same `FolderTieredProvider` the desktop's embedded
        // daemon wires) so both stay in lockstep. Failing to open the store
        // is non-fatal: the tiered runner falls back to dispatch-plan-only
        // mode when no provider is injected. `FolderTieredProvider` is the
        // sole provider — its `finalize_corpus` override runs the
        // vault-wide synthesis pass needed for `vault_themes`; see the
        // builder's docs.
        let tiered_provider = sovereign_tools::enrichment_bootstrap::build_folder_tiered_provider(
            data_dir,
            Arc::clone(&provider),
            Arc::new(corpus_engine::IngestAtlas),
        );
        // GliNER per-chunk entity extractor loaded once in the outer scope
        // (above) so the engine and the folder driver share the same
        // Arc<dyn> handle. Clone here to reuse.
        let chunk_entity_extractor_for_engine = chunk_entity_extractor.clone();

        let mut engine_builder = CorpusEngine::new(recipes_dir, indexes_dir, embed)
            .with_embedding_model(&embed_model_name)
            .with_batch_embed_fn(batch_embed)
            .with_inference_fn(inference_fn)
            .with_self_node_id(self_node_id.to_string());
        if let Some(provider) = tiered_provider {
            engine_builder = engine_builder.with_tiered_provider(provider);
        }
        if let Some(extractor) = chunk_entity_extractor_for_engine {
            engine_builder = engine_builder.with_chunk_entity_extractor(extractor);
        }
        // The `sec_edgar` custom acquirer (ticker -> installed SEC
        // filings corpus) lives in `sovereign-tools` so `corpus-engine`
        // stays free of SEC domain knowledge. Registered HERE, on the
        // engine itself, rather than piggybacked on
        // `KnowledgeViewManager::new`: that runs after
        // `install_http_and_mcp` has already mounted the install route,
        // so a fast install could reach `acquire_source` before the
        // acquirer exists — and it sits behind the
        // `knowledge_view.enabled` gate, which has nothing to say about
        // SEC filings. Registration is cheap and unconditional; a
        // recipe that never names the kind never invokes it.
        sovereign_tools::sec_edgar::register(&engine_builder);
        derived_embed_model = embed_model_name.clone();
        Arc::new(engine_builder)
    };
    (engine, derived_embed_model)
}

/// Build the watched-folder tiered-enrichment deps (its own
/// `FolderTieredProvider` over the shared `sovereign.db`). Independent of the
/// engine-side conv provider; `None` (legacy-subprocess fallback) when the
/// state store can't be opened. Consumes the GLiNER extractor handle.
pub fn build_folder_tiered_deps(
    data_dir: &Path,
    provider: Arc<dyn InferenceProvider>,
    chunk_entity_extractor: Option<
        Arc<dyn corpus_index::ingest_port::tiered::ChunkEntityExtractor>,
    >,
) -> Option<sovereign_tools::local_corpus::watched::enrich::TieredDeps> {
    // The provider comes from the shared builder so the desktop's embedded
    // daemon wires an identical one (`sovereign_tools::enrichment_bootstrap`);
    // the engine's `FolderTiered` over it is built here, where the engine is
    // (phase-b-49).
    let tiered_provider = sovereign_tools::enrichment_bootstrap::build_folder_tiered_provider(
        data_dir,
        provider,
        Arc::new(corpus_engine::IngestAtlas),
    )?;
    tracing::info!(
        target: "sovereign_tools::enrichment_bootstrap",
        "enrichment_bootstrap: folder tiered deps constructed — FolderTieredProvider wired"
    );
    Some(sovereign_tools::local_corpus::watched::enrich::TieredDeps {
        tiered: Arc::new(corpus_engine::FolderTiered::new(
            tiered_provider,
            chunk_entity_extractor,
        )),
    })
}

/// Default bind for an anchor's in-process RPC worker. Applied only when a
/// `[shared_model]` role asks to serve but no explicit `SOVEREIGN_RPC_SERVE`
/// is set.
///
/// LOOPBACK, not `0.0.0.0`. The ggml rpc-server authenticates nothing and
/// encrypts nothing, so a node that only set `role = "anchor"` used to offer
/// its GPU and the tensors crossing it to every host on its network. Members
/// reach this worker through the encrypted mesh tunnel instead — the
/// `RPC_ALPN` splice in `sovereign_mesh::iroh_access`, which admits members
/// only — and that path needs no LAN bind. An operator who genuinely wants the
/// plaintext LAN bind sets it and acknowledges it; see
/// [`sovereign_contracts::launch::RpcServe`].
const DEFAULT_RPC_BIND: &str = "127.0.0.1:50052";

/// `--rpc-worker[=<bind>]` → the address this node should serve its GPU on.
///
/// Lending a GPU to the mesh was previously reachable only by editing
/// `[shared_model] role` in a TOML file or by knowing the name of an
/// undocumented environment variable. Both work; neither is something an
/// operator can discover from `--help`, and "turn this box into a worker" is a
/// one-line intention that deserves a one-line spelling.
///
/// Accepts `--rpc-worker`, `--rpc-worker=<bind>` and `--rpc-worker <bind>`. A
/// following token is taken as the bind only when it is not itself a flag, so
/// `--rpc-worker --setup-only` means the default bind, not a bind of
/// `--setup-only`.
///
/// This is deliberately NOT the same lever as `role = "anchor"`. The role also
/// turns on peer *discovery* (`SOVEREIGN_RPC_DISCOVER`) and enters this node in
/// the host election; this flag only offers the GPU. On a node whose daemon
/// predates the 2026-07-29 containment fix that distinction matters, because
/// the discovery flag is what the boot gate reads back — see
/// [`crate::build::containment`].
pub fn rpc_worker_flag(args: &[String]) -> Option<String> {
    let mut it = args.iter().enumerate();
    let (i, a) =
        it.find(|(_, a)| a.as_str() == "--rpc-worker" || a.starts_with("--rpc-worker="))?;
    if let Some(bind) = a.strip_prefix("--rpc-worker=") {
        let bind = bind.trim();
        // `--rpc-worker=` with nothing after it is a typo, not a request to
        // serve on the empty string (which `serve_rpc_worker_if_configured`
        // would silently ignore, leaving the operator with no worker and no
        // explanation).
        return Some(if bind.is_empty() {
            DEFAULT_RPC_BIND.to_string()
        } else {
            bind.to_string()
        });
    }
    let next = args
        .get(i + 1)
        .map(String::as_str)
        .filter(|n| !n.starts_with('-') && !n.is_empty());
    Some(next.unwrap_or(DEFAULT_RPC_BIND).to_string())
}

/// Apply `--rpc-worker` to the env contract the RPC consumers read.
///
/// Runs BEFORE [`apply_shared_model_role_to_env`], which only fills
/// `SOVEREIGN_RPC_SERVE` in when it is unset — so an explicit flag beats the
/// configured role, matching how an explicit env var already beats both.
pub fn apply_rpc_worker_flag(args: &[String]) {
    let Some(bind) = rpc_worker_flag(args) else {
        return;
    };
    std::env::set_var("SOVEREIGN_RPC_SERVE", &bind);
    tracing::info!(bind = %bind, "--rpc-worker → SOVEREIGN_RPC_SERVE");
}

/// Translate `[shared_model] role` into the RPC env contract that the
/// three decoupled RPC consumers already read — the inference serve
/// (`serve_rpc_worker_if_configured`), this module's discovery loop, and
/// commonwealth-api's `/status` advertise. This is the desktop-friendly
/// source of the role (the app writes the config; no hand-set env vars);
/// an explicitly-set env var always wins, so CLI/power users are
/// unaffected. One traceable place where role → RPC wiring happens.
///
/// - `Host`   → discover peers' workers AND serve (a host also anchors).
/// - `Anchor` → serve this node's GPU into the layer-split.
/// - `Consumer` (default) → neither; the node only queries the shared
///   model (that routing is wired separately, not via these env vars).
pub fn apply_shared_model_role_to_env(cfg: &sovereign_core::setup_config::SharedModelSection) {
    use sovereign_core::setup_config::SharedModelRole;
    // The shared model this node routes its primary turns into (any role that
    // names one — consumers query it, anchors/host also serve it). Read by the
    // mesh inference provider via SOVEREIGN_SHARED_MODEL_ID.
    if let Some(id) = cfg.model_id.as_deref() {
        if !id.is_empty() && std::env::var_os("SOVEREIGN_SHARED_MODEL_ID").is_none() {
            std::env::set_var("SOVEREIGN_SHARED_MODEL_ID", id);
            tracing::info!(
                model_id = id,
                "shared-model: primary routes to SOVEREIGN_SHARED_MODEL_ID"
            );
        }
    }
    let serve = matches!(cfg.role, SharedModelRole::Anchor | SharedModelRole::Host);
    // Host failover: EVERY anchor spawns the discovery loop, not just the
    // statically-designated host — so any anchor can take over the host role the
    // instant it is elected leader (`partition::should_host`). The loop stays
    // dormant (it discovers + keeps worker eligibility warm but does NOT
    // distribute) until this node is the host. So "discover" now means
    // "participates in the host election", which every anchor does.
    let discover = serve;
    // The plaintext-LAN acknowledgement, carried into the env contract BEFORE
    // any bind is resolved — `RpcServe` is the one decider and it reads the
    // env half, so a config that acknowledges must be visible to it whether
    // the bind came from the role below or from an explicit variable. Env wins
    // if pre-set, matching every other key in this section.
    if cfg.allow_plaintext_lan
        && std::env::var_os(sovereign_contracts::launch::RPC_ALLOW_PLAINTEXT_LAN_ENV).is_none()
    {
        std::env::set_var(
            sovereign_contracts::launch::RPC_ALLOW_PLAINTEXT_LAN_ENV,
            "1",
        );
        tracing::warn!(
            "shared-model: `[shared_model] allow_plaintext_lan = true` — a non-loopback \
             RPC bind will be accepted; the ggml worker authenticates nothing"
        );
    }
    if serve && std::env::var_os("SOVEREIGN_RPC_SERVE").is_none() {
        std::env::set_var("SOVEREIGN_RPC_SERVE", DEFAULT_RPC_BIND);
        tracing::info!(
            role = ?cfg.role,
            bind = DEFAULT_RPC_BIND,
            "shared-model: anchor role → SOVEREIGN_RPC_SERVE"
        );
    }
    // Anchor-tier worker eligibility: a host treats its fellow anchors with the
    // stricter `EligibilityConfig::anchor` profile (slower settle, quarantine on
    // first flap), since a flapping anchor can GGML_ABORT the host mid-decode.
    // Set the env knobs the eligibility gate reads; an explicit env always wins.
    // Applied for any serving role so a future failover host (an anchor that
    // becomes leader) already carries the right profile.
    if serve {
        if std::env::var_os("SOVEREIGN_RPC_WORKER_SETTLE_SECS").is_none() {
            std::env::set_var(
                "SOVEREIGN_RPC_WORKER_SETTLE_SECS",
                sovereign_mesh::worker_eligibility::ANCHOR_SETTLE_SECS.to_string(),
            );
        }
        if std::env::var_os("SOVEREIGN_RPC_WORKER_FLAP_THRESHOLD").is_none() {
            std::env::set_var(
                "SOVEREIGN_RPC_WORKER_FLAP_THRESHOLD",
                sovereign_mesh::worker_eligibility::ANCHOR_FLAP_THRESHOLD.to_string(),
            );
        }
    }
    if discover && std::env::var_os("SOVEREIGN_RPC_DISCOVER").is_none() {
        std::env::set_var("SOVEREIGN_RPC_DISCOVER", "1");
        tracing::info!(role = ?cfg.role, "shared-model: anchor spawns the host-election discovery loop");
    }
    // The operator's optional designated-host pin. Published so every anchor's
    // `should_host` check honours it while it's an eligible anchor, and fails
    // over to election (min NodeId) when it drops out.
    if let Some(pin) = cfg.host_node_id.as_deref() {
        if !pin.is_empty() && std::env::var_os("SOVEREIGN_SHARED_MODEL_HOST_NODE_ID").is_none() {
            std::env::set_var("SOVEREIGN_SHARED_MODEL_HOST_NODE_ID", pin);
            tracing::info!(pin, "shared-model: designated-host pin published");
        }
    }
    // The host enforces the quorum + pooled-memory gate before distributing, so it
    // carries those knobs into the RPC env contract too (env wins if already set).
    if discover {
        if std::env::var_os("SOVEREIGN_RPC_QUORUM_ANCHORS").is_none() {
            std::env::set_var(
                "SOVEREIGN_RPC_QUORUM_ANCHORS",
                cfg.quorum_anchors.to_string(),
            );
        }
        if let Some(gb) = cfg.min_pooled_gb {
            if std::env::var_os("SOVEREIGN_RPC_MIN_POOLED_GB").is_none() {
                std::env::set_var("SOVEREIGN_RPC_MIN_POOLED_GB", gb.to_string());
            }
        }
        if let Some(h) = cfg.headroom {
            if std::env::var_os("SOVEREIGN_RPC_HEADROOM").is_none() {
                std::env::set_var("SOVEREIGN_RPC_HEADROOM", h.to_string());
            }
        }
        // Shard-fetch mode (host-side orchestrator reads this). The fleet
        // default is `ranges` — each anchor pulls only its slice, the only way
        // a model bigger than one node's disk distributes. Set for any serving
        // role so a failover host already carries it. Env wins if pre-set.
        if std::env::var_os("SOVEREIGN_RPC_SHARD_FETCH").is_none() {
            std::env::set_var("SOVEREIGN_RPC_SHARD_FETCH", cfg.shard_fetch.as_env());
        }
    }
}

/// THE reader of `SOVEREIGN_RPC_DISCOVER` (TOPOLOGY §10 phase 10, ARCH §10.6).
///
/// A PRESENCE check — any value, including empty, arms discovery. That is the
/// established semantics and it is preserved here rather than tightened;
/// changing what counts as "set" is a behaviour change and this rung is about
/// having one answer, not a new one.
///
/// Three sites asked independently (`bootstrap`, `build/containment`,
/// `doctor_cmd`), and two of them feed a containment VERDICT — so a divergence
/// would mean the doctor reporting a containment posture the daemon does not
/// actually run under.
///
/// Lives in `crate::startup` rather than here (moved 2026-09-17) because this
/// module is gated on `treesitter` and `build/containment` is not; the env read
/// has no treesitter dependency.
pub use crate::startup::rpc_discovery_armed;

/// This daemon's mesh, as the discovery loop and the warm orchestrator read it
/// (compute's `distributed_discovery` and `distributed_warm`,
/// pb-serve-distributes): the roster, transport and identity of a Running
/// daemon, `None` otherwise; the host role published for `/v1/mesh/status`;
/// the discovery memory the warm orchestrator resolves endpoints through;
/// where this daemon serves model files (its internal port, and the reachable
/// bases on it); and its
/// mesh proof.
pub fn mesh_ports(
    daemon: &Arc<EmbeddedDaemon>,
) -> sovereign_serving_host::rpc_discovery::MeshPorts {
    use sovereign_serving_host::rpc_discovery::{MeshNow, MeshPorts, ModelOrigin};
    let reader = Arc::clone(daemon);
    let origin = Arc::clone(daemon);
    let prover = Arc::clone(daemon);
    MeshPorts {
        model_origin: Arc::new(move || {
            let daemon = Arc::clone(&origin);
            Box::pin(async move {
                let (_client_port, internal_port) = daemon.resolved_ports().await;
                ModelOrigin {
                    internal_port,
                    bases: sovereign_mesh::mesh_discovery::reachable_addresses(internal_port)
                        .into_iter()
                        .map(|a| format!("http://{a}"))
                        .collect(),
                }
            })
        }),
        proof: Arc::new(move || {
            let daemon = Arc::clone(&prover);
            Box::pin(async move {
                let stamp = daemon.app_state().await?.mesh_proof_stamp().await?;
                let (name, value) = stamp.pair();
                Some((name, value.to_string()))
            })
        }),
        mesh: Arc::new(move || {
            let daemon = Arc::clone(&reader);
            Box::pin(async move {
                let app = daemon.app_state().await?;
                Some(MeshNow {
                    roster: Arc::clone(&app.inner.fabric.membership),
                    transport: app.peer_transport(),
                    self_id: app.inner.fabric.identity.current(),
                })
            })
        }),
        on_host_role: Arc::new(crate::mesh_http::set_shared_model_host),
        discovery: daemon.rpc_discovery(),
    }
}

/// How often the refresher re-derives the manifest to check that no transition
/// was missed. Slow enough to be free, fast enough that a missed event is a
/// minute of wrongness rather than an outage.
const MANIFEST_RECONCILE: std::time::Duration = std::time::Duration::from_secs(60);

/// Keep the mesh self-manifest in step with the distributed primary's lifecycle.
///
/// `build_self_manifest` is a SNAPSHOT of the local provider, taken once when
/// the [`InferenceRouter`](sovereign_serving_host::peer_inference::InferenceRouter)
/// is built. At that moment the distributed
/// slot has never spawned (`DynamicChildSlot::new` deliberately does not spawn),
/// so `is_serving()` is false, the Slow tier answers with the small FAST model,
/// and the heavyweight primary is absent from the manifest entirely. Minutes
/// later the discovery tick warms the workers and respawns the child into
/// Serving — and nothing rebuilds the snapshot. `locate_named_model` then
/// returns Unknown and every request that NAMES the shared model 503s from a
/// perfectly healthy cluster, which defeats the point of sharing a model on a
/// mesh. Observed live 2026-07-28 (note c5678d34); the same failure shape as
/// 2026-05-20, which was fixed only for the hot-load path.
///
/// Driven by the lifecycle watch rather than a timer, because the requirement is
/// symmetry, not freshness: advertise exactly what we can serve. A RETIRED or
/// Failed child must stop being advertised as promptly as a Serving one starts,
/// or peers route into a guaranteed `ComputeUnavailable` — and `retire()` runs
/// on every empty-worker tick and every warm refusal, so that window is not
/// hypothetical.
pub fn spawn_self_manifest_refresh(
    mesh_provider: Arc<sovereign_serving_host::peer_inference::InferenceRouter>,
    distributed_slot: Option<Arc<sovereign_compute::manager::DynamicChildSlot>>,
) {
    let Some(slot) = distributed_slot else {
        tracing::debug!(
            target: "compute_child",
            "self-manifest refresh: no distributed-primary slot — the manifest has no \
             lifecycle-gated rows to track"
        );
        return;
    };
    crate::supervise::spawn_supervised("self_manifest_refresh", move || {
        let mesh = Arc::clone(&mesh_provider);
        let slot = Arc::clone(&slot);
        async move {
            let mut rx = slot.subscribe();
            // `subscribe()` returns a receiver marked-seen, so `changed()` awaits
            // the NEXT transition. A transition between provider construction and
            // this point would otherwise be invisible forever — hence one
            // unconditional reconcile before the loop. Do not drop this as
            // redundant: it is the boot-race fix.
            mesh.refresh_self_manifest_because("startup reconcile");
            loop {
                tokio::select! {
                    changed = rx.changed() => {
                        if changed.is_err() {
                            // Slot dropped — the daemon is shutting down.
                            break;
                        }
                        // Extract before any await: holding a watch borrow across
                        // one deadlocks the publisher.
                        let (lifecycle, reason) = {
                            let st = rx.borrow_and_update();
                            (st.lifecycle.as_str(), st.last_transition_reason.clone())
                        };
                        mesh.refresh_self_manifest_because(&format!(
                            "compute child {lifecycle} ({reason})"
                        ));
                    }
                    _ = tokio::time::sleep(MANIFEST_RECONCILE) => {
                        // Detector, not mechanism — see `reconcile_self_manifest`.
                        mesh.reconcile_self_manifest();
                    }
                }
            }
        }
    });
}

/// Spawn the deferred slot-alias push onto the mesh provider.
pub fn spawn_slot_alias_push(
    daemon: Arc<EmbeddedDaemon>,
    mesh_provider: Arc<sovereign_serving_host::peer_inference::InferenceRouter>,
) {
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
    let mesh_for_alias_push = mesh_provider.clone();
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
                        mesh_for_alias_push.set_slot_aliases(map);
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

/// Spawn the lazy canonical-fingerprint stamper for legacy (pre-fingerprint) ingests.
pub fn spawn_lazy_stamp_fingerprints(engine: Arc<CorpusEngine>) {
    // Lazy-stamp canonical fingerprints for any installed
    // canonicals that don't yet carry one (legacy ingests pre-
    // dating the canonical-sync surface). One BLAKE3 over the
    // content_hash list per corpus; idempotent. Fired in the
    // background so daemon startup doesn't block on it. See
    // `corpus_engine::CorpusEngine::lazy_stamp_legacy_fingerprints`
    // for the contract.
    let engine_for_stamp = Arc::clone(&engine);
    // Supervised one-shot: idempotent per the contract above —
    // DAEMON_RESILIENCE.md P0.4.
    crate::supervise::spawn_supervised("lazy_stamp_fingerprints", move || {
        let engine_for_stamp = Arc::clone(&engine_for_stamp);
        async move {
            engine_for_stamp.lazy_stamp_legacy_fingerprints().await;
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
pub fn spawn_vector_index_readiness_sweep(engine: Arc<CorpusEngine>) {
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
    engine: Arc<CorpusEngine>,
    notes: Arc<dyn sovereign_contracts::notes::AgentNotes>,
) -> axum::Router {
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
    engine: Arc<CorpusEngine>,
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
    // Critical: pass the same `recipes_dir` the `CorpusEngine`
    // was constructed with (see the `let recipes_dir = …` block
    // above where the engine is built). Otherwise the manager
    // writes its generated recipe TOMLs into a directory the
    // engine never reads from, and the first sweep's apply step
    // errors `No registry entry for corpus '<id>'`.
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
                super::workflow_trigger::DaemonWorkflowRuntime::new(format!(
                    "http://127.0.0.1:{}",
                    config.daemon.client_port
                )),
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

/// Build the mesh-routed inference provider (gossip peers composited with
/// pinned worker pods loaded from disk), plus the [`DeferredDaemon`] handle it
/// routes through.
///
/// **The daemon is NOT built here.** It used to be — first thing, empty, so the
/// provider had something to hold — and that inversion is what forced every
/// other dependency to arrive through a setter. The wiring is genuinely cyclic
/// (the daemon serves peers through a provider that routes to peers), so the
/// cycle is broken by the handle: `run_daemon` builds every service, commissions
/// the daemon with all of them at once, then calls `DeferredDaemon::bind`.
/// The handle is an ARGUMENT rather than minted here because a terminal's
/// forwarding provider is built earlier still — in `load_provider`, before this
/// runs — and binds to its entry node through the same handle. One
/// `DeferredDaemon` per daemon, or the terminal would resolve its entry node
/// through a mesh view nobody ever binds (§10.6).
pub async fn build_mesh_provider(
    provider: Arc<dyn InferenceProvider>,
    daemon: Arc<crate::DeferredDaemon>,
) -> (
    Arc<crate::DeferredDaemon>,
    Arc<sovereign_serving_host::peer_inference::InferenceRouter>,
    sovereign_contracts::in_flight::LocalInFlightGauge,
) {
    // Wrap the raw `EmbeddedLlamaCpp` in `InferenceRouter`
    // before installing it as the daemon's serving provider.
    //
    // Without this wrapper the daemon's HTTP `/v1/chat/completions`
    // path silently substitutes a local model whenever the request
    // names a model that's only advertised by a peer (e.g. asking
    // for `gemma-4-E4B-it-Q4_K_M` on a node that only loads
    // `Qwen3.5-9B` and `35B-Q6` would answer with 35B-Q6 and stamp
    // the response accordingly). The wrapper inspects
    // `request.model_id` and either:
    //   * serves locally when self_manifest advertises the id
    //     (the local provider's slot picker handles Fast/Primary/
    //     Code/extras matching by name), or
    //   * forwards the request over HTTP to the peer whose manifest
    //     advertises the id, or
    //   * returns `ModelNotLoaded` if no node serves it — instead
    //     of the previous silent substitution.
    //
    // Mirrors the desktop wiring in
    // `sovereign-desktop/src-tauri/src/state.rs:649` so a request
    // hitting either entrypoint follows the same routing rules.
    // Keep a typed handle to the mesh provider so we can push the
    // slot-alias map into it once `register_local_model_slots` has
    // populated `AppState.slot_aliases`. The trait-object form is
    // what the daemon needs; the typed form is what the alias
    // installer needs.
    // Compose the gossiped-mesh source with any pinned worker pod
    // snapshots persisted on disk. `pod up` writes one snapshot per
    // pod into `~/.svrnmesh/worker-pods/`; this loop loads them at
    // daemon startup and registers each with the inference scheduler
    // so subsequent `chat/completions` calls can route to them.
    // Empty when no pods are configured (the common case) —
    // pinned_source.peer_inference_endpoints() returns an empty Vec
    // and the composite degrades to mesh-only.
    // Spec: docs/PINNED_WORKER_AS_INFERENCE_PEER.md.
    let pinned_source =
        Arc::new(sovereign_serving_host::pinned_worker_source::PinnedWorkerEndpointSource::new());
    if let Some(dir) = sovereign_mesh::pinned_pod_snapshot::default_snapshot_dir() {
        let snapshots = sovereign_mesh::pinned_pod_snapshot::load_all_snapshots(&dir);
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // 2026-05-18: silently expired tokens caused a 6h SEP-on-Vast
        // outage. Registering an already-expired snapshot means every
        // routed inference call gets `token expired` from the pod and
        // is retried via mesh fallback — wasteful and confusing. Skip
        // expired snapshots loudly here so the operator sees the
        // problem at daemon start, not after burning a night of GPU.
        const NEAR_EXPIRY_WARN_SECS: u64 = 4 * 3600; // 4h
        for snap in snapshots {
            let expires_unix = snap.bootstrap_blob.expires_unix;
            if expires_unix <= now_unix {
                tracing::error!(
                    vast_id = %snap.vast_id,
                    expires_unix,
                    expired_secs_ago = now_unix.saturating_sub(expires_unix),
                    "daemon_cmd: pinned-pod snapshot token EXPIRED — \
                     skipping (tear down with `svrn mesh pod down {id}` \
                     or relaunch with `--ttl-hours <N>` to refresh)",
                    id = snap.vast_id,
                );
                continue;
            }
            let remaining = expires_unix.saturating_sub(now_unix);
            if remaining < NEAR_EXPIRY_WARN_SECS {
                tracing::warn!(
                    vast_id = %snap.vast_id,
                    expires_unix,
                    remaining_secs = remaining,
                    "daemon_cmd: pinned-pod snapshot token near expiry \
                     (<4h remaining) — plan a fresh `mesh pod up` if \
                     your run will outlast it"
                );
            }
            match snap.to_pinned_pod() {
                Ok(pod) => {
                    tracing::info!(
                        vast_id = %snap.vast_id,
                        host = %snap.host,
                        port = snap.port,
                        node_id = %pod.node_id,
                        expires_in_h = remaining as f64 / 3600.0,
                        "daemon_cmd: registered pinned worker pod with inference scheduler"
                    );
                    pinned_source.register(pod).await;
                }
                Err(e) => {
                    tracing::warn!(
                        vast_id = %snap.vast_id,
                        error = %e,
                        "daemon_cmd: pinned-pod snapshot rejected — skipping"
                    );
                }
            }
        }
    }
    let composite = Arc::new(
        sovereign_serving_host::pinned_worker_source::CompositeVenueSource::new(
            Arc::clone(&daemon) as Arc<dyn sovereign_contracts::venue::VenueSource>,
            Arc::clone(&pinned_source),
        ),
    );
    // The gauge exists BEFORE the provider: the node creates it here, hands
    // the same `Arc` to the router's guards (`.in_flight`) and returns it so
    // `ServingCore` can give it to `AppState`. There is no install afterwards
    // — the counter is never a slot filled once the provider is built
    // (`quality/DAEMON_CORE.md` §4.2 "Where an install slot breaks a cycle").
    let in_flight_gauge = sovereign_contracts::in_flight::LocalInFlightGauge::new();
    tracing::info!(
        "daemon_cmd: minted the in-flight gauge before the router — gossip \
         will advertise this node's actual load from the same counter the \
         provider's guards write"
    );
    let mesh_provider = Arc::new(
        sovereign_serving_host::peer_inference::InferenceRouter::builder(Arc::clone(&provider))
            .candidates(Arc::clone(&composite) as Arc<dyn sovereign_contracts::venue::VenueSource>)
            .host(Arc::clone(&daemon) as Arc<dyn sovereign_serving_host::venue_host::VenueHost>)
            .manifest(Arc::new(crate::slot_manifest::CoreSlotManifest))
            .in_flight(in_flight_gauge.arc())
            .build(),
    );
    // The pinned pods' TLS handles do not travel with the venue (the scheduler
    // may not name `PinnedTransport`); the router resolves them by `node_id`
    // through this source.
    mesh_provider.set_pinned_transports(Arc::clone(&pinned_source)
        as Arc<dyn sovereign_serving_host::venue_host::PinnedTransportResolver>);
    // A guest link this node accepted lets a granted model id resolve to the
    // LENDING node while the turn stays here. Wired at the COLD-START
    // assembly point, which is the whole reason this function exists: the
    // hot-reload factory in `provider.rs` had it and this did not,
    // so a freshly started daemon kept `NoGuestLenders` and the guest route
    // was dead until something happened to trigger a provider reload.
    // Observed live 2026-08-28: zero `guest-lender` lines in a daemon whose
    // `guest.json` was present and valid.
    mesh_provider.set_guest_source(sovereign_mesh::guest_source::stored_guest_source());
    (daemon, mesh_provider, in_flight_gauge)
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
#[path = "tests/bootstrap_rpc_worker_flag.rs"]
mod rpc_worker_flag_tests;

// Moved to a sibling file: inline, these put this file past its arch-gate
// slack (ARCH §3.1). `#[path]`, so the names are unchanged.
#[cfg(test)]
#[path = "tests/bootstrap_advertise.rs"]
mod advertise_tests;
