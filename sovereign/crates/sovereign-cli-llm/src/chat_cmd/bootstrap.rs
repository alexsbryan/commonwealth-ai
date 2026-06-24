// SPDX-License-Identifier: AGPL-3.0-or-later
//! Daemon-backed Runtime bootstrap for `sovereign chat`.
//!
//! Mirrors `sovereign-desktop::state::bootstrap` — same StateStore,
//! CorpusEngine, tools, mesh-knowledge wiring — but the
//! `InferenceProvider` is a `SplitInferenceProvider` that delegates
//! chat completions to the daemon's chat model and embeddings to the
//! daemon's embed model over HTTP. No embedded llama.cpp, no Tauri.
//!
//! Rationale
//! ---------
//! The desktop's Attach mode is the architectural template we want:
//! "the daemon already owns the model, talk to it over HTTP". The
//! desktop currently still loads local weights even in Attach mode
//! (historical quirk); this CLI does what Attach *should* do — pure
//! HTTP.
//!
//! The split-provider dance is required because `RemoteApiProvider`
//! uses a single `model_id` for both `/chat/completions` AND
//! `/embeddings`. Sending a chat model to the embeddings endpoint
//! returns non-embedding shapes (or errors). We keep two instances
//! and route by method.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use sovereign_core::error::{Error, Result};
use sovereign_core::planner::LlmPlanner;
use sovereign_core::runtime::Runtime;
use sovereign_core::traits::{ApprovalChannel, InferenceProvider, StateStore};
use sovereign_core::types::*;
use sovereign_core::{SkillRegistry, ToolRegistry};
// Re-exported (not just `use`d) so the other CLI modules that referenced the
// formerly-local `chat_cmd::bootstrap::SplitInferenceProvider` (raptor,
// recipe_cmd) keep resolving after it was promoted to sovereign-inference.
pub use sovereign_inference::remote::SplitInferenceProvider;
use sovereign_store::sqlite::SqliteStateStore;
use sovereign_tools::shell::ShellTool;

use crate::chat_cmd::config::ChatGlobals;

/// Bundle of everything the chat subcommands need from bootstrap.
/// Carries `Arc<Runtime>` plus the handles required to persist turns
/// (the store) and browse prior conversations.
pub struct ChatSession {
    pub runtime: Arc<Runtime>,
    pub store: Arc<dyn StateStore>,
    pub corpus_engine: Arc<corpus_engine::CorpusEngine>,
    pub inference: Arc<dyn InferenceProvider>,
    pub daemon_base: String,
    /// Resolved embed model id (e.g. `Qwen3-Embedding-0.6B-Q8_0`).
    /// Surfaced so cache layers (atlas embeddings, future per-corpus
    /// vector caches) can key on the active model and invalidate when
    /// the operator swaps it.
    pub embed_model: String,
    /// The per-process atlas-grounding manager (the same Arc installed
    /// on `runtime` as its `AtlasContextProvider`). Exposed so a
    /// measurement harness can `warm_one(corpus)` its sealed corpus —
    /// `build_session` only loads already-cached atlases
    /// (`init_from_cache`), so a freshly-enriched corpus contributes 0
    /// contexts until something warms it. Warming this Arc is visible to
    /// `runtime` because they share it.
    pub atlas_mgr: Arc<sovereign_tools::atlas_context_manager::AtlasContextManager>,
}

/// Build a `Runtime` backed by the daemon over HTTP.
///
/// Fails fast if the daemon isn't answering — there's no recovery
/// path a retry could fix, and a partially-initialized Runtime
/// pointing at a dead endpoint would produce confusing errors deep
/// in retrieval. The caller should exit with a hint.
pub async fn build_session(globals: &ChatGlobals) -> Result<ChatSession> {
    build_session_with_skills(globals, SkillRegistry::new()).await
}

/// Build a daemon-backed `ChatSession` with a caller-supplied
/// `SkillRegistry`. The default `build_session` passes an empty
/// registry — chat-as-chat doesn't need skills loaded. The Tier-B
/// voice eval harness (`sovereign voice eval`) supplies a registry
/// pre-populated with the relational skills (inner-work,
/// personal-assistant) and pre-activates the per-scenario one so
/// the runtime's `primary_skill_register()` resolves to
/// `Relational` and the witness-voice contract gets prepended.
pub async fn build_session_with_skills(
    globals: &ChatGlobals,
    skills: SkillRegistry,
) -> Result<ChatSession> {
    // 1. Probe the daemon before we touch anything else. A fast fail
    //    here prints a clean "start the daemon" message instead of
    //    the cryptic timeout from the first real request.
    let base = globals.daemon_base.clone();
    let v1 = format!("{base}/v1");
    probe_or_bail(&base).await?;

    // 2. Resolve model IDs. Preference order:
    //       a) explicit `--chat-model` / `--embed-model` flag,
    //       b) the daemon's `SetupConfig.models.*` filename stems
    //          — this is what the daemon actually loaded, and the
    //          daemon advertises those IDs on `/v1/models`,
    //       c) fallback: probe `/v1/models` and pick the first
    //          chat- and first embed-shaped entries.
    //    The historical (c)-only path picked non-deterministically
    //    between a locally-loaded `qwen-embedding-0.6b` (1024-dim)
    //    and a mesh-peer-advertised `Qwen3-Embedding-0.6B-Q8_0`
    //    whose dimensionality didn't match any installed corpus —
    //    silently downgrading every retrieval to FTS-only. Reading
    //    the config directly removes that race.
    let (chat_model, embed_model) = resolve_model_ids(&v1, globals).await?;
    eprintln!("Daemon: {base}");
    eprintln!("Chat model:  {chat_model}");
    eprintln!("Embed model: {embed_model}");

    let inference: Arc<dyn InferenceProvider> = Arc::new(SplitInferenceProvider::new(
        &v1,
        chat_model,
        embed_model.clone(),
        // Matches the RemoteApiProvider default from the desktop
        // Attach path. `Runtime` consumers read this via
        // `capabilities().max_context_tokens`; for today's models
        // this is approximate but non-blocking.
        8192,
    ));

    // 3. Open the state store. Creating the data dir on the fly is
    //    safe — mirrors the desktop's behaviour and means a first
    //    `sovereign chat` against a fresh home directory doesn't
    //    stumble on a missing folder.
    std::fs::create_dir_all(&globals.data_dir)
        .map_err(|e| Error::Serialization(format!("create {:?}: {e}", globals.data_dir)))?;
    let db_path = globals.data_dir.join("sovereign.db");
    eprintln!("Database:    {}", db_path.display());
    let store_concrete = Arc::new(
        SqliteStateStore::open(&db_path)
            .map_err(|e| Error::Serialization(format!("open db {:?}: {e}", db_path)))?,
    );
    let store: Arc<dyn StateStore> = store_concrete.clone();

    // 4. Build the CorpusEngine. The desktop (`state.rs:706-707`) and
    //    the legacy REPL (`main.rs:477-478`) both hardcode
    //    `~/.sovereign/{recipes,indexes}` regardless of
    //    `config.data.dir` — that field governs the state DB only,
    //    not corpus storage. Matching that convention means this CLI
    //    sees the same corpora the desktop just ingested.
    //
    //    If a user passed `--data-dir` explicitly they almost
    //    certainly meant to override BOTH paths; honour that by
    //    using `<data_dir>/indexes` when `--data-dir` was given.
    //    Otherwise stick to the hardcoded well-known path.
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let dotsovereign = home.join(".sovereign");
    let (recipes_dir, indexes_dir): (PathBuf, PathBuf) = if globals.data_dir_explicit {
        (
            globals.data_dir.join("recipes"),
            globals.data_dir.join("indexes"),
        )
    } else {
        (dotsovereign.join("recipes"), dotsovereign.join("indexes"))
    };
    eprintln!("Indexes:     {}", indexes_dir.display());
    let embed_fn = sovereign_tools::corpus::inference_to_embed_fn(Arc::clone(&inference));
    let inference_fn = sovereign_tools::corpus::inference_to_inference_fn(Arc::clone(&inference));
    // The engine's `expected_embedding_model` flows into
    // `_corpus_meta.json` at ingest time and into shard-consistency
    // checks. The CLI doesn't ingest during chat, but if any tool
    // path later triggers an ingest (e.g. watcher-driven reindex
    // through the same engine), it must match what the desktop
    // would have written. We've already resolved `embed_model` from
    // SetupConfig above.
    let corpus_engine = Arc::new(
        corpus_engine::CorpusEngine::new(recipes_dir, indexes_dir.clone(), embed_fn)
            .with_embedding_model(&embed_model)
            .with_inference_fn(inference_fn),
    );
    log_installed_corpora(&corpus_engine).await;

    // 5. Skills — empty registry is fine for chat; the runtime uses
    //    them to prefix system prompts with skill descriptors, and
    //    the chat flow is identical under "no active skill".
    let skills = Arc::new(skills);

    // 6. Tools. Keep this list identical to the desktop bootstrap so
    //    the retrieval + tool-use path exercised here matches what
    //    the user sees in the GUI. Notably: `SearchTool::with_web`
    //    drives the "Searched ... web" sources in provenance.
    // Tier 4 — shared tool-result cache. Same shape as the
    // desktop bootstrap: per-conversation cache slices, 5-turn
    // TTL. Idempotent tools (knowledge_lookup, code-intel reads)
    // hit the cache when the model re-calls with the same args
    // within the window.
    let tool_cache = Arc::new(sovereign_core::tool_result_cache::ToolResultCache::new());
    let mut tools = ToolRegistry::new().with_cache(Arc::clone(&tool_cache));
    tools.register(Box::new(ShellTool));
    tools.register(Box::new(sovereign_tools::document::DocumentTool::new(
        Arc::clone(&store),
        Arc::clone(&inference),
    )));
    tools.register(Box::new(sovereign_tools::ClaimSearchTool::new(Arc::clone(
        &corpus_engine,
    ))));
    tools.register(Box::new(sovereign_tools::EpistemicLandscapeTool::new(
        Arc::clone(&corpus_engine),
    )));
    // Deterministic land-value-tax analytics over parcel corpora
    // (e.g. sf-assessor-roll) — pre-cited figures the ComplexTask
    // synthesizer quotes verbatim ("no confabulated numbers").
    tools.register(Box::new(
        sovereign_tools::parcel_analytics::ParcelAnalyticsTool::new(Arc::clone(&corpus_engine)),
    ));
    // Code-intelligence tools previously registered here against an
    // in-memory stub ScipGraph. Dropped 2026-05-22 along with the
    // REPL's treesitter dep — real SCIP queries go through
    // `sovereign daemon` (sovereign-cli-atos), which builds the
    // merged graph from ~/.sovereign/indexes/*/scip_graph.db.
    tools.register(Box::new(sovereign_tools::search::SearchTool::with_web(
        Arc::clone(&store),
        Arc::clone(&inference),
        // DuckDuckGo — free, no key required. Matches the no-API-key
        // fallback in main.rs for parity with the legacy REPL.
        sovereign_tools::web::search::SearchBackend::DuckDuckGo,
    )));
    // Unified knowledge-lookup front door (Tool-Mastery framework
    // Phase 5). Returns a single Evidence envelope across corpus
    // + memory + note channels. The plan migrates skills onto
    // this tool as a follow-up PR; for now it coexists with
    // `search` / `knowledge` / `claim_search`. Note: the
    // NoteStore handle is wired later (after Runtime build); we
    // register the tool here and re-register with notes once we
    // have the store. For now, register without notes — the
    // single-turn knowledge-gym mocks the tool client-side so
    // production daemon-side notes channel isn't load-bearing
    // for the gym, and the threads bench doesn't drive
    // knowledge_lookup directly anyway.
    tools.register(Box::new(sovereign_tools::KnowledgeLookupTool::new(
        Arc::clone(&store),
        Arc::clone(&inference),
    )));
    tools.register(Box::new(sovereign_tools::web::WebFetchTool::new()));
    tools.register(Box::new(sovereign_tools::WikipediaFetchTool::new(
        Arc::clone(&corpus_engine),
    )));
    // `attached_doc_search` is registered unconditionally; the
    // execute() path returns a clear "no document attached" payload
    // on conversations without a DocumentSession, so the model can
    // probe it harmlessly. When a doc IS attached, the runtime's
    // ReasonWithTools loop can call it directly — that's the lever
    // the book-report bench exposed as missing (sovereign decision
    // 7693f16b: attached docs as Tool, not parallel pipeline).
    tools.register(Box::new(sovereign_tools::AttachedDocumentSearchTool::new(
        Arc::clone(&store),
        Arc::clone(&inference),
    )));

    // External MCP servers (the `[[mcp_servers]]` array of the canonical
    // config): connect over HTTP and register their tools into the SAME
    // registry the agent plans against, so a server added via `sovereign mcp
    // add` or the desktop settings pane is callable here too. One shared
    // loader, every surface — parity with the router stack below. The manager
    // is held only for connection statuses (logged); the live transports are
    // owned by the registered tools in the registry.
    let mcp = sovereign_tools::mcp::load_from_setup_config(&mut tools).await;
    for st in mcp.server_statuses().await {
        if st.connected {
            eprintln!("MCP:         {} ({} tools)", st.name, st.tool_count);
        } else if let Some(e) = &st.error {
            eprintln!("MCP:         {} unavailable — {e}", st.name);
        }
    }

    eprintln!("Tools:       {} registered", tools.count());

    // 7. Router + planner. The legacy REPL defaults to
    //    `PassthroughRouter`; the desktop uses LLM-based routing.
    //    Use the LLM router here so the chat flow is bit-for-bit
    //    identical to the desktop surface — the point of the CLI is
    //    to reproduce that flow, not a simplified version.
    //
    //    Embed-router pre-check: when the exemplar TOML is reachable
    //    (default `sovereign/router/exemplars.toml` or
    //    `$SOVEREIGN_ROUTER_EXEMPLARS`), load it and pre-embed every
    //    exemplar. Subsequent routing decisions consult the embed
    //    classifier before the heuristic + LLM cascade. Falls through
    //    to the legacy stack on load failure or low-confidence
    //    classifications.
    // Router classifier stack — built through the shared `router_bootstrap`
    // helper so the CLI/bench, desktop, and served daemon all wire the SAME
    // classifiers (parity by construction; see sovereign-core/router_bootstrap.rs).
    // `from_env_and_repo` keeps the `$SOVEREIGN_*` overlay + repo-relative
    // exemplars for dev tuning; a packaged build falls through to the baked set.
    let (llm_router, router_report) = sovereign_core::router_bootstrap::build_llm_router(
        Arc::clone(&inference),
        Arc::clone(&store),
        Arc::clone(&skills),
        &sovereign_core::router_bootstrap::ExemplarOverrides::from_env_and_repo(),
    )
    .await;
    eprintln!(
        "Router classifier stack: embed={} scope={} effort={} current_info={}",
        router_report.embed_router.is_some(),
        router_report.scope.is_some(),
        router_report.effort.is_some(),
        router_report.current_info.is_some(),
    );
    let router: Box<dyn sovereign_core::traits::Router> = Box::new(llm_router);
    let planner = LlmPlanner::new(Arc::clone(&inference), Arc::clone(&skills));

    // 8. Approval channel. Chat turns don't trigger confirmations
    //    in the normal path; we wire a yes-only stub so any stray
    //    approval request is auto-granted rather than deadlocking a
    //    one-shot CLI.
    let approval: Arc<dyn ApprovalChannel> = Arc::new(AutoApprove);

    // 9. Mesh knowledge client. Talks to the daemon's `/v1/mesh` —
    //    when no mesh is running, reqwest gets ECONNREFUSED on the
    //    first call and the Runtime falls through to local-only
    //    retrieval. Safe to install unconditionally (same policy as
    //    the desktop).
    let mesh_knowledge: Option<Arc<dyn sovereign_core::traits::MeshKnowledgeSource>> =
        match sovereign_mesh::knowledge_client::MeshKnowledgeClient::new(&base) {
            Ok(c) => Some(Arc::new(c)),
            Err(_) => None,
        };

    // 10. Runtime. Only the fields we need — routing events stay at
    //     the no-op default (the CLI has no UI to emit to), and
    //     landscape-digest / KnowledgeView is intentionally omitted
    //     (desktop feature, not load-bearing for chat correctness).
    let mut inference_config = InferenceConfig::default();
    if let Some(t) = globals.temperature {
        inference_config.temperature = t;
        eprintln!("Temperature: {t} (override)");
    }
    if let Some(n) = globals.max_tokens {
        inference_config.max_tokens = n;
        eprintln!("Max tokens: {n} (override)");
    }
    // Session-level answering discipline (general persona layer). `govern
    // ask` sets this to its governance answering rules; ordinary chat
    // leaves it None (byte-identical prompt to before).
    if globals.custom_instructions.is_some() {
        inference_config.custom_instructions = globals.custom_instructions.clone();
    }
    // Tool-Mastery Layer 3 — NoteStore for the per-conversation
    // tool_decision write hook (runtime.rs handle_message_stream's
    // post-gap-check spawn). Same path the daemon uses
    // (`daemon_cmd.rs::build_tool_registry` → `data_dir.join("notes.db")`)
    // so the chat REPL and bench surfaces share one outcome log.
    let notes_path = globals.data_dir.join("notes.db");
    let notes_store = match corpus_engine_notes::NoteStore::open(&notes_path) {
        Ok(s) => Some(Arc::new(s)),
        Err(e) => {
            eprintln!(
                "warn: NoteStore open failed at {} ({e}); tool-decision \
                 writes will no-op this session",
                notes_path.display()
            );
            None
        }
    };

    let mut runtime = Runtime::new(
        Arc::clone(&inference),
        router,
        Box::new(planner),
        Arc::new(tools),
        Arc::clone(&store),
        skills,
        approval,
        inference_config,
    )
    .with_corpus_engine(Arc::clone(&corpus_engine));
    if let Some(ns) = notes_store.as_ref() {
        runtime = runtime.with_note_store(Arc::clone(ns));
    }
    // Conv-tiered briefing reader — same SqliteStateStore handle
    // already opened above also impls ConvTieredReader. Spec
    // `sovereign/docs/specs/CONV_TIERED_PORT.md`.
    runtime = runtime.with_conv_tiered_reader(
        Arc::clone(&store_concrete) as Arc<dyn sovereign_store::sqlite::ConvTieredReader>
    );
    if let Some(m) = mesh_knowledge {
        runtime = runtime.with_mesh_knowledge(m);
    }
    // GLiNER entity extractor for entity-aware retrieval-over-history
    // (`Runtime::maybe_retrieve_relevant_history`). Best-effort: probe
    // the default model id; if installed, load it and wire it onto
    // the Runtime. Failures soft-fall-through to pure cosine + MMR
    // — the bench/chat path keeps working without GLiNER.
    {
        let model_id = sovereign_tools::gliner_ner::DEFAULT_MODEL_ID;
        if sovereign_tools::gliner_ner::probe_model_available(model_id) {
            match sovereign_tools::gliner_ner::GlinerExtractor::new_default() {
                Ok(g) => {
                    let arc: Arc<dyn sovereign_core::traits::EntityExtractor> = Arc::new(g);
                    runtime = runtime.with_gliner(arc);
                    tracing::info!(
                        model = model_id,
                        "bootstrap: GLiNER entity extractor loaded"
                    );
                }
                Err(e) => {
                    tracing::warn!(error = %e, "bootstrap: GLiNER probe ok but load failed; entity-aware retrieval disabled");
                }
            }
        } else {
            tracing::debug!(
                model = model_id,
                "bootstrap: GLiNER model not installed; entity-aware retrieval disabled (falls back to cosine+MMR)"
            );
        }
    }
    // Atlas Layer 0: load any installed Wikipedia link graph. Probes
    // `<indexes_dir>/<corpus>/wikipedia_graph.db` for each installed
    // corpus and, on the first hit, wires it into the Runtime. Today
    // we expect at most one Wikipedia-class corpus per install — if
    // a future build needs multiple, switch this to a registry of
    // (corpus_id, Arc<WikipediaGraph>).
    if let Some(graph) = load_wikipedia_graph(&corpus_engine, &indexes_dir).await {
        eprintln!(
            "Wiki graph:  {} articles, {} edges",
            graph.article_count().await,
            graph.edge_count().await,
        );
        runtime = runtime.with_wikipedia_graph(graph);
    }

    // Atlas-grounded retrieval: build the per-process atlas context
    // manager, attach it to the Runtime, then synchronously load
    // every atlas whose embeddings are already cached on disk.
    // Cold-start embed work (uncached atlases) is intentionally NOT
    // done here — that belongs in the post-install hook so the
    // first user query has a deterministic latency and isn't gated
    // by a 40-min wiki-scale embed pass.
    let atlas_mgr = Arc::new(
        sovereign_tools::atlas_context_manager::AtlasContextManager::new(
            indexes_dir.clone(),
            Arc::clone(&inference),
            embed_model.clone(),
        ),
    );
    runtime =
        runtime
            .with_atlas_context_provider(Arc::clone(&atlas_mgr)
                as Arc<dyn sovereign_core::atlas_context::AtlasContextProvider>);
    atlas_mgr.init_from_cache().await;
    eprintln!(
        "Atlas: {} corpus context(s) loaded from cache",
        sovereign_core::atlas_context::AtlasContextProvider::loaded_corpus_ids(atlas_mgr.as_ref())
            .len()
    );
    // Adaptive triage (Phase B2): start the bump-flusher background
    // task so query-time hits eventually land on disk and feed the
    // next triage rebuild. 30s interval — losing up to half a
    // minute of bumps on a hard kill is acceptable for a statistical
    // signal.
    let _bump_flusher = Arc::clone(&atlas_mgr).spawn_bump_flusher(30);

    // Cross-corpus meta-atlas (Move 5). Loads
    // `~/.sovereign/meta-atlas/canonical_atoms.json` produced by
    // `sovereign meta-atlas build`. Empty / absent file → boost is a
    // no-op and retrieval falls back to cosine + existing
    // entity-boost. Operator can rebuild with the CLI; we don't auto-
    // build at chat boot (cost is non-trivial on a 1.6M-atom
    // wikipedia install).
    let meta_atlas_path = corpus_engine::meta_atlas::default_meta_atlas_path();
    let meta_atlas =
        match corpus_engine::meta_atlas::MetaAtlasIndex::load(meta_atlas_path.as_deref()) {
            Ok(idx) => Arc::new(idx),
            Err(e) => {
                eprintln!("Meta-atlas: load failed ({e}); boost disabled");
                Arc::new(corpus_engine::meta_atlas::MetaAtlasIndex::empty())
            }
        };
    eprintln!(
        "Meta-atlas:  {} canonical atoms across {} corpus(es)",
        meta_atlas.len(),
        meta_atlas.corpus_count(),
    );
    runtime = runtime.with_meta_atlas(Arc::clone(&meta_atlas));

    // Cross-corpus bridge edges (Phase 6). Loads
    // `~/.sovereign/meta-atlas/bridge_edges.json` produced by `sovereign
    // meta-atlas align`. Empty/absent → bridge_boost is a no-op; the
    // boost only runs at all when `SOVEREIGN_META_BRIDGE` is set.
    let bridge_index = match corpus_engine::meta_atlas::BridgeIndex::load(None) {
        Ok(idx) => Arc::new(idx),
        Err(e) => {
            eprintln!("Bridge: load failed ({e}); bridge boost disabled");
            Arc::new(corpus_engine::meta_atlas::BridgeIndex::empty())
        }
    };
    eprintln!("Bridge:      {} cross-corpus edges", bridge_index.len());
    runtime = runtime.with_bridge(Arc::clone(&bridge_index));

    // Optional cross-encoder reranker. When `SOVEREIGN_RERANK_MODEL_PATH`
    // is set, load that GGUF into a `StandaloneReranker` and wire it
    // into the Runtime. The reranker runs locally (the daemon-attached
    // `SplitInferenceProvider` doesn't support rerank), so this is
    // process-local additional weight (~500 MB for jina-reranker-v3-Q6_K).
    //
    // The candidate pool / threshold defaults come from
    // `RerankConfig::default()` (candidates_k = 50, no min_score) but
    // can be tuned per-run via `SOVEREIGN_RERANK_CANDIDATES_K` and
    // `SOVEREIGN_RERANK_MIN_SCORE` so an eval ablation can sweep them
    // without rebuilding.
    // Dedup-only ablation: `SOVEREIGN_RERANK_DEDUP_ONLY=1` enables
    // overfetch + per-article dedup using ONLY the fusion ordering.
    // Tests whether the SEP source-recall lift seen in the reranker
    // experiment is actually driven by the dedup mechanism or by the
    // cross-encoder logits — a critical question for the
    // "do we need a reranker slot at all?" decision in
    // `sovereign/docs/RERANK_EXPERIMENT.md`. Takes precedence over
    // `SOVEREIGN_RERANK_MODEL_PATH` so the operator can A/B without
    // touching two env vars at once.
    let dedup_only = std::env::var("SOVEREIGN_RERANK_DEDUP_ONLY")
        .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    // Per-corpus dedup allowlist. Empirically (RERANK_EXPERIMENT.md):
    // SEP gains +10 sources from dedup; wiki LOSES 3. Default is
    // SEP-only when nothing is set, but the operator can override.
    // Empty string ("") explicitly = no filter (apply to all corpora;
    // matches the original cross-corpus ablation behaviour).
    let dedup_filter = match std::env::var("SOVEREIGN_RERANK_DEDUP_CORPORA") {
        Ok(s) if s.is_empty() => None,
        Ok(s) => Some(
            s.split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect::<std::collections::HashSet<_>>(),
        ),
        Err(_) => Some(["sep".to_string()].into_iter().collect()),
    };

    // Dedup picker: `fused` (default, RRF/blended-score order) or
    // `vector` (cosine distance to query — tests whether RRF noise
    // inside an article is what hurts wiki dedup).
    let dedup_picker = match std::env::var("SOVEREIGN_RERANK_DEDUP_PICKER")
        .as_deref()
        .unwrap_or("fused")
    {
        "vector" | "vector_distance" => corpus_engine::DedupPicker::VectorDistance,
        _ => corpus_engine::DedupPicker::FusedScore,
    };

    if dedup_only {
        let mut cfg = corpus_engine::RerankConfig::default();
        cfg.enabled = true;
        cfg.per_article = true;
        cfg.dedup_corpus_filter = dedup_filter.clone();
        cfg.dedup_picker = dedup_picker;
        if let Ok(s) = std::env::var("SOVEREIGN_RERANK_CANDIDATES_K") {
            if let Ok(n) = s.parse::<usize>() {
                cfg.candidates_k = n;
            }
        }
        eprintln!(
            "Rerank dedup-only ablation: candidates_k={}, per_article=true, picker={:?}, dedup_corpora={:?} (no cross-encoder)",
            cfg.candidates_k,
            cfg.dedup_picker,
            cfg.dedup_corpus_filter.as_ref().map(|s| {
                let mut v: Vec<&String> = s.iter().collect();
                v.sort();
                v
            })
        );
        runtime = runtime.with_rerank_config(cfg);
    } else if let Ok(rerank_path) = std::env::var("SOVEREIGN_RERANK_MODEL_PATH") {
        let path = PathBuf::from(&rerank_path);
        match sovereign_inference::reranker_standalone::StandaloneReranker::load(
            &path,
            sovereign_core::model_family::ModelFamily::Reranker,
            None,
        ) {
            Ok(reranker) => {
                let reranker: Arc<dyn InferenceProvider> = Arc::new(reranker);
                let rerank_fn = sovereign_tools::corpus::inference_to_rerank_fn(reranker);
                let mut cfg = corpus_engine::RerankConfig::default();
                cfg.enabled = true;
                if let Ok(s) = std::env::var("SOVEREIGN_RERANK_CANDIDATES_K") {
                    if let Ok(n) = s.parse::<usize>() {
                        cfg.candidates_k = n;
                    }
                }
                if let Ok(s) = std::env::var("SOVEREIGN_RERANK_MIN_SCORE") {
                    if let Ok(f) = s.parse::<f32>() {
                        cfg.min_score = Some(f);
                    }
                }
                if let Ok(s) = std::env::var("SOVEREIGN_RERANK_ALPHA") {
                    if let Ok(f) = s.parse::<f32>() {
                        cfg.alpha = f;
                    }
                }
                if let Ok(s) = std::env::var("SOVEREIGN_RERANK_PER_ARTICLE") {
                    cfg.per_article = s == "1" || s.eq_ignore_ascii_case("true");
                }
                if let Ok(s) = std::env::var("SOVEREIGN_RERANK_ATLAS_WEIGHT") {
                    if let Ok(f) = s.parse::<f32>() {
                        cfg.atlas_weight = f;
                    }
                }
                cfg.dedup_corpus_filter = dedup_filter.clone();
                cfg.dedup_picker = dedup_picker;
                eprintln!(
                    "Reranker:    {} (candidates_k={}, alpha={:.2}, per_article={}, atlas_weight={:.2}, dedup_corpora={:?}, min_score={:?})",
                    path.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
                    cfg.candidates_k,
                    cfg.alpha,
                    cfg.per_article,
                    cfg.atlas_weight,
                    cfg.dedup_corpus_filter.as_ref().map(|s| {
                        let mut v: Vec<&String> = s.iter().collect();
                        v.sort();
                        v
                    }),
                    cfg.min_score
                );
                runtime = runtime.with_rerank(rerank_fn, cfg);
            }
            Err(e) => {
                eprintln!(
                    "warning: failed to load reranker at {}: {e} — running baseline",
                    path.display()
                );
            }
        }
    }

    Ok(ChatSession {
        runtime: Arc::new(runtime),
        store,
        corpus_engine,
        inference,
        daemon_base: base,
        embed_model,
        atlas_mgr,
    })
}

/// GET `/v1/models` with a 2s timeout. Any non-200 aborts bootstrap
/// with a clear remediation hint — the alternative is cryptic
/// "connection refused" errors minutes later, mid-retrieval.
async fn probe_or_bail(base: &str) -> Result<()> {
    let url = format!("{base}/v1/models");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|e| Error::Serialization(format!("http client build: {e}")))?;
    match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => Ok(()),
        Ok(r) => Err(Error::Serialization(format!(
            "daemon at {base} returned {} from /v1/models. \
             Is it really a sovereign daemon? Try `sovereign doctor`.",
            r.status()
        ))),
        Err(_) => Err(Error::Serialization(format!(
            "daemon unreachable at {base}. \
             Start it with `sovereign daemon run`, or pass --daemon <url>."
        ))),
    }
}

/// Resolve `(chat_model_id, embed_model_id)` against the daemon.
/// See the call-site comment in `build_session` for the preference
/// order — explicit flag → SetupConfig stem → `/v1/models` probe.
async fn resolve_model_ids(v1: &str, globals: &ChatGlobals) -> Result<(String, String)> {
    // (a) Explicit flags short-circuit everything.
    if let (Some(c), Some(e)) = (&globals.chat_model, &globals.embed_model) {
        return Ok((c.clone(), e.clone()));
    }

    // (b) SetupConfig filename stems. The daemon loads
    //     `config.models.embed` and advertises it on `/v1/models`
    //     under its filename stem (e.g. `qwen-embedding-0.6b.gguf`
    //     → `qwen-embedding-0.6b`). Preferring the stem over
    //     `/v1/models` iteration means we always reach the
    //     *local* slot, never a mesh-peer advertisement, and the
    //     answer is stable across invocations.
    let from_config = chat_and_embed_stems_from_config();
    let mut chat_found = globals
        .chat_model
        .clone()
        .or_else(|| from_config.as_ref().and_then(|s| s.chat.clone()));
    let mut embed_found = globals
        .embed_model
        .clone()
        .or_else(|| from_config.as_ref().and_then(|s| s.embed.clone()));
    if let (Some(c), Some(e)) = (chat_found.as_ref(), embed_found.as_ref()) {
        return Ok((c.clone(), e.clone()));
    }

    // (c) Fallback: probe `/v1/models`. Used when SetupConfig is
    //     absent (fresh install, dev without setup) or when it
    //     lacks one of the two slots.
    let url = format!("{v1}/models");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| Error::Serialization(format!("http client build: {e}")))?;
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| Error::Serialization(format!("GET {url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(Error::Serialization(format!(
            "GET {url} returned {}",
            resp.status()
        )));
    }
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| Error::Serialization(format!("parse /v1/models: {e}")))?;
    let arr = v
        .get("data")
        .and_then(|d| d.as_array())
        .ok_or_else(|| Error::Serialization("/v1/models: no `data` array".into()))?;
    for m in arr {
        let Some(id) = m.get("id").and_then(|s| s.as_str()) else {
            continue;
        };
        let lower = id.to_lowercase();
        let is_embed = lower.contains("embedding") || lower.contains("-embed");
        if is_embed {
            if embed_found.is_none() {
                embed_found = Some(id.to_string());
            }
        } else if chat_found.is_none() {
            chat_found = Some(id.to_string());
        }
    }

    match (chat_found, embed_found) {
        (Some(c), Some(e)) => Ok((c, e)),
        (None, _) => Err(Error::Serialization(
            "daemon lists no chat models — check `sovereign setup` and the primary/fast slots"
                .into(),
        )),
        (_, None) => Err(Error::Serialization(
            "daemon lists no embedding model — retrieval will fail. Set `[models] embed` in \
             ~/.sovereign/config.toml or pass --embed-model."
                .into(),
        )),
    }
}

/// Filename-stem extraction for `SetupConfig.models.{primary,embed}`.
/// The daemon advertises these on `/v1/models` using exactly the
/// file stem (`qwen-embedding-0.6b.gguf` → `qwen-embedding-0.6b`),
/// so returning those stems gives us the stable local-model IDs
/// without any `/v1/models` round-trip.
struct ConfigModelStems {
    chat: Option<String>,
    embed: Option<String>,
}

fn chat_and_embed_stems_from_config() -> Option<ConfigModelStems> {
    let cfg = sovereign_core::setup_config::SetupConfig::load().ok()?;
    Some(ConfigModelStems {
        chat: cfg
            .models
            .primary
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string()),
        embed: cfg
            .models
            .embed
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string()),
    })
}

/// Emit a one-line summary of what the CorpusEngine can see. Helps
/// the user confirm they're pointing at the right `~/.sovereign/indexes`
/// before running a confused-retrieval diagnostic.
async fn log_installed_corpora(engine: &corpus_engine::CorpusEngine) {
    match engine.installed_indexes().await {
        Ok(ix) if ix.is_empty() => {
            eprintln!("Corpora:     (none installed)");
        }
        Ok(ix) => {
            let names: Vec<String> = ix
                .iter()
                .map(|i| format!("{} ({} chunks)", i.corpus_id, i.chunk_count))
                .collect();
            eprintln!("Corpora:     {}", names.join(", "));
        }
        Err(e) => {
            eprintln!("Corpora:     <error: {e}>");
        }
    }
}

/// Probe `<indexes_dir>/<corpus_id>/wikipedia_graph.db` for each
/// installed corpus and return the first WikipediaGraph that opens
/// cleanly. `None` when no graph file is present — retrieval then
/// behaves exactly as before (no graph expansion, no contested
/// markers). Builds graphs out-of-band via
/// `sovereign atlas wikipedia build-graph <corpus-id>`.
async fn load_wikipedia_graph(
    engine: &corpus_engine::CorpusEngine,
    indexes_dir: &std::path::Path,
) -> Option<Arc<corpus_engine::WikipediaGraph>> {
    // Memory-pressure escape hatch. The graph is a 7M-edge sqlite
    // mmap; on a host already running the daemon, loading it twice
    // (daemon + bench) has tipped past available RAM in practice. Set
    // SOVEREIGN_DISABLE_WIKI_GRAPH=1 for retrieval workflows that
    // don't need the Layer 0 link graph (e.g. attached-document
    // benches that only exercise the doc-local index).
    if std::env::var("SOVEREIGN_DISABLE_WIKI_GRAPH")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        eprintln!("Wiki graph:  disabled via SOVEREIGN_DISABLE_WIKI_GRAPH");
        return None;
    }
    let infos = engine.installed_indexes().await.ok()?;
    for info in infos {
        let db_path = corpus_engine::WikipediaGraph::default_db_path(indexes_dir, &info.corpus_id);
        if !db_path.exists() {
            continue;
        }
        match corpus_engine::WikipediaGraph::open(&db_path, &info.corpus_id) {
            Ok(g) => return Some(Arc::new(g)),
            Err(e) => {
                tracing::warn!(
                    corpus = %info.corpus_id,
                    db = %db_path.display(),
                    error = %e,
                    "wikipedia_graph: open failed; skipping"
                );
            }
        }
    }
    None
}

/// Approval channel that silently yes-answers everything. Chat never
/// hits the ask-user path in practice; this prevents a surprise
/// deadlock in a one-shot CLI invocation.
struct AutoApprove;

#[async_trait]
impl ApprovalChannel for AutoApprove {
    async fn request_approval(&self, _step: &Step, _preview: &ActionPreview) -> Result<bool> {
        Ok(true)
    }

    async fn ask_user(&self, _question: &str) -> Result<String> {
        Ok(String::new())
    }

    fn emit_progress(&self, _step: &Step, _output: &StepOutput) {}
}
