// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bootstrap phases extracted verbatim from `run_daemon` so the orchestrator
//! reads as a table of contents instead of a 1,900-line scroll. Each `fn`
//! here is one self-contained startup phase; `mod.rs::run_daemon` calls them
//! in order. Behaviour-preserving — these are code moves, not rewrites.

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use super::discovery_policy;
use super::lifecycle::daemon_pid_path;
use super::warn_orphaned_indexes;
use commonwealth_core::ids::NodeId;
use corpus_engine::{CorpusEngine, EmbedFn};
use corpus_engine_notes::{NodeRoster, NotePropagationEvent, NoteStore, RosterEntry};
use sovereign_core::model_family::{
    EmbedModelInfo, ModelFamily, NormalizationStrategy, PoolingStrategy,
};
use sovereign_core::setup_config::SetupConfig;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::ToolRegistry;
use sovereign_inference::embedded::EmbeddedLlamaCpp;
use sovereign_mesh::EmbeddedDaemon;

/// Resolve this node's persistent id using the same precedence
/// `EmbeddedDaemon::start_daemon` applies on resume: the `node_id` file, then
/// the id baked into `mesh.json`, then generate-and-persist. Shared by the
/// work-atlas store id and the engine's `self_node_id` so a partition-of-self
/// lookup matches the daemon's own id.
pub(super) fn resolve_self_node_id(data_dir: &Path) -> NodeId {
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
pub(super) fn build_node_roster(data_dir: &Path, self_node_id: NodeId) -> Option<NodeRoster> {
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

/// Load the shared GLiNER per-chunk entity extractor once (the ONNX model is
/// ~150 MB for v1, ~795 MB for GLiNER2; one load only). Returns the raw
/// handle (for the NoteStore T2 `GlinerFn` adapter) alongside the
/// trait-object wrapper (for the engine's tiered runner and the folder
/// driver). Both `None` when the model isn't installed — tiered ingest then
/// falls back to RAPTOR-derived entities.
///
/// Generation is chosen inside the shared builder
/// (`sovereign_gliner::configured_model_id`); nothing on this side of the
/// call knows or needs to know which backend it got.
pub(super) fn load_gliner_extractor(
    data_dir: &Path,
) -> (
    Option<Arc<dyn sovereign_gliner::LabeledEntityExtractor>>,
    Option<Arc<dyn corpus_engine::enrichment::tiered::ChunkEntityExtractor>>,
) {
    // Delegated to the shared builder so the desktop's embedded daemon wires
    // an identical stack. See `sovereign_tools::enrichment_bootstrap`.
    sovereign_gliner::load_gliner_extractor(data_dir)
}

/// Build the single shared `CorpusEngine` (powers `/mcp` tools AND
/// `corpus_collaborate` ingest). Wires a REAL embed slot through `provider`
/// (a zero-vector stub here once poisoned 4M chunks — see inline note), the
/// batch variant, the NoteStore T1/T2 hooks, the conv-tiered provider, and
/// the shared GLiNER chunk extractor.
pub(super) fn build_corpus_engine(
    data_dir: &Path,
    provider: Arc<dyn InferenceProvider>,
    notes_store: Arc<NoteStore>,
    gliner_raw: &Option<Arc<dyn sovereign_gliner::LabeledEntityExtractor>>,
    config: &SetupConfig,
    self_node_id: NodeId,
    chunk_entity_extractor: &Option<
        Arc<dyn corpus_engine::enrichment::tiered::ChunkEntityExtractor>,
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
                    .map_err(|e| corpus_engine::Error::Embed(e.to_string()))
            })
        });
        let provider_for_batch = Arc::clone(&provider);
        let batch_embed: corpus_engine::types::BatchEmbedFn =
            Arc::new(move |texts: &[String]| {
                let p = Arc::clone(&provider_for_batch);
                let texts = texts.to_vec();
                Box::pin(async move {
                    p.embed_batch(&texts)
                        .await
                        .map_err(|e| corpus_engine::Error::Embed(e.to_string()))
                })
            });

        // Wire the SAME embed slot into the NoteStore so T1
        // (semantic-blend retrieval) lights up. NoteStore has its
        // own `Error` type — adapt at the boundary so
        // `corpus-engine-notes` stays dep-free of `corpus-engine`
        // per `ARCH §8.3` (one-way edge).
        let provider_for_notes = Arc::clone(&provider);
        let notes_embed: corpus_engine_notes::EmbedFn = Arc::new(move |text: &str| {
            let p = Arc::clone(&provider_for_notes);
            let text = text.to_string();
            Box::pin(async move {
                p.embed(&text).await.map_err(|e| {
                    corpus_engine_notes::Error::Io(std::io::Error::other(format!(
                        "notes embed: {e}"
                    )))
                })
            })
        });
        if let Err(e) = notes_store.set_embed_fn(notes_embed) {
            tracing::warn!(target = "notes", error = e, "notes: embed_fn already set");
        } else {
            tracing::info!(
                target = "notes",
                "notes: T1 embed_fn wired to local embed slot"
            );
        }

        // T2: wire the same GLiNER session into the NoteStore so
        // write_note extracts (entity, kind) pairs into
        // `note_entities`. Skips if GLiNER isn't loaded — T2 then
        // works on author-supplied symbols + files only (still a
        // useful signal for read_notes_related).
        if let Some(ref gliner) = gliner_raw {
            let gliner_clone = Arc::clone(gliner);
            let notes_gliner: corpus_engine_notes::GlinerFn = Arc::new(move |text: &str| {
                let g = Arc::clone(&gliner_clone);
                let text = text.to_string();
                Box::pin(async move {
                    // `extract_mentions` is sync on both backends
                    // (Mutex-locked ONNX session). Run on the blocking
                    // pool so we don't park the async runtime for
                    // ~tens of ms.
                    tokio::task::spawn_blocking(move || g.extract_mentions(&text))
                        .await
                        .map_err(|e| {
                            corpus_engine_notes::Error::Io(std::io::Error::other(format!(
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
                            corpus_engine_notes::Error::Io(std::io::Error::other(format!(
                                "notes gliner: {e}"
                            )))
                        })
                })
            });
            if let Err(e) = notes_store.set_gliner_fn(notes_gliner) {
                tracing::warn!(target = "notes", error = e, "notes: gliner_fn already set");
            } else {
                tracing::info!(
                    target = "notes",
                    "notes: T2 gliner_fn wired to loaded GLiNER session"
                );
            }
        } else {
            tracing::info!(
                target = "notes",
                "notes: GLiNER not loaded; T2 will use author-supplied symbols/files only"
            );
        }
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
        let inference_fn =
            sovereign_tools::corpus::inference_to_inference_fn(Arc::clone(&provider));
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
pub(super) fn build_folder_tiered_deps(
    data_dir: &Path,
    provider: Arc<dyn InferenceProvider>,
    chunk_entity_extractor: Option<
        Arc<dyn corpus_engine::enrichment::tiered::ChunkEntityExtractor>,
    >,
) -> Option<sovereign_tools::local_corpus::watched::enrich::TieredDeps> {
    // Delegated to the shared builder so the desktop's embedded daemon wires
    // an identical stack. See `sovereign_tools::enrichment_bootstrap`.
    sovereign_tools::enrichment_bootstrap::build_folder_tiered_deps(
        data_dir,
        provider,
        chunk_entity_extractor,
    )
}

/// Default bind for an anchor's in-process RPC worker — the value the
/// distributed-inference docs use. Applied only when a `[shared_model]`
/// role asks to serve but no explicit `SOVEREIGN_RPC_SERVE` is set.
const DEFAULT_RPC_BIND: &str = "0.0.0.0:50052";

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
/// [`crate::daemon_cmd::build::containment`].
pub(super) fn rpc_worker_flag(args: &[String]) -> Option<String> {
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
pub(super) fn apply_rpc_worker_flag(args: &[String]) {
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
pub(super) fn apply_shared_model_role_to_env(
    cfg: &sovereign_core::setup_config::SharedModelSection,
) {
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

/// Optional operator scoping of the distributable-worker pool:
/// `SOVEREIGN_RPC_WORKER_ALLOWLIST` = comma-separated node-id hex prefixes.
/// Absent or empty = no filter (every discovered worker is a candidate).
/// Exists for controlled measurements — when several peers advertise RPC
/// serving, this pins a distributed load to the worker under test instead of
/// sharding across whoever happens to be online (first need: the 2026-07-27
/// cloud-tensor-peer proof, where a LAN peer still advertising RPC from an
/// earlier experiment would have contaminated the WAN decode number).
fn worker_allowlist() -> Option<Vec<String>> {
    let raw = std::env::var("SOVEREIGN_RPC_WORKER_ALLOWLIST").ok()?;
    let list: Vec<String> = raw
        .split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    (!list.is_empty()).then_some(list)
}

/// The manually configured RPC workers (`SOVEREIGN_RPC_WORKERS`, comma
/// separated). These never enter the eligible-worker snapshot — discovery only
/// ever adds to them — so any gate reading that snapshot has to union them back
/// in or it would permanently hold a manual setup.
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
pub(crate) fn rpc_discovery_armed() -> bool {
    std::env::var("SOVEREIGN_RPC_DISCOVER").is_ok()
}

fn env_rpc_workers() -> Vec<String> {
    // One reader, in `sovereign_inference::embedded` — this function used to
    // carry a byte-identical copy (TOPOLOGY §10 phase 10).
    sovereign_inference::embedded::rpc_workers_from_env()
}

/// Spawn the mesh RPC-worker auto-discovery loop (opt-in via `SOVEREIGN_RPC_DISCOVER`).
pub(super) fn spawn_rpc_worker_discovery(
    daemon: Arc<EmbeddedDaemon>,
    engine_handle: Option<Arc<EmbeddedLlamaCpp>>,
    distributed_slot: Option<Arc<sovereign_compute::manager::DynamicChildSlot>>,
) {
    // Mesh RPC-worker auto-discovery. With `SOVEREIGN_RPC_DISCOVER` set, this
    // host periodically scans peers' `/status` for advertised RPC workers and
    // feeds them to the embedded engine's worker provider — so distributing a
    // model across the cluster needs no manual `SOVEREIGN_RPC_WORKERS` list.
    // (Applies on the next model load after discovery populates; an eagerly
    // loaded model picks workers up on reload — see register_rpc_workers.)
    if rpc_discovery_armed() {
        let snapshot = Arc::new(std::sync::RwLock::new(Vec::<String>::new()));
        sovereign_inference::embedded::set_rpc_worker_provider({
            let snap = Arc::clone(&snapshot);
            move || snap.read().map(|v| v.clone()).unwrap_or_default()
        });
        // Worker eligibility gate — only distribute to PROVEN-STABLE workers, so a
        // flapping worker can neither thrash the reload loop nor (by crashing
        // mid-compute) GGML_ABORT the host. See `sovereign_mesh::worker_eligibility`.
        let eligibility =
            std::sync::Arc::new(sovereign_mesh::worker_eligibility::WorkerEligibility::default());
        sovereign_mesh::worker_eligibility::set_global(std::sync::Arc::clone(&eligibility));
        let daemon_for_disco = Arc::clone(&daemon);
        let engine_for_reload = engine_handle.clone();
        let distributed_slot = distributed_slot.clone();
        // Close the loop between the two independent respawn authorities. The
        // supervisor restarts a crashed child with identical argv, and the
        // child re-reads its handoff from disk — so when the workers it names
        // are gone, every restart re-dials a corpse and re-aborts on a budget
        // only THIS loop can refresh (3 futile respawns in 48s, 2026-07-28).
        // The gate lets the supervisor ask us before paying for that.
        if std::env::var("SOVEREIGN_COMPUTE_SPAWN_GATE").as_deref() == Ok("0") {
            tracing::warn!(
                target: "compute_child",
                "distributed primary: spawn gate DISABLED by SOVEREIGN_COMPUTE_SPAWN_GATE=0"
            );
        } else if let Some(slot) = &distributed_slot {
            let snap = Arc::clone(&snapshot);
            // Sized once: the GGUF set does not change under a running daemon, and
            // the gate is re-polled every 2s while held — stat'ing every shard on
            // each poll would be pure waste.
            let model_bytes = sovereign_inference::embedded::total_model_bytes(slot.model_path());
            let gate_model_path = slot.model_path().to_path_buf();
            let gate_child_ctx = slot
                .context_size()
                .unwrap_or(sovereign_compute::child_main::DEFAULT_CTX);
            slot.set_spawn_gate(Arc::new(
                move |ctx: &sovereign_compute::manager::SpawnContext<'_>| {
                    let eligible = snap.read().map(|v| v.clone()).unwrap_or_default();
                    let env = env_rpc_workers();
                    // Two independent preconditions. The worker question came
                    // first; the memory question exists because a respawn into a
                    // footprint that did not fit is how a contained child crash
                    // becomes an unusable machine (notes 309c841b, 92d55ceb).
                    let worker = discovery_policy::spawn_gate_verdict(ctx.pinned, &eligible, &env);
                    match worker {
                        sovereign_compute::supervisor::SpawnVerdict::Hold { .. } => worker,
                        sovereign_compute::supervisor::SpawnVerdict::Allow => {
                            // One sample for both terms — a reserve sized off one
                            // reading and a fit judged against another is the
                            // failure mode this subsystem already has six of.
                            let (available, total) =
                                sovereign_inference::embedded::system_memory_bytes();
                            // llama.cpp's projected KV/compute terms — cached
                            // after the first success, so the 2s re-poll while
                            // held does not re-pay the projection.
                            let overheads = sovereign_inference::embedded::projected_overheads(
                                &gate_model_path,
                                gate_child_ctx,
                                false,
                            );
                            discovery_policy::memory_headroom_verdict(
                                discovery_policy::host_share_need_bytes(
                                    model_bytes,
                                    ctx.local_blocks,
                                    ctx.total_blocks,
                                    overheads.as_ref(),
                                ),
                                available,
                                sovereign_inference::embedded::host_reserve_bytes_detected(total),
                            )
                        }
                    }
                },
            ));
        }
        // Distributed-primary child state, shared with the warm task because a
        // warm can take minutes of GGUF transfer and must never block the 15s
        // discovery tick.
        let child_state: Arc<std::sync::Mutex<ChildDistributionState>> =
            Arc::new(std::sync::Mutex::new(ChildDistributionState::default()));
        let child_busy = Arc::new(std::sync::atomic::AtomicBool::new(false));
        // Supervised: a panic here used to silently freeze worker
        // discovery + shared-model host failover for the rest of the
        // process's life (DAEMON_RESILIENCE.md P0.4). Loop state
        // (last_loaded / debounce) resets on restart — rediscovery
        // reconverges within a tick.
        crate::supervise::spawn_supervised("rpc_worker_discovery", move || {
            let daemon_for_disco = Arc::clone(&daemon_for_disco);
            let engine_for_reload = engine_for_reload.clone();
            let snapshot = Arc::clone(&snapshot);
            let eligibility = std::sync::Arc::clone(&eligibility);
            let distributed_slot = distributed_slot.clone();
            let child_state = Arc::clone(&child_state);
            let child_busy = Arc::clone(&child_busy);
            async move {
                // Live child lifecycle, read each tick for engagement evidence.
                // Subscribed once — the receiver survives every respawn/retire
                // (the channel is owned by the slot, not a generation).
                let child_lifecycle_rx = distributed_slot.as_ref().map(|s| s.subscribe());
                // `last_loaded` = worker set the resident primary was loaded across;
                // `current` = ELIGIBLE set seen last tick (for debounce — wait for it
                // to stop changing before paying a reload).
                let mut last_loaded: Vec<String> = Vec::new();
                let mut current: Vec<String> = Vec::new();
                let mut stable_since = std::time::Instant::now();
                // When the eligible set first went EMPTY. Retiring the child is
                // gated on this persisting, because the peer most likely to look
                // absent is the one busy serving our own model warm.
                let mut empty_since: Option<std::time::Instant> = None;
                // Designated-host pin (parsed once). When present + eligible it wins;
                // otherwise the host role is the elected leader of the anchors.
                let host_pin = std::env::var("SOVEREIGN_SHARED_MODEL_HOST_NODE_ID")
                    .ok()
                    .and_then(|s| commonwealth_core::ids::NodeId::from_hex(&s));
                let allowlist = worker_allowlist();
                if let Some(list) = &allowlist {
                    tracing::info!(
                        ?list,
                        "shared-model: RPC worker allowlist active — non-matching discovered workers are excluded"
                    );
                }
                let mut was_host = false;
                loop {
                    // Raw discovery → eligibility gate → only PROVEN-STABLE workers
                    // reach the provider + the reload decision. A flapping worker stays
                    // out of `workers`, so the set the debounce compares never
                    // oscillates on a flap — the source of the 11-reloads-in-27min thrash.
                    let mut raw = daemon_for_disco.discover_rpc_workers().await;
                    if let Some(list) = &allowlist {
                        let keep_node =
                            |hex: &str| list.iter().any(|p| hex.starts_with(p.as_str()));
                        raw.workers.retain(|w| {
                            let hex = w.node_id.to_hex();
                            let keep = keep_node(&hex);
                            if !keep {
                                tracing::debug!(
                                    worker = %hex,
                                    endpoint = %w.endpoint,
                                    "shared-model: discovered worker excluded by SOVEREIGN_RPC_WORKER_ALLOWLIST"
                                );
                            }
                            keep
                        });
                        // The unconfirmed set must be filtered by the SAME rule,
                        // or an allowlist-excluded peer could hold eligibility
                        // state it is never allowed to have.
                        raw.unconfirmed.retain(|n| keep_node(&n.to_hex()));
                    }
                    // First-party engagement evidence: a worker carrying our
                    // own child's warm or serving session cannot answer a probe
                    // for as long as it does (ggml's RPC server accepts one
                    // connection at a time) — but that traffic is a better
                    // probe than the probe. Feeding the engaged endpoints into
                    // the tick is what makes the eligibility grace independent
                    // of load duration: before this, every model needing >120s
                    // to load was killed mid-load by its own absence grace
                    // (2026-08-02, notes 92d55ceb/16fc9204).
                    if let (Some(slot), Some(rx)) = (&distributed_slot, &child_lifecycle_rx) {
                        // A warm in flight is actively moving shard bytes to
                        // the targets recorded at Respawn time.
                        if child_busy.load(std::sync::atomic::Ordering::SeqCst) {
                            if let Ok(st) = child_state.lock() {
                                raw.engaged.extend(st.attempted.iter().cloned());
                            }
                        }
                        // A live child holds loading/serving RPC sessions
                        // across its pinned endpoints. Degraded, Restarting and
                        // Failed deliberately do NOT vouch: those are exactly
                        // the states where the worker may be the thing that
                        // died, and discovery must be allowed to see it.
                        use sovereign_compute::child::ChildLifecycle as Lc;
                        if matches!(
                            rx.borrow().lifecycle,
                            Lc::Starting | Lc::Warming | Lc::Serving
                        ) {
                            raw.engaged.extend(slot.pinned_endpoints());
                        }
                        raw.engaged.sort();
                        raw.engaged.dedup();
                    }
                    let now = std::time::Instant::now();
                    eligibility.observe_outcome(&raw, now);
                    let workers = eligibility.eligible(now); // sorted + deduped, eligible-only
                    if let Ok(mut w) = snapshot.write() {
                        *w = workers.clone();
                    }
                    if workers != current {
                        tracing::info!(
                            eligible = workers.len(),
                            discovered = raw.workers.len(),
                            // `unconfirmed` vs `polled` is what makes an
                            // "eligible=0 discovered=0" line readable: it says
                            // whether we heard "no worker" or heard nothing.
                            unconfirmed = raw.unconfirmed.len(),
                            polled = raw.polled,
                            workers = ?workers,
                            "mesh RPC eligible-worker set changed"
                        );
                        current = workers.clone();
                        stable_since = std::time::Instant::now();
                    }
                    // Maintained every tick, not just on change, so the grace
                    // measures how long the set has ACTUALLY been empty.
                    empty_since = if current.is_empty() {
                        empty_since.or_else(|| Some(std::time::Instant::now()))
                    } else {
                        None
                    };
                    // Host-role decision, re-evaluated every tick over gossiped
                    // membership — this is the failover mechanism. A non-host anchor
                    // still discovers + keeps its eligibility warm above, but does NOT
                    // distribute below, so at most the elected leader assembles the
                    // split. `should_host` is deterministic over the anchor set, so
                    // all anchors converge without coordination; a minority that can't
                    // see quorum still won't load (the quorum gate holds it "forming").
                    let anchors = daemon_for_disco.eligible_anchors().await;
                    let am_host = match daemon_for_disco.self_node_id().await {
                        Some(me) => {
                            commonwealth_core::partition::should_host(me, host_pin, &anchors)
                        }
                        None => false, // identity not ready yet → don't host
                    };
                    if am_host != was_host {
                        tracing::info!(
                            am_host,
                            anchors = anchors.len(),
                            pinned = host_pin.is_some(),
                            "shared-model: host-role transition"
                        );
                        // Publish for `/v1/mesh/status` so the mesh soak can assert
                        // the no-split-brain invariant (≤1 host across the fleet).
                        sovereign_mesh::mesh_http::set_shared_model_host(am_host);
                        was_host = am_host;
                    }

                    // In child mode the loop can't track "what is loaded"
                    // locally — the warm completes asynchronously — so it reads
                    // the shared cell the warm task writes. The comparison is
                    // against the worker set we last ACTED ON, not the set that
                    // ended up warm: a warm can legitimately place on a subset
                    // (a worker that went ineligible between discovery and
                    // planning), and comparing against the subset would make
                    // `changed` true forever and respawn the child every tick.
                    let mut retry_due = false;
                    if distributed_slot.is_some() {
                        let st = child_state.lock().unwrap_or_else(|e| e.into_inner());
                        last_loaded = st.attempted.clone();
                        retry_due = st
                            .retry_at
                            .map(|t| std::time::Instant::now() >= t)
                            .unwrap_or(false);
                    }

                    // Reload when the worker set CHANGES (grow or shrink) vs what's
                    // loaded, once it's been stable briefly. A shrink prunes the dead
                    // worker's device on reload (live_device_list_if_pruning_needed).
                    let changed = current != last_loaded || retry_due;
                    // Shrink-fast-prune: if a worker the resident primary is loaded
                    // ACROSS dropped out of the eligible set, reload IMMEDIATELY — the
                    // dead worker must be pruned (live_device_list_if_pruning_needed)
                    // before it GGML_ABORTs the host mid-compute, and survivors' warm
                    // caches make the re-plan fast. A pure grow (new workers, all loaded
                    // ones still present) keeps the anti-thrash STABLE debounce.
                    let shrank = last_loaded.iter().any(|w| !current.contains(w));
                    if am_host && changed && shrank {
                        let lost: Vec<&String> = last_loaded
                            .iter()
                            .filter(|w| !current.contains(*w))
                            .collect();
                        tracing::info!(
                            ?lost,
                            "shared-model: anchor dropped — reloading now to prune + re-form on survivors"
                        );
                    }
                    match (&distributed_slot, &engine_for_reload) {
                        // ── Child mode: the primary lives in a supervised
                        // child, so a worker-set change is a KILL + RESPAWN,
                        // never an in-place reload. An in-place reload has to
                        // free the old sharded model's buffers on workers that
                        // may already be gone, and ggml's RPC client aborts the
                        // process on a dead endpoint — that is exactly how the
                        // daemon died on 2026-07-27 (note c4ef6fa0), from this
                        // very code path.
                        //
                        // The decision itself is `discovery_policy` — pure, so it
                        // can be exercised without a mesh. Only the EFFECTS live
                        // here.
                        (Some(slot), _) => {
                            let tick = discovery_policy::TickInputs {
                                am_host,
                                busy: child_busy.load(std::sync::atomic::Ordering::SeqCst),
                                current: &current,
                                last_loaded: &last_loaded,
                                retry_due,
                                stable_for: stable_since.elapsed(),
                                empty_for: empty_since.map(|t| t.elapsed()),
                                child_age: slot.spawned_at().map(|t| t.elapsed()),
                            };
                            match discovery_policy::decide_child_action(&tick) {
                                discovery_policy::ChildAction::Hold => {}
                                discovery_policy::ChildAction::Busy => {
                                    tracing::debug!(
                                        "distributed primary: warm/respawn already in flight — skipping this tick"
                                    );
                                }
                                // Stay unavailable rather than fall back to a
                                // local load that would starve the host.
                                discovery_policy::ChildAction::Retire { reason } => {
                                    slot.retire(&reason);
                                    if let Ok(mut st) = child_state.lock() {
                                        st.attempted.clear();
                                        st.retry_at = None;
                                    }
                                }
                                // Deliberately does NOT clear `attempted`.
                                // Leaving it populated is what keeps `changed`
                                // true so the grace is re-evaluated every tick —
                                // and what makes recovery free: when the worker
                                // returns, `current == attempted`, the tick is a
                                // plain Hold, and the still-serving child is
                                // never disturbed.
                                discovery_policy::ChildAction::WaitForWorkers {
                                    empty_for_secs,
                                    child_age_secs,
                                } => {
                                    tracing::info!(
                                        target: "compute_child",
                                        empty_for_secs,
                                        child_age_secs,
                                        "distributed primary: eligible set is empty — holding the \
                                         child while the grace burns down (a peer busy serving our \
                                         own warm looks absent)"
                                    );
                                }
                                discovery_policy::ChildAction::Respawn { workers } => {
                                    child_busy.store(true, std::sync::atomic::Ordering::SeqCst);
                                    // Record the attempt BEFORE it runs, so the
                                    // next tick compares against it and does not
                                    // queue a second warm behind this one.
                                    if let Ok(mut st) = child_state.lock() {
                                        st.attempted = workers.clone();
                                        st.retry_at = None;
                                    }
                                    let slot = Arc::clone(slot);
                                    let child_state = Arc::clone(&child_state);
                                    let child_busy = Arc::clone(&child_busy);
                                    // Detached: warming seeds every worker's
                                    // shard and can take minutes of GGUF
                                    // transfer. The 15s tick must keep running
                                    // (host election, eligibility) throughout.
                                    tokio::spawn(async move {
                                        respawn_distributed_primary(slot, workers, child_state)
                                            .await;
                                        child_busy
                                            .store(false, std::sync::atomic::Ordering::SeqCst);
                                    });
                                }
                            }
                        }
                        // ── In-process arm. Deliberately NOT sharing the policy
                        // function above: its consequences are different in kind
                        // (this `reload_primary` is the uncatchable GGML_ABORT of
                        // P0.4), and a shared decision would invite treating the
                        // two paths as interchangeable.
                        //
                        // Only the host distributes. A non-host anchor keeps its
                        // worker discovery + eligibility warm (above) so that, the
                        // moment it's elected host, `changed` vs its empty
                        // `last_loaded` triggers an immediate assemble on the
                        // already-settled survivors.
                        (None, engine) => {
                            if am_host
                                && changed
                                && (shrank || stable_since.elapsed() >= discovery_policy::STABLE)
                            {
                                match engine {
                                    Some(engine) => {
                                        tracing::info!(workers = ?current, "RPC worker set changed — reloading primary to redistribute");
                                        match engine.reload_primary().await {
                                            Ok(()) => last_loaded = current.clone(),
                                            Err(e) => {
                                                tracing::warn!(error = %e, "reload_primary failed; will retry next tick")
                                            }
                                        }
                                    }
                                    // No primary handle (provider build failed) —
                                    // keep the snapshot fresh so a later manual
                                    // load still picks workers up.
                                    None => last_loaded = current.clone(),
                                }
                            }
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                }
            }
        });
    }
}

/// Distributed-primary child state shared between the discovery loop and the
/// detached warm task.
#[derive(Default)]
struct ChildDistributionState {
    /// The eligible worker set the last warm+respawn ACTED ON — not the set
    /// that ended up warm. Comparing against the attempt is what keeps a
    /// partial placement (a worker that went ineligible between discovery and
    /// planning) from looking like a permanent "changed" and respawning the
    /// child on every tick.
    attempted: Vec<String>,
    /// When to try again after a refusal, even though nothing changed. Without
    /// it, one transient warm failure against an otherwise stable worker set
    /// would leave the primary down until a worker happened to join or leave.
    retry_at: Option<std::time::Instant>,
}

/// How long to wait before re-attempting a refused warm against an unchanged
/// worker set. Long enough that a genuinely-forming cluster isn't hammered,
/// short enough that a transient failure isn't a permanent outage.
const CHILD_WARM_RETRY: std::time::Duration = std::time::Duration::from_secs(120);

/// Warm the mesh workers for the distributed primary, then respawn the compute
/// child across exactly the set that warmed.
///
/// The split of labour is forced by what each process can reach: only the
/// daemon can warm (the orchestrator needs the mesh member directory, the iroh
/// transport bases, and the daemon's own ports), and only the child should
/// load (ggml's RPC client aborts the process it runs in when a worker dies).
/// So the daemon plans + warms, writes what it decided into a handoff file, and
/// the child loads against it.
///
/// The plan crosses with the worker list on purpose. The shard plan is cached
/// per `(model, worker set)` precisely because a worker's free VRAM shifts by
/// its own cached shard, so re-planning after a warm cuts the blocks
/// differently — and that cache is process-local. A child that re-planned would
/// miss every warm cache and fall back to bulk weight send (the send()
/// deadlock). Pinning the daemon's plan in the child keeps warm-time and
/// load-time placement identical across the process boundary.
async fn respawn_distributed_primary(
    slot: Arc<sovereign_compute::manager::DynamicChildSlot>,
    workers: Vec<String>,
    child_state: Arc<std::sync::Mutex<ChildDistributionState>>,
) {
    use sovereign_inference::embedded::DistributedWarmOutcome;

    /// Park the slot unavailable and schedule one retry, so a refusal against
    /// an unchanged worker set is a delay, not a permanent outage.
    fn refuse(
        slot: &sovereign_compute::manager::DynamicChildSlot,
        state: &std::sync::Mutex<ChildDistributionState>,
        reason: &str,
    ) {
        slot.retire(reason);
        if let Ok(mut st) = state.lock() {
            st.retry_at = Some(std::time::Instant::now() + CHILD_WARM_RETRY);
        }
    }

    /// Park the slot unavailable with **no retry timer**, for a refusal that
    /// waiting cannot fix.
    ///
    /// A cluster that is still forming resolves itself as anchors join, so
    /// [`refuse`] retries. A device whose assigned share exceeds its memory
    /// does not: re-planning the same model across the same devices produces
    /// the same overflow, and a timer just repeats the refusal on a schedule.
    ///
    /// This costs no new state and no new timer. The discovery loop already
    /// re-plans for free when the worker set changes (`changed = current !=
    /// last_loaded`), which is exactly — and only — when the answer could
    /// differ.
    fn park(
        slot: &sovereign_compute::manager::DynamicChildSlot,
        state: &std::sync::Mutex<ChildDistributionState>,
        reason: &str,
    ) {
        slot.retire(reason);
        if let Ok(mut st) = state.lock() {
            st.retry_at = None;
        }
    }

    tracing::info!(
        target: "compute_child",
        workers = ?workers,
        model = %slot.model_path().display(),
        "distributed primary: warming worker shards before respawning the child"
    );

    let model_path = slot.model_path().to_path_buf();
    // The context the CHILD will load with — the warm path's memory projection
    // sizes KV from it, so it must be the child's real value, not a guess.
    let child_ctx = slot
        .context_size()
        .unwrap_or(sovereign_compute::child_main::DEFAULT_CTX);
    // Blocking: the warm orchestrator bridges to async with `block_on` and can
    // run for minutes. It must not run on a runtime worker thread.
    let outcome = match tokio::task::spawn_blocking(move || {
        sovereign_inference::embedded::warm_distributed_primary(&model_path, child_ctx)
    })
    .await
    {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(error = %e, "distributed primary: warm task panicked");
            refuse(&slot, &child_state, "warm task panicked");
            return;
        }
    };

    match outcome {
        DistributedWarmOutcome::Warm { endpoints, plan } => {
            let handoff = sovereign_compute::distribution::DistributionHandoff {
                endpoints: endpoints.clone(),
                plan,
            };
            match slot.respawn_distributed(&handoff) {
                Ok(()) => {
                    tracing::info!(
                        target: "compute_child",
                        attempted = ?workers,
                        warmed = ?endpoints,
                        "distributed primary: child respawned across the warmed worker set"
                    );
                    // The attempt already recorded `workers`; clear the retry
                    // timer. Placing on a SUBSET of the eligible set is a
                    // normal outcome (a worker can go ineligible between
                    // discovery and planning) and must not read as "changed".
                    if let Ok(mut st) = child_state.lock() {
                        st.retry_at = None;
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "distributed primary: respawn failed");
                    refuse(&slot, &child_state, "child respawn failed");
                }
            }
        }
        // Every refusal below is "stay unavailable" — the same posture the
        // in-process path takes with InsufficientCluster/LocalUnfit. Falling
        // back to a local load of a model this size is what collapsed the
        // desktop session on 2026-07-27.
        DistributedWarmOutcome::InsufficientCluster { eligible, quorum } => {
            let reason =
                format!("cluster forming — {eligible} eligible anchor(s), quorum {quorum}");
            tracing::info!(target: "compute_child", eligible, quorum, "distributed primary: {reason}");
            refuse(&slot, &child_state, &reason);
        }
        DistributedWarmOutcome::WorkerUnfit(overflow) => {
            // Parked, not retried: the cluster is fully formed and has the
            // memory in aggregate — one device just cannot hold what it was
            // assigned. Waiting changes nothing; a worker-set change re-plans
            // for free.
            tracing::warn!(
                target: "compute_child",
                device = overflow.device_index,
                endpoint = ?overflow.endpoint,
                held_mb = overflow.held_mb,
                need_mb = overflow.need_mb,
                capacity_mb = overflow.capacity_mb,
                "distributed primary: per-device fit refusal — parking (not retrying)"
            );
            park(&slot, &child_state, &overflow.refusal());
        }
        DistributedWarmOutcome::Unplannable => {
            refuse(
                &slot,
                &child_state,
                "could not plan the shards (no RPC device, unreadable GGUF, or unmappable worker)",
            );
        }
        DistributedWarmOutcome::WarmFailed { error } => {
            tracing::warn!(target: "compute_child", %error, "distributed primary: warm failed");
            refuse(&slot, &child_state, &format!("worker warm failed: {error}"));
        }
    }
}

/// How often the refresher re-derives the manifest to check that no transition
/// was missed. Slow enough to be free, fast enough that a missed event is a
/// minute of wrongness rather than an outage.
const MANIFEST_RECONCILE: std::time::Duration = std::time::Duration::from_secs(60);

/// Keep the mesh self-manifest in step with the distributed primary's lifecycle.
///
/// `build_self_manifest` is a SNAPSHOT of the local provider, taken once in
/// `MeshInferenceProvider::with_peer_source`. At that moment the distributed
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
    mesh_provider: Arc<sovereign_mesh::peer_inference::MeshInferenceProvider>,
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

/// Spawn the deferred slot-alias push + in-flight-publisher install onto AppState.
pub(super) fn spawn_slot_alias_push(
    daemon: Arc<EmbeddedDaemon>,
    mesh_provider: Arc<sovereign_mesh::peer_inference::MeshInferenceProvider>,
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
    // Same spawned task also installs MIP's in-flight publisher Arc
    // onto AppState — feeds the gossip-load-awareness path so peers
    // see this node's true serving load instead of phantom-idle.
    // See `sovereign/docs/MESH_LOAD_AWARENESS.md` for the design.
    let daemon_for_alias_push = Arc::clone(&daemon);
    let mesh_for_alias_push = mesh_provider.clone();
    // Supervised one-shot: publisher install + alias push are
    // idempotent, so a panic-restart just retries the wiring
    // (DAEMON_RESILIENCE.md P0.4).
    crate::supervise::spawn_supervised("slot_alias_push", move || {
        let daemon_for_alias_push = Arc::clone(&daemon_for_alias_push);
        let mesh_for_alias_push = mesh_for_alias_push.clone();
        async move {
            // Poll briefly for the AppState to be available. The
            // setup transition usually completes within a few
            // hundred ms; cap at 30s so a stuck setup never hangs
            // this spawn.
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
            let mut publisher_installed = false;
            loop {
                if let Some(state) = daemon_for_alias_push.app_state().await {
                    if !publisher_installed {
                        state
                            .install_in_flight_publisher(mesh_for_alias_push.in_flight_publisher());
                        publisher_installed = true;
                        tracing::info!(
                            "daemon_cmd: installed in-flight publisher on AppState \
                             — gossip will now advertise this node's actual load"
                        );
                    }
                    let snapshot = state.inner.slot_aliases.load();
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

/// Load this node's measurement history into the gossip buffer.
///
/// The mesh KV store is `in_memory()` — a wire buffer, not storage — so
/// everything this node has ever published vanishes from the mesh when the daemon
/// restarts. `svrn mesh bench` publishes each run as it takes it
/// (`POST /v1/mesh/measurements`), which covers new runs and nothing else: after
/// a restart a peer's next anti-entropy round would find our namespace empty and
/// our history would quietly evaporate one restart at a time, while still looking
/// perfectly intact in `~/.svrnmesh/mesh-measurements.json`. This is the step
/// that makes the file authoritative rather than merely local.
///
/// Idempotent by construction: `wire_key` is derived from the record, so a
/// republish overwrites its own entry instead of accumulating copies. Invalid
/// runs are refused by `to_wire` and stay home.
///
/// Synchronous and cheap — the file is capped at `MAX_RUNS_PER_KEY` per
/// configuration and is a few KB in practice. No spawn, so a peer that gossips
/// with us immediately after boot cannot race an empty namespace.
pub(super) fn republish_local_measurements(
    work_atlas_mesh_store: &commonwealth_state::MeshStore,
    self_node_id: NodeId,
) {
    use sovereign_core::mesh_measurements as mm;

    let file = mm::load();
    let (mut published, mut withheld) = (0usize, 0usize);
    for rec in file.records() {
        let Some(bytes) = mm::to_wire(rec) else {
            withheld += 1;
            continue;
        };
        match work_atlas_mesh_store.set(
            mm::MEASUREMENTS_APP_ID,
            &mm::wire_key(rec),
            bytes.into(),
            self_node_id,
        ) {
            Ok(_) => published += 1,
            Err(e) => tracing::warn!(
                target = "mesh_measurements",
                error = %e,
                "mesh-measurements: boot republish set() failed"
            ),
        }
    }
    if published > 0 || withheld > 0 {
        tracing::info!(
            target = "mesh_measurements",
            published,
            withheld,
            total = file.records().len(),
            "mesh-measurements: republished local history into the gossip buffer"
        );
    }
}

/// Wire NoteStore's outbound propagation sink to publish notes via the mesh store.
pub(super) fn wire_note_propagation_sink(
    notes_store: Arc<NoteStore>,
    work_atlas_mesh_store: Arc<commonwealth_state::MeshStore>,
    self_node_id: NodeId,
    convergence: Arc<commonwealth_api::state::ConvergenceRecord>,
) {
    // ── NoteStore propagation wiring ─────────────────────────────
    //
    // Now that `mesh_store` is live, wire NoteStore's outbound
    // sink to publish global non-private notes via app_id="notes"
    // (and private notes via "notes-private", which is
    // structurally gossip-excluded — see
    // `commonwealth-state::peer_preferences::GOSSIP_EXCLUDED_APP_IDS`).
    let mesh_for_sink = Arc::clone(&work_atlas_mesh_store);
    let self_id_for_sink = self_node_id;
    let sink: corpus_engine_notes::PropagationSinkFn =
        Arc::new(move |ev: &NotePropagationEvent| {
            let app_id = if ev.tombstone {
                // Tombstones ride the public namespace so peers
                // converge to the deleted state. Private notes
                // never propagate, so private tombstones don't
                // need to either.
                "notes"
            } else {
                "notes"
            };
            // Receipt stamp (order commons-fluency fix 3): the wire
            // copy carries the publication clock — the moment THIS
            // sink's set() accepted it — which is the origin end of
            // the two-sided receipt. The original event is left
            // untouched; the store stamps its row from the return
            // value below. One timestamp, two uses: the wire copy and
            // the liveness stamp (fix 9) share it, so `/status`'s
            // convergence age can never disagree with the receipt.
            let sent_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let mut wired = ev.clone();
            wired.sent_at = Some(sent_at);
            // This `to_vec` is the wire. Since order
            // `mesh-scale-t1-notes` it cannot emit the note's
            // embedding whatever `ev` holds — `NotePropagationEvent`
            // serializes that field as `null` unconditionally — which
            // took a gossiped note from a measured 16.1 KB to ~1.6 KB
            // and the 8 MiB push limit from ~520 notes to ~5,300
            // (research/scale-analysis/
            // MESH_SCALE_100_USERS_1000_CORPORA.md §8.3.1). Peers
            // re-embed the content in their own model space at ingest.
            match serde_json::to_vec(&wired) {
                Ok(bytes) => {
                    match mesh_for_sink.set(
                        app_id,
                        &ev.content_hash,
                        bytes.into(),
                        self_id_for_sink,
                    ) {
                        // `Ok(_)`: the bool is "did the value change"
                        // (a re-publish of the same hash reports
                        // false) — either way the note IS on the mesh,
                        // which is what the receipt means.
                        Ok(_) => {
                            tracing::debug!(
                                target = "notes",
                                content_hash = %ev.content_hash,
                                tombstone = ev.tombstone,
                                sent_at = wired.sent_at,
                                "notes: propagated"
                            );
                            // Liveness stamp (fix 9): the origin's
                            // publish path just succeeded — `/status`
                            // reads this as the convergence age.
                            convergence.record_outbound_publish_success(sent_at);
                            true
                        }
                        Err(e) => {
                            tracing::warn!(
                                target = "notes",
                                error = %e,
                                content_hash = %ev.content_hash,
                                "notes: mesh propagation sink set() failed"
                            );
                            false
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        target = "notes",
                        error = %e,
                        "notes: failed to serialize propagation event"
                    );
                    false
                }
            }
        });
    if let Err(e) = notes_store.set_propagation_sink(sink) {
        tracing::warn!(
            target = "notes",
            error = e,
            "notes: propagation_sink already set"
        );
    } else {
        tracing::info!(
            target = "notes",
            "notes: propagation_sink wired to MeshStore (app_id=notes)"
        );
    }
}

/// Spawn the one-shot pre-T1/T2 note tier-artifact backfill (embeddings + entities).
pub(super) fn spawn_notes_tier_backfill(notes_store: Arc<NoteStore>) {
    // One-shot tier-artifact backfill: pre-T1/T2 notes (anything
    // written before `embed_fn`/`gliner_fn` were wired) get
    // embeddings + entity rows on a background task so the
    // existing notes corpus benefits from semantic recall +
    // related-notes lookup immediately, not only when re-written.
    // Runs once per daemon start. Best-effort: rows that error
    // skip + pick up on the next start.
    //
    // Since order `mesh-scale-t1-notes` this is also the recovery
    // path for gossiped notes: the wire no longer carries vectors, so
    // `ingest_remote_notes` re-embeds each remote note locally, and
    // any it could not embed (embed slot down) lands here with no
    // `note_embeddings` row — outside the cosine pool, never blended
    // unembedded, until this pass picks it up. One-shot is the reason
    // the poller warns when its deferred count is non-zero.
    let notes_for_backfill = Arc::clone(&notes_store);
    // Supervised one-shot: best-effort + idempotent per the contract
    // above (rows that error skip + pick up next start) —
    // DAEMON_RESILIENCE.md P0.4.
    crate::supervise::spawn_supervised("notes_tier_backfill", move || {
        let notes_for_backfill = Arc::clone(&notes_for_backfill);
        async move {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            let report = notes_for_backfill.backfill_tier_artifacts(0).await;
            if report.embeddings_backfilled > 0 || report.entities_backfilled > 0 {
                tracing::info!(
                    target = "notes",
                    embeddings = report.embeddings_backfilled,
                    entities = report.entities_backfilled,
                    embed_skipped = report.embed_skipped,
                    entity_skipped = report.entity_skipped,
                    "notes: tier-artifact backfill done"
                );
            }
        }
    });

    // TTL sweep — this is what keeps the store all-signal WITHOUT anyone running
    // `notes rationalize` by hand. Operational-exhaust kinds (tool_decision,
    // checkpoint…) age out on their own: first sweep ~30s after boot, then every
    // 24h. Tombstone (not delete) so it's resurrection-proof and, for any legacy
    // Global telemetry, gossips the removal to peers. TTL tunable via
    // SOVEREIGN_NOTES_EPHEMERAL_TTL_DAYS (default 30; <=0 disables).
    let notes_for_ttl = Arc::clone(&notes_store);
    // Supervised: the sweep is tombstone-idempotent; a panic must not
    // silently end TTL hygiene (DAEMON_RESILIENCE.md P0.4).
    crate::supervise::spawn_supervised("notes_ttl_sweep", move || {
        let notes_for_ttl = Arc::clone(&notes_for_ttl);
        async move {
            let ttl_days: i64 = std::env::var("SOVEREIGN_NOTES_EPHEMERAL_TTL_DAYS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(30);
            if ttl_days <= 0 {
                tracing::info!(
                    target = "notes",
                    "notes: ephemeral TTL sweep disabled (ttl_days<=0)"
                );
                return;
            }
            let ttl_secs = ttl_days * 86_400;
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(24 * 60 * 60));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                match notes_for_ttl.purge_expired_ephemeral(ttl_secs).await {
                    Ok(n) if n > 0 => tracing::info!(
                        target = "notes",
                        swept = n,
                        ttl_days,
                        "notes: TTL sweep tombstoned expired ephemeral notes"
                    ),
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(target = "notes", error = %e, "notes: TTL sweep failed")
                    }
                }
            }
        }
    });
}

/// Spawn the poller that bridges inbound gossip note entries into `NoteStore`.
pub(super) fn spawn_notes_ingest_poller(
    work_atlas_mesh_store: Arc<commonwealth_state::MeshStore>,
    notes_store: Arc<NoteStore>,
    self_node_id: NodeId,
    convergence: Arc<commonwealth_api::state::ConvergenceRecord>,
) {
    // Ingest poller: bridge inbound MeshStore entries (merged from
    // gossip) into `NoteStore::ingest_remote_notes`. MeshStore
    // doesn't expose a merge-callback today — periodic scan is
    // the path of least resistance. `ingest_remote_notes` is
    // idempotent (content_hash dedup) so re-reads cost nothing.
    //
    // Cadence: 10s, matching the gossip push-pull cadence. Skips
    // entries whose `origin` is `self_node_id` (those are notes
    // WE published; reingesting via our own sink would be a no-op
    // but wastes a JSON roundtrip).
    let mesh_for_poller = Arc::clone(&work_atlas_mesh_store);
    let notes_for_poller = Arc::clone(&notes_store);
    let self_id_for_poller = self_node_id;
    let convergence_for_poller = Arc::clone(&convergence);
    // Supervised: ingest is content-hash idempotent; a panic must not
    // silently stop cross-peer note convergence (DAEMON_RESILIENCE.md
    // P0.4).
    crate::supervise::spawn_supervised("notes_ingest_poller", move || {
        let mesh_for_poller = Arc::clone(&mesh_for_poller);
        let notes_for_poller = Arc::clone(&notes_for_poller);
        let self_id_for_poller = self_id_for_poller;
        let convergence_for_poller = Arc::clone(&convergence_for_poller);
        async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let entries = match mesh_for_poller.scan("notes", "") {
                    Ok(e) => e,
                    Err(err) => {
                        tracing::debug!(
                            target = "notes",
                            error = %err,
                            "notes: ingest poller scan failed"
                        );
                        continue;
                    }
                };
                let mut events: Vec<NotePropagationEvent> = Vec::new();
                for entry in entries {
                    if entry.origin == self_id_for_poller {
                        continue;
                    }
                    match serde_json::from_slice::<NotePropagationEvent>(&entry.value) {
                        Ok(ev) => events.push(ev),
                        Err(e) => {
                            tracing::warn!(
                                target = "notes",
                                key = %entry.key,
                                error = %e,
                                "notes: ingest poller could not decode entry; skipping"
                            );
                        }
                    }
                }
                if events.is_empty() {
                    continue;
                }
                match notes_for_poller.ingest_remote_notes(events).await {
                    Ok(report) => {
                        // Liveness stamp (fix 9): a peer batch was
                        // applied — `/status` reads this as the
                        // inbound convergence age. Stamped on ANY Ok:
                        // a deduplicated-only batch still proves the
                        // scan→decode→apply loop ran.
                        convergence_for_poller
                            .record_inbound_ingest_success(sovereign_core::time::unix_now());
                        if report.inserted > 0 || report.tombstoned > 0 || report.forked > 0 {
                            tracing::info!(
                                target = "notes",
                                inserted = report.inserted,
                                tombstoned = report.tombstoned,
                                forked = report.forked,
                                deduplicated = report.deduplicated,
                                rejected = report.rejected,
                                // Order `mesh-scale-t1-notes`. These
                                // three answer, from the daemon log
                                // alone: did the peers' notes get an
                                // embedding in OUR model space
                                // (`recomputed`), are any sitting
                                // outside the cosine pool waiting on
                                // the backfill (`deferred`), and is
                                // some peer still on a pre-strip build
                                // shipping vectors we throw away
                                // (`foreign_discarded`)?
                                embeddings_recomputed = report.embeddings_recomputed,
                                embeddings_deferred = report.embeddings_deferred,
                                foreign_discarded = report.foreign_embeddings_discarded,
                                "notes: ingest poller converged batch"
                            );
                        }
                        if report.embeddings_deferred > 0 {
                            // Not an error — the note IS stored and IS
                            // readable by keyword. But it is invisible
                            // to semantic recall until
                            // `backfill_tier_artifacts` runs, and that
                            // is a one-shot at daemon start, so a
                            // non-zero count here that persists means
                            // the embed slot is down.
                            tracing::warn!(
                                target = "notes",
                                deferred = report.embeddings_deferred,
                                "notes: remote notes stored without a local embedding; \
                                 excluded from semantic recall until the tier backfill \
                                 runs (next daemon start)"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            target = "notes",
                            error = %e,
                            "notes: ingest_remote_notes failed"
                        );
                    }
                }
            }
        }
    });
}

/// Spawn the lazy canonical-fingerprint stamper for legacy (pre-fingerprint) ingests.
pub(super) fn spawn_lazy_stamp_fingerprints(engine: Arc<CorpusEngine>) {
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

/// Spawn the tier-2 enrichment resume scan for unfinished workspaces after a restart.
pub(super) fn spawn_tier2_enrichment_resume(data_dir: &Path) {
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
pub(super) async fn advertise_embed_model(
    provider: Arc<dyn InferenceProvider>,
    config: &SetupConfig,
    resolved_embed_family: ModelFamily,
) -> sovereign_mesh::EmbedAdvertisement {
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
        return sovereign_mesh::EmbedAdvertisement::Unavailable { reason };
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
            sovereign_mesh::EmbedAdvertisement::Advertised(embed_info)
        }
        Err(e) => sovereign_mesh::EmbedAdvertisement::Unavailable {
            reason: format!("embed probe failed: {e}"),
        },
    }
}

/// Build the `/mcp` mount for the daemon's [`sovereign_mesh::ServingCapability`].
///
/// It used to also install the mesh, admin, reading and solve routers, the
/// provider factory and the setup config — six separate calls, each of which a
/// host could omit. The first four are now built by the daemon itself or
/// declared on the headless variant; only the tool mount is genuinely
/// host-specific, because only the host knows which tools it registered.
pub(super) fn build_mcp_surface(
    tools: ToolRegistry,
    notes_store: Arc<NoteStore>,
) -> sovereign_mesh::McpSurface {
    let session_id = format!("daemon-{}", uuid::Uuid::new_v4());
    sovereign_mesh::McpSurface::Mounted(sovereign_mesh::McpMount {
        tools: Arc::new(tools),
        notes: notes_store,
        session_id,
    })
}

/// Build the project-freshness reindexer (commit harvester + project/knowledge-view
/// HTTP routers), resume persisted projects, and return the reindexer handle to
/// hold for the daemon's life.
/// Returns the reindexer handle to hold for the daemon's life, plus the two
/// routers it owns — `/v1/projects/*` and
/// `POST /v1/knowledge/landscape_digest`. Both used to be installed onto the
/// daemon from in here; they are now returned so the caller can name them in
/// the daemon's variant, which is what makes "the daemon serves a knowledge
/// digest" a fact of the type rather than of whether this function ran.
pub(super) async fn start_freshness_pipeline(
    data_dir: &Path,
    notes_store: Arc<NoteStore>,
    engine: Arc<CorpusEngine>,
    provider: Arc<dyn InferenceProvider>,
    // The merged SCIP graph the MCP tools also hold. Shared (not rebuilt here)
    // so the reindexer's overlay + full-rebuild updates are live to `symbols()`.
    merged_handle: sovereign_mesh::reindexer::ScipGraphHandle,
) -> (
    Arc<sovereign_mesh::reindexer::Reindexer>,
    axum::Router,
    axum::Router,
) {
    // ── Project freshness pipeline ────────────────────────────────
    //
    // The Reindexer owns per-project FS watchers, git-HEAD pollers,
    // and the coalescing rebuild queue. Each registered project
    // gets one `ProjectHandle`; the daemon shells out to this
    // subsystem from HTTP (`/v1/projects/*`) rather than invoking
    // exporters synchronously. Persisted projects (loaded from
    // `~/.svrnmesh/projects.json`) are re-registered at startup
    // so a daemon restart resumes watching everything without a
    // user action.
    let freshness_indexes_dir = data_dir.join("indexes");
    // `merged_handle` is the SAME graph the tool registry holds (passed in by
    // the caller), so every rebuild/overlay update the reindexer makes is
    // immediately visible to `symbols`/`callers`/`blast`.
    let mut reindexer = sovereign_mesh::reindexer::Reindexer::new(
        freshness_indexes_dir.clone(),
        Arc::clone(&merged_handle),
    );
    // Phase 7.1: configure the commit-message harvester so the
    // reindexer's git-HEAD poll harvests non-noisy commits into
    // `source='committed'` notes. Must run BEFORE any clone /
    // share — Arc::get_mut returns None once this is shared.
    sovereign_mesh::reindexer::Reindexer::with_commit_harvester(
        &mut reindexer,
        Arc::clone(&notes_store),
    );
    let project_http = sovereign_mesh::project_http::project_router(Arc::clone(&reindexer));

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
    let inference_fn = sovereign_tools::corpus::inference_to_inference_fn(Arc::clone(&provider));
    let knowledge_view_manager = Arc::new(
        sovereign_tools::knowledge_view::KnowledgeViewManager::new(
            Arc::clone(&engine),
            inference_fn,
            knowledge_view_db_path,
            Vec::new(),
        )
        .await,
    );
    let knowledge_view_http = sovereign_mesh::landscape_digest_http::landscape_digest_router(
        Arc::clone(&knowledge_view_manager),
    );

    // Resume any previously-registered projects so FS watchers
    // come back up without the user running `project register`
    // again. Missing / unreadable registry is non-fatal — the
    // daemon runs happily with zero registered projects.
    let registry = sovereign_mesh::projects::Registry::load().unwrap_or_else(|e| {
        tracing::warn!(error = %e, "could not load project registry; starting empty");
        sovereign_mesh::projects::Registry::default()
    });
    for entry in registry.entries() {
        reindexer.register(entry.clone()).await;
        tracing::info!(corpus = %entry.corpus_id, "resumed registered project");
    }
    warn_orphaned_indexes(&freshness_indexes_dir, &registry);
    (reindexer, project_http, knowledge_view_http)
}

/// Build the watched-folder reconciliation subsystem (LocalCorpusManager +
/// enrichment defaults + tiered deps) and spawn its scheduler; returns the held
/// subsystem handle.
pub(super) async fn setup_watched_folders(
    engine: Arc<CorpusEngine>,
    state_store: Arc<dyn sovereign_core::traits::StateStore>,
    data_dir: &Path,
    config: &SetupConfig,
    folder_tiered_deps: Option<sovereign_tools::local_corpus::watched::enrich::TieredDeps>,
    atlas_builder: Option<
        Arc<dyn sovereign_tools::local_corpus::watched::enrich::AtlasBuildRunner>,
    >,
) -> Option<sovereign_mesh::watched_folder_setup::WatchedSubsystem> {
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
    // `sovereign_mesh::watched_folder_setup` so the desktop's
    // embedded daemon can call the same path.
    // Critical: pass the same `recipes_dir` the `CorpusEngine`
    // was constructed with (see the `let recipes_dir = …` block
    // above where the engine is built). Otherwise the manager
    // writes its generated recipe TOMLs into a directory the
    // engine never reads from, and the first sweep's apply step
    // errors `No registry entry for corpus '<id>'`.
    let lc_recipes_dir = data_dir.join("recipes");
    match sovereign_tools::local_corpus::LocalCorpusManager::init_with_recipes_dir(
        Arc::clone(&engine),
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
            if let Some(deps) = folder_tiered_deps.clone() {
                manager.set_tiered_deps(deps).await;
                tracing::info!(
                    "watched_folder:tiered_deps_installed — \
                         enable_enrichment will route through the \
                         in-process tiered driver"
                );
            }
            // ontology-v1 P0.4 — `enrich_now` on an `[enrichment] type =
            // "atlas"` recipe runs the atlas orchestrator in THIS process.
            // Without this the driver falls back to spawning `sovereign-cli`,
            // which a shipped desktop bundle does not carry (exit 127).
            match atlas_builder {
                Some(builder) => {
                    manager.set_atlas_builder(builder).await;
                    tracing::info!(
                        "watched_folder:atlas_builder_installed — \
                             enrich_now runs atlas recipes in-process"
                    );
                }
                None => tracing::info!(
                    "watched_folder:atlas_builder_absent — \
                         atlas recipes will spawn `sovereign-cli enrich build`"
                ),
            }
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
                sovereign_mesh::watched_folder_setup::WatchedSubsystem::install(
                    Arc::clone(&engine),
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
pub(super) async fn build_mesh_provider(
    provider: Arc<dyn InferenceProvider>,
    daemon: Arc<sovereign_mesh::DeferredDaemon>,
) -> (
    Arc<sovereign_mesh::DeferredDaemon>,
    Arc<sovereign_mesh::peer_inference::MeshInferenceProvider>,
) {
    // Wrap the raw `EmbeddedLlamaCpp` in `MeshInferenceProvider`
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
        Arc::new(sovereign_mesh::pinned_worker_source::PinnedWorkerEndpointSource::new());
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
                     skipping (tear down with `svrn pipeline pod down {id}` \
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
                     (<4h remaining) — plan a fresh `pipeline pod up` if \
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
    let composite_source: Arc<dyn sovereign_mesh::peer_inference::PeerEndpointSource> = Arc::new(
        sovereign_mesh::pinned_worker_source::CompositeEndpointSource::new(
            Arc::clone(&daemon) as Arc<dyn sovereign_mesh::peer_inference::PeerEndpointSource>,
            Arc::clone(&pinned_source),
        ),
    );
    let mesh_provider = Arc::new(
        sovereign_mesh::peer_inference::MeshInferenceProvider::with_peer_source(
            Arc::clone(&provider),
            composite_source,
        ),
    );
    // A guest link this node accepted lets a granted model id resolve to the
    // LENDING node while the turn stays here. Wired at the COLD-START
    // assembly point, which is the whole reason this function exists: the
    // hot-reload factory in `daemon_cmd/provider.rs` had it and this did not,
    // so a freshly started daemon kept `NoGuestLenders` and the guest route
    // was dead until something happened to trigger a provider reload.
    // Observed live 2026-08-28: zero `guest-lender` lines in a daemon whose
    // `guest.json` was present and valid.
    mesh_provider.set_guest_source(Arc::new(
        sovereign_mesh::guest_lender::StoredGuestLink::new(),
    ));
    (daemon, mesh_provider)
}

/// Swap the real `MeshBroadcaster` into the deferred handle (peer fan-out) and
/// spawn the work-atlas TTL GC; returns the GC task handle to hold.
pub(super) fn finalize_work_atlas(
    daemon: Arc<EmbeddedDaemon>,
    work_atlas_broadcaster: Arc<sovereign_work_atlas::tools::DeferredBroadcaster>,
    work_atlas_store: Arc<sovereign_work_atlas::WorkAtlasStore>,
    work_atlas_cfg: sovereign_work_atlas::WorkAtlasConfig,
) -> tokio::task::JoinHandle<()> {
    // ── Work atlas (Phase 2) finalisation ─────────────────────────────
    //
    // 1. Swap the real `MeshBroadcaster` into the `DeferredBroadcaster`
    //    now that `daemon.app_state()` returns `Some`. After this,
    //    work-atlas writes broadcast to peers within the round-trip
    //    rather than waiting up to one full 10s gossip interval.
    // 2. Spawn the TTL eviction loop so expired claims and idle
    //    sessions are reaped on a 60s cadence.
    {
        let daemon_for_atlas = Arc::clone(&daemon);
        let broadcaster_for_atlas = Arc::clone(&work_atlas_broadcaster);
        tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
            loop {
                if let Some(state) = daemon_for_atlas.app_state().await {
                    let real: Box<dyn sovereign_work_atlas::tools::ClaimBroadcaster> =
                        Box::new(sovereign_mesh::MeshBroadcaster::new(state));
                    broadcaster_for_atlas.set(real);
                    tracing::info!("work_atlas: real broadcaster wired (peer fan-out active)");
                    return;
                }
                if tokio::time::Instant::now() >= deadline {
                    tracing::warn!(
                        "work_atlas: broadcaster wire-up timed out — \
                         claims/observations will only reach peers via the \
                         10s gossip round (still functional, just slower)"
                    );
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        });
    }
    sovereign_work_atlas::gc::WorkAtlasGc::new(Arc::clone(&work_atlas_store), work_atlas_cfg)
        .spawn()
}

/// Install the AppState foreground-yield hook on the lint/test watchers so their
/// cargo subprocesses back off under chat-slot memory pressure.
pub(super) fn install_foreground_yield_hook(
    daemon: Arc<EmbeddedDaemon>,
    lint_watcher: Option<Arc<corpus_engine_watchers::LintWatcher>>,
    test_watcher: Option<Arc<corpus_engine_watchers::TestWatcher>>,
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
    // spawning cargo. Late-bind: daemon_cmd builds the watchers
    // earlier in this function (before EmbeddedDaemon exists);
    // `daemon.app_state()` returns Some only after start_daemon
    // completes. Poll with the same deadline pattern as the
    // work-atlas broadcaster wire-up above.
    if lint_watcher.is_some() || test_watcher.is_some() {
        let daemon_for_hook = Arc::clone(&daemon);
        let lint_for_hook = lint_watcher.clone();
        let test_for_hook = test_watcher.clone();
        tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
            loop {
                if let Some(hook) = daemon_for_hook.build_yield_hook().await {
                    if let Some(w) = lint_for_hook.as_ref() {
                        w.set_yield_hook(Arc::clone(&hook));
                        tracing::info!("foreground-yield: hook installed on lint watcher");
                    }
                    if let Some(w) = test_for_hook.as_ref() {
                        w.set_yield_hook(Arc::clone(&hook));
                        tracing::info!("foreground-yield: hook installed on test watcher");
                    }
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
pub(super) fn write_pidfile() -> (std::path::PathBuf, u32) {
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

/// Bundle of every handle the workspace watchers + Phase-2 work-atlas setup
/// produces. Destructured at the call site back into locals, so the rest of the
/// bootstrap reads unchanged.
pub(super) struct WatcherAtlasSetup {
    pub(super) watcher_heartbeat: Arc<corpus_engine_watchers::WatcherHeartbeat>,
    pub(super) lint_watcher: Option<Arc<corpus_engine_watchers::LintWatcher>>,
    pub(super) test_watcher: Option<Arc<corpus_engine_watchers::TestWatcher>>,
    pub(super) watched_lint_scope: Option<String>,
    pub(super) watched_test_scope: Option<String>,
    pub(super) watcher_monitor: Option<tokio::task::JoinHandle<()>>,
    pub(super) work_atlas_mesh_store: Arc<commonwealth_state::MeshStore>,
    pub(super) work_atlas_store: Arc<sovereign_work_atlas::WorkAtlasStore>,
    pub(super) work_atlas_broadcaster: Arc<sovereign_work_atlas::tools::DeferredBroadcaster>,
    pub(super) work_atlas_cfg: sovereign_work_atlas::WorkAtlasConfig,
    pub(super) work_atlas_repo_root: Option<PathBuf>,
    pub(super) work_atlas_repo_id: Option<String>,
    pub(super) work_atlas_branch: Option<String>,
}

/// Resolve the workspace-driven lint/test watchers and the Phase-2 work-atlas
/// store/observer/supervisor. Returns every handle the rest of the bootstrap
/// needs as a [`WatcherAtlasSetup`] bundle.
pub(super) fn setup_watchers_and_work_atlas(
    workspace_dir: &Option<PathBuf>,
    data_dir: &Path,
    lint_store: Arc<corpus_engine_watchers::LintResultStore>,
    test_store: Arc<corpus_engine_watchers::TestResultStore>,
) -> WatcherAtlasSetup {
    // Shared liveness beacon: the coordinator loop stamps it, the
    // status tools read it. Replaces the old one-shot `watcher_active`
    // bool, which could not detect a watcher that died after starting.
    // Mirrors to a sidecar file so the separate `sovereign` CLI process
    // (which reads the same SQLite stores) sees the same liveness — the
    // daemon's in-memory atomic alone is invisible cross-process.
    let watcher_heartbeat =
        corpus_engine_watchers::WatcherHeartbeat::with_sidecar(data_dir.join("watcher-heartbeat"));
    let mut lint_watcher: Option<Arc<corpus_engine_watchers::LintWatcher>> = None;
    let mut test_watcher: Option<Arc<corpus_engine_watchers::TestWatcher>> = None;
    let mut watched_lint_scope: Option<String> = None;
    let mut watched_test_scope: Option<String> = None;
    // Held for the lifetime of `start_daemon`. This is the
    // `WatcherSupervisor`'s monitor task: dropping it aborts the
    // monitor, which in turn drops the live coordinator handle and
    // shuts the watcher down. Underscored because we never read it back;
    // the value is the side effect of holding the task alive.
    let mut _watcher_monitor: Option<tokio::task::JoinHandle<()>> = None;

    // ── Work atlas wiring (Phase 2) ────────────────────────────────────
    // Single shared `Arc<MeshStore>`: handed into the EmbeddedDaemon
    // via `set_mesh_store` so `AppState.inner.mesh_store` IS this
    // instance, and also handed into the `WorkAtlasStore` so claims
    // and observations land in the same store gossip publishes from.
    // In-memory is intentional — matches the daemon's existing
    // long-term-persistence-via-mesh.json design. The atlas-relevant
    // records have TTLs measured in hours; restart cost is acceptable.
    let work_atlas_mesh_store: Arc<commonwealth_state::MeshStore> = Arc::new(
        commonwealth_state::MeshStore::in_memory().expect("in-memory MeshStore for work atlas"),
    );
    // Node identity — same resolution order EmbeddedDaemon uses when
    // it starts (file-on-disk → mesh.json → generate). Resolved early
    // so `WorkAtlasStore::node_id` matches the daemon's `self_id`.
    let work_atlas_node_id = resolve_self_node_id(data_dir);
    // Measurement history is durable on disk but the buffer above is not; without
    // this, everything this node has measured leaves the mesh at every restart.
    // Runs before anything can gossip, so a peer never sees a momentarily empty
    // namespace and concludes we have measured nothing.
    republish_local_measurements(&work_atlas_mesh_store, work_atlas_node_id);
    let work_atlas_store = Arc::new(sovereign_work_atlas::WorkAtlasStore::new(
        Arc::clone(&work_atlas_mesh_store),
        work_atlas_node_id,
    ));
    // Deferred broadcaster — `MeshBroadcaster` needs `AppState`, which
    // isn't reachable until `daemon.try_resume()`. The MCP tools and
    // the AtlasObserver hold this handle now; we swap the real
    // broadcaster in once `app_state` is available.
    let work_atlas_broadcaster = Arc::new(sovereign_work_atlas::tools::DeferredBroadcaster::new());
    let work_atlas_cfg = {
        let path = sovereign_core::rebrand::work_atlas_toml();
        sovereign_work_atlas::WorkAtlasConfig::load_or_default(&path).unwrap_or_else(|e| {
            tracing::warn!(
                error = %e,
                path = %path.display(),
                "work_atlas: config load failed, using defaults"
            );
            sovereign_work_atlas::WorkAtlasConfig::defaults()
        })
    };
    // Resolved later if the workspace has an `origin` remote.
    let mut work_atlas_observer: Option<Arc<sovereign_work_atlas::AtlasObserver>> = None;
    let mut work_atlas_repo_root: Option<std::path::PathBuf> = None;
    let mut work_atlas_repo_id: Option<String> = None;
    let mut work_atlas_branch: Option<String> = None;

    if let Some(ref ws) = workspace_dir {
        let sov_cfg = corpus_engine::SovereignConfig::load_or_default(&ws.join(".sovereign"));
        // Single-permit semaphore shared by the lint + test watchers so
        // their cargo subprocesses serialize instead of compounding
        // memory pressure. Without this, both fire concurrent cargo
        // check / cargo test invocations on every debounced edit
        // flush, doubling RSS and inviting macOS to SIGTERM the daemon
        // under pressure.
        let run_slot = Arc::new(tokio::sync::Semaphore::new(1));

        if let Some(ref cfg) = sov_cfg.lint_runner {
            let working_dir = cfg.working_dir.as_ref().map(|d| {
                let p = PathBuf::from(d);
                if p.is_absolute() {
                    p
                } else {
                    ws.join(p)
                }
            });
            watched_lint_scope = Some(cfg.command.clone());
            lint_watcher = Some(Arc::new(
                corpus_engine_watchers::LintWatcher::new(
                    &cfg.command,
                    working_dir,
                    cfg.timeout_secs.unwrap_or(120),
                    Arc::clone(&lint_store),
                )
                .with_run_slot(Arc::clone(&run_slot)),
            ));
            tracing::info!(
                command = %cfg.command,
                workspace = %ws.display(),
                "lint watcher configured (shared run slot)"
            );
        }
        if let Some(ref cfg) = sov_cfg.test_runner {
            let working_dir = cfg.working_dir.as_ref().map(|d| {
                let p = PathBuf::from(d);
                if p.is_absolute() {
                    p
                } else {
                    ws.join(p)
                }
            });
            watched_test_scope = Some(cfg.command.clone());
            test_watcher = Some(Arc::new(
                corpus_engine_watchers::TestWatcher::new(
                    &cfg.command,
                    working_dir,
                    cfg.timeout_secs.unwrap_or(300),
                    Arc::clone(&test_store),
                )
                .with_run_slot(Arc::clone(&run_slot)),
            ));
            tracing::info!(
                command = %cfg.command,
                workspace = %ws.display(),
                "test watcher configured (shared run slot)"
            );
        }

        // Work-atlas observer (Phase 2). Needs a `repo_id` to scope
        // observations to. An `origin` remote yields the cross-node id;
        // without one the workspace gets a machine-local id instead of being
        // dropped, so a housemate's own project still gets an atlas — the
        // observations simply do not travel, which is the truth about a repo
        // no peer can name. Only "not a git repo at all" leaves the observer
        // unwired.
        match sovereign_work_atlas::resolve_repo_id_allowing_local(ws) {
            Ok((repo_root, repo_id, _source)) => {
                let branch = sovereign_cli_shared::repo::current_branch(&repo_root);
                let observer = Arc::new(sovereign_work_atlas::AtlasObserver::new(
                    Arc::clone(&work_atlas_store),
                    work_atlas_cfg.clone(),
                    Arc::clone(&work_atlas_broadcaster)
                        as Arc<dyn sovereign_work_atlas::tools::ClaimBroadcaster>,
                    repo_root.clone(),
                    repo_id.clone(),
                    branch.clone(),
                ));
                work_atlas_observer = Some(Arc::clone(&observer));
                work_atlas_repo_root = Some(repo_root);
                work_atlas_repo_id = Some(repo_id);
                work_atlas_branch = branch;
                eprintln!("svrn daemon: work-atlas observer wired on {}", ws.display());
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    workspace = %ws.display(),
                    "work_atlas:repo_id_missing — atlas observer disabled (no origin remote)"
                );
            }
        }

        if lint_watcher.is_some() || test_watcher.is_some() || work_atlas_observer.is_some() {
            let debounce_ms = sov_cfg
                .lint_runner
                .as_ref()
                .and_then(|c| c.debounce_ms)
                .or_else(|| sov_cfg.test_runner.as_ref().and_then(|c| c.debounce_ms))
                .unwrap_or(800);

            // Collect the registered watchers once; the supervisor holds
            // them so it can rebuild the coordinator on restart without
            // re-deriving anything.
            let mut watchers: Vec<Arc<dyn corpus_engine_watchers::BackgroundWatcher>> = Vec::new();
            if let Some(ref w) = lint_watcher {
                watchers.push(Arc::clone(w) as Arc<dyn corpus_engine_watchers::BackgroundWatcher>);
            }
            if let Some(ref w) = test_watcher {
                watchers.push(Arc::clone(w) as Arc<dyn corpus_engine_watchers::BackgroundWatcher>);
            }
            if let Some(ref obs) = work_atlas_observer {
                watchers
                    .push(Arc::clone(obs) as Arc<dyn corpus_engine_watchers::BackgroundWatcher>);
            }

            // The supervisor performs the initial start AND self-heals:
            // if the coordinator loop dies or its heartbeat freezes, it
            // rebuilds and restarts (bounded backoff). Holding the monitor
            // task handle keeps the watcher alive for the daemon's life.
            let supervisor = crate::watcher_supervisor::WatcherSupervisor::new(
                watchers,
                vec![ws.clone()],
                debounce_ms,
                Arc::clone(&watcher_heartbeat),
            );
            _watcher_monitor = supervisor.spawn();
            if _watcher_monitor.is_some() {
                eprintln!(
                    "svrn daemon: watcher supervisor live on {} (self-healing)",
                    ws.display()
                );
            }
        }
    } else {
        tracing::debug!(
            "no workspace resolved (set SOVEREIGN_WORKSPACE_DIR or write \
             ~/.svrnmesh/workspace) — lint/test watcher disabled"
        );
    }
    WatcherAtlasSetup {
        watcher_heartbeat,
        lint_watcher,
        test_watcher,
        watched_lint_scope,
        watched_test_scope,
        watcher_monitor: _watcher_monitor,
        work_atlas_mesh_store,
        work_atlas_store,
        work_atlas_broadcaster,
        work_atlas_cfg,
        work_atlas_repo_root,
        work_atlas_repo_id,
        work_atlas_branch,
    }
}

#[cfg(test)]
mod rpc_worker_flag_tests {
    use super::{rpc_worker_flag, DEFAULT_RPC_BIND};

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn absent_means_this_node_lends_nothing() {
        assert_eq!(rpc_worker_flag(&args(&["run"])), None);
        assert_eq!(rpc_worker_flag(&args(&[])), None);
    }

    #[test]
    fn bare_flag_takes_the_documented_default() {
        assert_eq!(
            rpc_worker_flag(&args(&["run", "--rpc-worker"])).as_deref(),
            Some(DEFAULT_RPC_BIND)
        );
    }

    #[test]
    fn an_explicit_bind_is_honoured_in_both_spellings() {
        for a in [
            args(&["--rpc-worker=10.0.0.4:9999"]),
            args(&["--rpc-worker", "10.0.0.4:9999"]),
        ] {
            assert_eq!(rpc_worker_flag(&a).as_deref(), Some("10.0.0.4:9999"));
        }
    }

    /// The space form must not swallow the next flag as a bind address.
    /// `serve_rpc_worker_if_configured` would then try to bind `--setup-only`,
    /// fail, and leave the operator with a daemon that quietly lends nothing.
    #[test]
    fn a_following_flag_is_not_mistaken_for_a_bind() {
        assert_eq!(
            rpc_worker_flag(&args(&["--rpc-worker", "--setup-only"])).as_deref(),
            Some(DEFAULT_RPC_BIND)
        );
    }

    /// `--rpc-worker=` is a typo. Serving on the empty string is silently a
    /// no-op inside ggml, so treat it as the default rather than as consent to
    /// do nothing.
    #[test]
    fn an_empty_bind_falls_back_rather_than_serving_nothing() {
        assert_eq!(
            rpc_worker_flag(&args(&["--rpc-worker="])).as_deref(),
            Some(DEFAULT_RPC_BIND)
        );
    }

    /// The flag has to survive the trip to the detached child `daemon start`
    /// spawns, which re-parses it from the `--rpc-worker=<bind>` form.
    #[test]
    fn the_forwarded_form_round_trips() {
        let bind =
            rpc_worker_flag(&args(&["--rpc-worker", "192.168.1.2:50052"])).expect("parsed once");
        let forwarded = args(&["run", &format!("--rpc-worker={bind}")]);
        assert_eq!(rpc_worker_flag(&forwarded), Some(bind));
    }
}

#[cfg(test)]
mod advertise_tests {
    use super::*;
    use sovereign_core::setup_config::SetupConfig;
    use sovereign_core::traits::InferenceProvider;
    use sovereign_core::types::{CompletionRequest, CompletionResponse, Depth, Speed};

    /// A provider whose embed probe SUCCEEDS while owning no weights — the
    /// terminal's actual shape, since `SplitInferenceProvider` forwards the
    /// probe to the entry node and returns a perfectly good vector.
    ///
    /// This is the whole point of the test: the probe cannot be the thing that
    /// decides whether to advertise, because on a terminal it answers a
    /// question about somebody else's machine.
    struct ForwardingProbe;

    #[async_trait::async_trait]
    impl InferenceProvider for ForwardingProbe {
        async fn complete(
            &self,
            _request: &CompletionRequest,
        ) -> sovereign_core::error::Result<CompletionResponse> {
            unreachable!("advertise_embed_model never completes a turn")
        }

        async fn complete_stream(
            &self,
            _request: &CompletionRequest,
        ) -> sovereign_core::error::Result<
            std::pin::Pin<
                Box<
                    dyn futures::Stream<Item = sovereign_core::error::Result<String>>
                        + Send
                        + 'static,
                >,
            >,
        > {
            unreachable!("advertise_embed_model never streams")
        }

        async fn embed(&self, _text: &str) -> sovereign_core::error::Result<Vec<f32>> {
            Ok(vec![0.0; 1024])
        }

        fn capabilities(&self) -> sovereign_core::types::ProviderCapabilities {
            sovereign_core::types::ProviderCapabilities {
                max_context_tokens: 4096,
                supports_structured_output: false,
                relative_speed: Speed::Fast,
                relative_reasoning: Depth::Moderate,
            }
        }
    }

    /// A terminal advertises NO embed model, even though its probe answers.
    ///
    /// Before the guard, `model_id` was resolved as `stem → entry_embed_model`,
    /// so this node published its ENTRY node's model as its own capability —
    /// and `capabilities.rs` filters collaborative-ingestion candidates by
    /// exact match on that field, so the planner would partition chunks onto a
    /// machine that can only proxy each one back to the node it was spreading
    /// load off (§18.3).
    #[tokio::test]
    async fn a_terminal_advertises_no_embed_model_even_though_its_probe_answers() {
        let mut cfg = SetupConfig::unconfigured();
        cfg.node.entry = Some("http://halo:9741/v1".into());
        cfg.node.entry_embed_model = Some("qwen3-embedding-0.6b".into());

        let ad = advertise_embed_model(Arc::new(ForwardingProbe), &cfg, ModelFamily::Unknown).await;

        assert!(
            ad.info().is_none(),
            "a terminal published an embed capability it does not hold: {:?}",
            ad.info().map(|i| i.model_id.clone()),
        );
        match ad {
            sovereign_mesh::EmbedAdvertisement::Unavailable { reason } => assert!(
                reason.contains("terminal") && reason.contains("halo"),
                "the absence must say WHY and name the entry node, got: {reason}"
            ),
            sovereign_mesh::EmbedAdvertisement::Advertised(_) => {
                unreachable!("asserted absent above")
            }
        }
    }
}
