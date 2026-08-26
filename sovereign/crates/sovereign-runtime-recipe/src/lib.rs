// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one recipe that commissions a [`Runtime`].
//!
//! # What this crate is
//!
//! `quality/TOPOLOGY.md` §3.5 wants one process to assemble and everything
//! else to be a surface. Phase 5a made the *argument* to `Runtime::new` total
//! — one [`RuntimeParts`] value, no builder chain a host can half-fill. This
//! crate is the other half: the *recipe* that fills it. The router classifier
//! stack, the turn's tool registry, and the enrichment lane are built here,
//! once, and every host receives the same ones.
//!
//! [`commission`] is the only call to `Runtime::new` in first-party production
//! code. That is deliberate and is what `sovereign-core/tests/
//! runtime_commission_census.rs` counts.
//!
//! # The drift this closes, measured
//!
//! Three hosts each carried their own copy of this recipe, and on 2026-08-25
//! only ONE of the eleven optional slots (`corpus_engine`) was wired by all
//! three. `routing_events` was absent in `svrn chat` for the entire life of
//! the builder surface; `compaction` existed in the desktop alone;
//! `corpus_principal` in the server alone. None of those was a decision
//! anybody took — a builder chain records a call and records nothing at all
//! about a call not made.
//!
//! # What it does NOT decide
//!
//! Anything a host genuinely differs on stays a host input, and each one is a
//! field the host must write rather than a builder it can forget:
//!
//! - the **inference provider** (embedded llama.cpp in the daemon, a
//!   `SplitInferenceProvider` over HTTP in the CLI),
//! - the **state store** — this crate never opens one. One writer per data
//!   root is a property of the process (TOPOLOGY phase 1), and a recipe that
//!   opened `sovereign.db` would be a second writer by construction,
//! - **shell access** ([`ShellAccess`]), because it is a real policy fork and
//!   §10 decided it: shell execution does not move into a long-lived daemon
//!   running as a different user with a different cwd,
//! - the five slots §3.5 says leave the `Runtime` entirely — `mesh_knowledge`,
//!   `compaction`, `routing_events`, `landscape_digests`, `corpus_principal`.
//!   They are left at named absence in the returned parts and a host that has
//!   one overrides it with struct-update syntax, so the override reads as a
//!   diff against this baseline.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sovereign_core::planner::LlmPlanner;
use sovereign_core::runtime::lane::LaneSources;
use sovereign_core::runtime::Runtime;
use sovereign_core::traits::{ApprovalChannel, InferenceProvider, RoutingEventSink, StateStore};
use sovereign_core::types::InferenceConfig;
use sovereign_core::{RuntimeParts, SkillRegistry, ToolRegistry};
use sovereign_tools::atlas_context_manager::AtlasContextManager;

/// Where the recipe's progress lines go.
///
/// One method, for the same reason `sovereign_core::runtime::TurnSink` has
/// one: commissioning only ever does one thing to a reporter. An interactive
/// CLI prints them as its boot banner; a daemon traces them. Neither is the
/// recipe's business, and a recipe that chose `eprintln!` — as all three
/// copies of it did — is one a daemon cannot use without shouting into its own
/// stderr.
pub trait RecipeProgress: Send + Sync {
    /// One already-formatted line. Callers format; this only routes.
    fn note(&self, line: &str);
}

/// The default: every line at `info` on the `runtime_recipe` target.
pub struct TracingProgress;

impl RecipeProgress for TracingProgress {
    fn note(&self, line: &str) {
        tracing::info!(target: "runtime_recipe", "{line}");
    }
}

/// Whether the turn's tool registry gets a shell.
///
/// A closed set of two, as an enum rather than a `bool`, because the two arms
/// are not symmetric preferences — one of them is a decision with a reason
/// (ARCH §2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellAccess {
    /// Register `ShellTool`. Correct for an interactive CLI: it runs as the
    /// invoking user, in the directory they invoked it from, for the length of
    /// one command they are watching.
    Granted,
    /// Do not register `ShellTool`. `quality/TOPOLOGY.md` §10 "Decisions
    /// taken" 1: shell execution does not move into a long-lived daemon
    /// running as a different user with a different cwd. Withheld is written
    /// down here so a reader sees a decision rather than an omission
    /// (ARCH §18.3).
    Withheld,
}

/// How much of the enrichment lane is loaded before commissioning returns.
///
/// Not a preference. The cross-corpus meta-atlas is a single ~1 GB JSON file
/// (`canonical_atoms.json`, 981 MB on the authoring host) and parsing it was
/// measured as the bulk of the desktop splash's `BuildingRuntime` phase, which
/// is why `LaneSources::meta_atlas` is an `ArcSwapOption` cell rather than a
/// value — the type already anticipated this fork, and until 2026-08-25 only
/// the desktop used it, by hand.
///
/// The fork is real because the two kinds of host want opposite things:
///
/// - A **one-shot CLI** is answering one question. It would rather wait once
///   than answer the only question it was asked with less than it has.
/// - A **long-lived service** must reach `listening` promptly, and a boot that
///   blocks on a gigabyte parse is a service that looks hung. It has many
///   turns ahead of it to be enriched for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneWarmth {
    /// Load everything before returning.
    Eager,
    /// Return as soon as the cheap members are wired and fill the meta-atlas
    /// cell from a background blocking task. Turns taken before it lands get
    /// the same treatment as a host with no meta-atlas at all — the boost is a
    /// no-op and retrieval falls back to cosine plus the existing
    /// entity-boost, which is a degradation in ranking and never a wrong
    /// answer.
    Deferred,
}

/// Where this host's cross-encoder rerank comes from — or why it has none.
///
/// `SOVEREIGN_RERANK_MODEL_PATH` names ONE GGUF and TWO different things load
/// it, in two different ways, and which is correct depends on the host:
///
/// - `sovereign daemon run` installs it as a slot **inside its embedded
///   llama.cpp engine** (`install_rerank_slot`, `daemon_cmd/build/inference.rs`).
/// - `svrn chat` loads a **standalone** `StandaloneReranker`, because its
///   provider is remote — a `SplitInferenceProvider` speaks HTTP to the daemon
///   and does not support rerank at all.
///
/// A host that does both puts the same weights in one process twice. That is
/// not hypothetical: it is exactly what the daemon would have done the moment
/// it started using this recipe, and the VRAM pre-flight would not have caught
/// it, because the pre-flight plans one rerank slot and there would have been
/// two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RerankWiring {
    /// Load a standalone cross-encoder from `SOVEREIGN_RERANK_MODEL_PATH`, if
    /// set and if it fits. Correct when the host's provider cannot rerank.
    Standalone,
    /// Do not load one: this host's provider already owns a rerank slot from
    /// the same variable.
    ///
    /// The turn therefore gets NO cross-encoder rerank today — reaching the
    /// host's own slot means handing the lane a `rerank_fn` over the host
    /// provider, which is a separate change. This arm exists so that gap is a
    /// sentence a reader can find rather than a doubled resident model an
    /// operator discovers from RSS (ARCH §18.3).
    AlreadyInProvider,
}

/// What a host must resolve before the recipe can run.
///
/// Total by construction, like [`RuntimeParts`] itself: every field is one the
/// host is the only thing that can answer, and there is no default that would
/// let it be skipped silently.
pub struct RecipeInputs {
    /// The provider every stage infers through. Embedded in the daemon,
    /// remote over HTTP in the CLI — the whole point of the seam.
    pub inference: Arc<dyn InferenceProvider>,
    /// The conversation store, ALREADY OPEN. See the module docs: this crate
    /// does not open one.
    pub store: Arc<dyn StateStore>,
    /// The same store viewed as the conv-tiered briefing reader, when the
    /// host's concrete store implements it (`SqliteStateStore` does;
    /// `InMemoryStateStore` does not). `None` is a real answer, not a
    /// forgotten wire — spec `sovereign/docs/specs/CONV_TIERED_PORT.md`.
    pub conv_tiered: Option<Arc<dyn sovereign_core::conv_tiered::ConvTieredReader>>,
    /// The corpus engine this process retrieves through.
    pub corpus_engine: Arc<corpus_engine::CorpusEngine>,
    /// Backing store for the per-conversation `tool_decision` write hook.
    pub note_store: Option<Arc<corpus_engine_notes::NoteStore>>,
    /// The skill registry the router and planner classify against.
    pub skills: Arc<SkillRegistry>,
    /// How a step that needs a human answer gets one.
    pub approval: Arc<dyn ApprovalChannel>,
    /// Temperature / max tokens / custom instructions for this process.
    pub inference_config: InferenceConfig,
    /// `<data root>/indexes` — where the atlas caches, the Wikipedia link
    /// graph and the per-corpus shards live.
    pub indexes_dir: PathBuf,
    /// The resolved embed model id. Keys the atlas embedding cache, so a host
    /// that swaps models invalidates rather than mixes dimensionalities.
    pub embed_model: String,
    /// Shell policy — see [`ShellAccess`].
    pub shell: ShellAccess,
    /// Lane loading policy — see [`LaneWarmth`]. A field rather than a default
    /// because getting it wrong is invisible in opposite directions: an eager
    /// service looks hung, and a deferred one-shot silently answers its only
    /// question with a boost that had not landed yet.
    pub warmth: LaneWarmth,
    /// Rerank wiring — see [`RerankWiring`]. Getting it wrong loads the same
    /// GGUF twice in one process.
    pub rerank: RerankWiring,
}

/// What the shared recipe produced.
pub struct CommonParts {
    /// Ready for [`commission`], or for struct-update with the host's extras.
    pub parts: RuntimeParts,
    /// The per-process atlas-grounding manager — the SAME `Arc` installed on
    /// `parts.lane.atlas_context`. Returned because a measurement harness
    /// warms a freshly-enriched corpus through it (`warm_one`), and the recipe
    /// only loads atlases already cached on disk. Warming this handle is
    /// visible to the commissioned `Runtime` because they share it.
    pub atlas_context: Arc<AtlasContextManager>,
}

/// THE call that turns parts into a running [`Runtime`].
///
/// A one-line wrapper, and the line is the point: it is the only
/// `Runtime::new` in first-party production code, so "how many processes
/// commission a Runtime" is answerable by counting callers of one function
/// instead of grepping for a constructor (`sovereign-core/tests/
/// runtime_commission_census.rs`).
pub fn commission(parts: RuntimeParts) -> Arc<Runtime> {
    Arc::new(Runtime::new(parts))
}

/// Build the parts every host shares: the tool registry, the router
/// classifier stack, the planner, and the enrichment lane.
///
/// Optional slots are left at named absence. A host with one — the desktop's
/// compaction worker, the server's principal resolver — writes it as a
/// struct-update override on [`CommonParts::parts`].
pub async fn common_parts(inputs: RecipeInputs, progress: &dyn RecipeProgress) -> CommonParts {
    let RecipeInputs {
        inference,
        store,
        conv_tiered,
        corpus_engine,
        note_store,
        skills,
        approval,
        inference_config,
        indexes_dir,
        embed_model,
        shell,
        warmth,
        rerank,
    } = inputs;

    log_installed_corpora(&corpus_engine, progress).await;

    let tools = build_tools(&store, &inference, &corpus_engine, shell, progress).await;
    let (router, planner) =
        build_router_and_planner(&inference, &store, &skills, Arc::clone(&tools), progress).await;
    let (lane, atlas_context) = build_lane(
        conv_tiered,
        &corpus_engine,
        &inference,
        &indexes_dir,
        &embed_model,
        warmth,
        rerank,
        progress,
    )
    .await;

    let parts = RuntimeParts {
        corpus_engine: Some(Arc::clone(&corpus_engine)),
        note_store,
        ..RuntimeParts::new(
            inference,
            router,
            Box::new(planner),
            tools,
            store,
            skills,
            approval,
            inference_config,
            lane,
        )
    };

    CommonParts {
        parts,
        atlas_context,
    }
}

// ─── Tools ────────────────────────────────────────────────────────────────

/// The turn's tool registry.
///
/// Kept as one list, in one place, because the alternative is what
/// `sovereign-core/tests/turn_tool_census.rs` measured on 2026-08-25: 33 tools
/// across the hosts, 7 common, 26 divergent — so which tools a model may call
/// depended on which binary you happened to be talking to.
async fn build_tools(
    store: &Arc<dyn StateStore>,
    inference: &Arc<dyn InferenceProvider>,
    corpus_engine: &Arc<corpus_engine::CorpusEngine>,
    shell: ShellAccess,
    progress: &dyn RecipeProgress,
) -> Arc<ToolRegistry> {
    // Tier 4 — shared tool-result cache. Per-conversation cache slices, 5-turn
    // TTL. Idempotent tools (knowledge_lookup, code-intel reads) hit the cache
    // when the model re-calls with the same args within the window.
    let tool_cache = Arc::new(sovereign_core::tool_result_cache::ToolResultCache::new());
    let mut tools = ToolRegistry::new().with_cache(Arc::clone(&tool_cache));

    match shell {
        ShellAccess::Granted => tools.register(Box::new(sovereign_tools::shell::ShellTool)),
        ShellAccess::Withheld => {
            tracing::debug!(
                "runtime_recipe: shell withheld — TOPOLOGY.md §10 decision 1 \
                 (no shell in a long-lived daemon)"
            );
        }
    }

    tools.register(Box::new(
        sovereign_tools::document::DocumentTool::new(Arc::clone(store), Arc::clone(inference))
            .declared(),
    ));
    tools.register(Box::new(
        sovereign_tools::ClaimSearchTool::new(Arc::clone(corpus_engine)).declared(),
    ));
    tools.register(Box::new(
        sovereign_tools::EpistemicLandscapeTool::new(Arc::clone(corpus_engine)).declared(),
    ));
    // Deterministic land-value-tax analytics over parcel corpora (e.g.
    // sf-assessor-roll) — pre-cited figures the ComplexTask synthesizer quotes
    // verbatim ("no confabulated numbers").
    tools.register(Box::new(
        sovereign_tools::parcel_analytics::ParcelAnalyticsTool::new(Arc::clone(corpus_engine))
            .declared(),
    ));
    // Typed SEC-filing figures with basis + accession, or first-class
    // refusals; declares the opt-in bare-numeral audit (FINANCIAL_CORPORA §6).
    tools.register(Box::new(
        sovereign_tools::sec_facts::SecFactsTool::new(Arc::clone(corpus_engine)).declared(),
    ));
    tools.register(Box::new(sovereign_tools::search::SearchTool::with_web(
        Arc::clone(store),
        Arc::clone(inference),
        // Client built by the egress boundary (order deep-research-t2a):
        // tools-base is contract-only and must not construct an
        // egress-capable HTTP client itself.
        sovereign_core::egress::search_client().expect("egress boundary search client build"),
        // DuckDuckGo — free, no key required.
        sovereign_tools::web::search::SearchBackend::DuckDuckGo,
    )));
    // Unified knowledge-lookup front door (Tool-Mastery Phase 5). Returns a
    // single Evidence envelope across corpus + memory + note channels.
    tools.register(Box::new(
        sovereign_tools::KnowledgeLookupTool::new(Arc::clone(store), Arc::clone(inference))
            .declared(),
    ));
    tools.register(Box::new(sovereign_tools::web::WebFetchTool::new()));
    tools.register(Box::new(
        sovereign_tools::WikipediaFetchTool::new(Arc::clone(corpus_engine)).declared(),
    ));
    // Registered unconditionally; `execute()` returns a clear "no document
    // attached" payload on conversations without a DocumentSession, so the
    // model can probe it harmlessly (sovereign decision 7693f16b: attached
    // docs as Tool, not parallel pipeline).
    tools.register(Box::new(
        sovereign_tools::AttachedDocumentSearchTool::new(Arc::clone(store), Arc::clone(inference))
            .declared(),
    ));

    // External MCP servers (the `[[mcp_servers]]` array of the canonical
    // config): connect over HTTP and register their tools into the SAME
    // registry the agent plans against, so a server added via `svrn mcp add`
    // or the desktop settings pane is callable here too.
    let mcp = sovereign_tools::mcp::load_from_setup_config(&mut tools).await;
    for st in mcp.server_statuses().await {
        if st.connected {
            progress.note(&format!(
                "MCP:         {} ({} tools)",
                st.name, st.tool_count
            ));
        } else if let Some(e) = &st.error {
            progress.note(&format!("MCP:         {} unavailable — {e}", st.name));
        }
    }

    progress.note(&format!("Tools:       {} registered", tools.count()));
    Arc::new(tools)
}

// ─── Router + planner ─────────────────────────────────────────────────────

async fn build_router_and_planner(
    inference: &Arc<dyn InferenceProvider>,
    store: &Arc<dyn StateStore>,
    skills: &Arc<SkillRegistry>,
    tools: Arc<ToolRegistry>,
    progress: &dyn RecipeProgress,
) -> (Box<dyn sovereign_core::traits::Router>, LlmPlanner) {
    // Built through the shared `router_bootstrap` helper so every host wires
    // the SAME classifiers (parity by construction). `from_env_and_repo` keeps
    // the `$SOVEREIGN_*` overlay + repo-relative exemplars for dev tuning; a
    // packaged build falls through to the baked set.
    let (llm_router, router_report) = sovereign_core::router_bootstrap::build_llm_router(
        Arc::clone(inference),
        Arc::clone(store),
        Arc::clone(skills),
        &sovereign_core::router_bootstrap::ExemplarOverrides::from_env_and_repo(),
        || {
            progress.note(
                "Router: exemplar embed cache cold — re-embedding exemplars \
                 (can take minutes on a CPU-only embed slot)…",
            )
        },
    )
    .await;
    progress.note(&format!(
        "Router classifier stack: embed={} scope={} effort={} current_info={}",
        router_report.embed_router.is_some(),
        router_report.scope.is_some(),
        router_report.effort.is_some(),
        router_report.current_info.is_some(),
    ));
    // Authority probe (FINANCIAL_CORPORA §7.3): the router consults the
    // registry's deterministic claims before intent classification.
    let router: Box<dyn sovereign_core::traits::Router> =
        Box::new(llm_router.with_authority_probe(tools));
    let planner = LlmPlanner::new(Arc::clone(inference), Arc::clone(skills));
    (router, planner)
}

// ─── The enrichment lane ──────────────────────────────────────────────────

/// Gather the turn's enrichment stack BEFORE the `Runtime` exists.
///
/// daemon-convergence Phase 4b: `LaneSources` is a required argument, not
/// eight `with_*` calls a host can forget. None of it needs a `Runtime` —
/// every provider is a function of the engine, the provider and the indexes
/// dir — so the gathering happens here and the constructor runs once the stack
/// is complete.
async fn build_lane(
    conv_tiered: Option<Arc<dyn sovereign_core::conv_tiered::ConvTieredReader>>,
    corpus_engine: &Arc<corpus_engine::CorpusEngine>,
    inference: &Arc<dyn InferenceProvider>,
    indexes_dir: &Path,
    embed_model: &str,
    warmth: LaneWarmth,
    rerank: RerankWiring,
    progress: &dyn RecipeProgress,
) -> (LaneSources, Arc<AtlasContextManager>) {
    let mut lane = LaneSources::none();
    lane.conv_tiered = conv_tiered;
    lane.gliner = load_gliner();

    // Atlas Layer 0: the installed Wikipedia link graph, if one is built.
    if let Some(graph) = load_wikipedia_graph(corpus_engine, indexes_dir, progress).await {
        progress.note(&format!(
            "Wiki graph:  {} articles, {} edges",
            graph.article_count().await,
            graph.edge_count().await,
        ));
        lane.wikipedia_graph = Some(graph);
    }

    // Atlas-grounded retrieval. Loads every atlas whose embeddings are already
    // cached on disk; cold-start embed work is deliberately NOT done here (it
    // belongs in the post-install hook, so the first user query has a
    // deterministic latency rather than waiting on a wiki-scale embed pass).
    let atlas_mgr = Arc::new(AtlasContextManager::new(
        indexes_dir.to_path_buf(),
        Arc::clone(inference),
        embed_model.to_string(),
    ));
    lane.atlas_context =
        Some(Arc::clone(&atlas_mgr)
            as Arc<
                dyn sovereign_core::atlas_context::AtlasContextProvider,
            >);
    atlas_mgr.init_from_cache().await;
    progress.note(&format!(
        "Atlas: {} corpus context(s) loaded from cache",
        sovereign_core::atlas_context::AtlasContextProvider::loaded_corpus_ids(atlas_mgr.as_ref())
            .len()
    ));
    // Adaptive triage (Phase B2): the bump-flusher lands query-time hits on
    // disk so they feed the next triage rebuild. 30s — losing up to half a
    // minute of bumps on a hard kill is acceptable for a statistical signal.
    let _bump_flusher = Arc::clone(&atlas_mgr).spawn_bump_flusher(30);

    // Cross-corpus meta-atlas (Move 5). Empty / absent file → the boost is a
    // no-op and retrieval falls back to cosine + existing entity-boost.
    load_meta_atlas(&lane, warmth, progress);

    // Cross-corpus bridge edges (Phase 6). Empty/absent → bridge_boost is a
    // no-op; the boost only runs at all when `SOVEREIGN_META_BRIDGE` is set.
    let bridge_index = match corpus_engine::meta_atlas::BridgeIndex::load(None) {
        Ok(idx) => Arc::new(idx),
        Err(e) => {
            progress.note(&format!("Bridge: load failed ({e}); bridge boost disabled"));
            Arc::new(corpus_engine::meta_atlas::BridgeIndex::empty())
        }
    };
    progress.note(&format!(
        "Bridge:      {} cross-corpus edges",
        bridge_index.len()
    ));
    lane.bridge = Some(Arc::clone(&bridge_index));

    load_reranker(&mut lane, rerank, progress);

    (lane, atlas_mgr)
}

/// Fill (or arrange to fill) `lane.meta_atlas` — see [`LaneWarmth`] for why
/// this is a fork rather than one behaviour.
///
/// The `Deferred` arm was the desktop's private background warm until
/// 2026-08-25; it lives here now so the daemon does not need a second copy of
/// it, and so the desktop's can be deleted when it lands on this recipe (ARCH
/// §10.6).
fn load_meta_atlas(lane: &LaneSources, warmth: LaneWarmth, progress: &dyn RecipeProgress) {
    fn read() -> corpus_engine::meta_atlas::MetaAtlasIndex {
        let path = corpus_engine::meta_atlas::default_meta_atlas_path();
        match corpus_engine::meta_atlas::MetaAtlasIndex::load(path.as_deref()) {
            Ok(idx) => idx,
            Err(e) => {
                tracing::warn!(error = %e, "runtime_recipe: meta-atlas load failed; boost disabled");
                corpus_engine::meta_atlas::MetaAtlasIndex::empty()
            }
        }
    }

    match warmth {
        LaneWarmth::Eager => {
            let idx = Arc::new(read());
            progress.note(&format!(
                "Meta-atlas:  {} canonical atoms across {} corpus(es)",
                idx.len(),
                idx.corpus_count(),
            ));
            lane.meta_atlas.store(Some(idx));
        }
        LaneWarmth::Deferred => {
            progress.note("Meta-atlas:  loading in the background (boost lands mid-session)");
            // The cell, not the `Runtime` — the `Runtime` does not exist yet,
            // and it never needed to: `install_meta_atlas` is one `store` on
            // this same `ArcSwapOption`. Cloning the cell here is what lets the
            // warm start before commissioning finishes.
            let cell = Arc::clone(&lane.meta_atlas);
            tokio::spawn(async move {
                // Blocking + CPU-heavy (a ~1 GB JSON parse); off the async
                // pool or it stalls every other task on this runtime.
                match tokio::task::spawn_blocking(read).await {
                    Ok(idx) => {
                        let atoms = idx.len();
                        cell.store(Some(Arc::new(idx)));
                        tracing::info!(
                            target: "runtime_recipe",
                            atoms,
                            "meta-atlas(bg): cross-corpus boost ready"
                        );
                    }
                    Err(e) => tracing::warn!(
                        error = %e,
                        "runtime_recipe: meta-atlas background warm panicked; boost stays off"
                    ),
                }
            });
        }
    }
}

/// GLiNER entity extractor for entity-aware retrieval-over-history. Probe
/// first; a missing model soft-falls-through to pure cosine + MMR.
fn load_gliner() -> Option<Arc<dyn sovereign_core::traits::EntityExtractor>> {
    let model_id = sovereign_gliner::gliner_ner::DEFAULT_MODEL_ID;
    if !sovereign_gliner::gliner_ner::probe_model_available(model_id) {
        tracing::debug!(
            model = model_id,
            "runtime_recipe: GLiNER model not installed; entity-aware \
             retrieval disabled (falls back to cosine+MMR)"
        );
        return None;
    }
    match sovereign_gliner::gliner_ner::GlinerExtractor::new_default() {
        Ok(g) => {
            tracing::info!(
                model = model_id,
                "runtime_recipe: GLiNER entity extractor loaded"
            );
            Some(Arc::new(g) as Arc<dyn sovereign_core::traits::EntityExtractor>)
        }
        Err(e) => {
            tracing::warn!(error = %e, "runtime_recipe: GLiNER probe ok but load failed; entity-aware retrieval disabled");
            None
        }
    }
}

/// Probe `<indexes_dir>/<corpus_id>/wikipedia_graph.db` (or the columnar
/// atlas store) for each installed corpus and return the first graph that
/// opens cleanly.
async fn load_wikipedia_graph(
    engine: &corpus_engine::CorpusEngine,
    indexes_dir: &Path,
    progress: &dyn RecipeProgress,
) -> Option<Arc<dyn corpus_engine::WikipediaGraphApi>> {
    // Memory-pressure escape hatch. The graph is a 7M-edge sqlite mmap; on a
    // host already running the daemon, loading it twice has tipped past
    // available RAM in practice.
    if std::env::var("SOVEREIGN_DISABLE_WIKI_GRAPH")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        progress.note("Wiki graph:  disabled via SOVEREIGN_DISABLE_WIKI_GRAPH");
        return None;
    }
    // WIKIPEDIA_ATLAS_V2 W3: per corpus, prefer the columnar store
    // (atlas/articles.lance + edges.lance) over the SQLite graph — the shared
    // `corpus_engine::open_wikipedia_graph` gate.
    let infos = engine.installed_indexes().await.ok()?;
    for info in infos {
        if let Some(g) = corpus_engine::open_wikipedia_graph(indexes_dir, &info.corpus_id).await {
            return Some(g);
        }
    }
    None
}

async fn log_installed_corpora(
    engine: &corpus_engine::CorpusEngine,
    progress: &dyn RecipeProgress,
) {
    match engine.installed_indexes().await {
        Ok(ix) if ix.is_empty() => progress.note("Corpora:     (none installed)"),
        Ok(ix) => {
            let names: Vec<String> = ix
                .iter()
                .map(|i| format!("{} ({} chunks)", i.corpus_id, i.chunk_count))
                .collect();
            progress.note(&format!("Corpora:     {}", names.join(", ")));
        }
        Err(e) => progress.note(&format!("Corpora:     <error: {e}>")),
    }
}

/// The optional cross-encoder reranker, and the dedup-only ablation that
/// takes precedence over it.
fn load_reranker(lane: &mut LaneSources, wiring: RerankWiring, progress: &dyn RecipeProgress) {
    if wiring == RerankWiring::AlreadyInProvider {
        // See `RerankWiring::AlreadyInProvider`. The dedup-only ablation below
        // is skipped too: it is a rerank CONFIG, and configuring a rerank this
        // host will not run is the kind of half-set state this document exists
        // to remove.
        tracing::debug!(
            "runtime_recipe: no standalone reranker — this host's provider \
             already owns the slot"
        );
        return;
    }
    // Dedup-only ablation: `SOVEREIGN_RERANK_DEDUP_ONLY=1` enables overfetch +
    // per-article dedup using ONLY the fusion ordering — the experiment that
    // asks whether the SEP source-recall lift is the dedup mechanism or the
    // cross-encoder logits (`sovereign/docs/RERANK_EXPERIMENT.md`). It takes
    // precedence so an operator can A/B without touching two env vars.
    let dedup_only = std::env::var("SOVEREIGN_RERANK_DEDUP_ONLY")
        .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let dedup_filter = sovereign_tools::corpus::rerank_dedup_filter_from_env();
    let dedup_picker = sovereign_tools::corpus::rerank_dedup_picker_from_env();

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
        progress.note(&format!(
            "Rerank dedup-only ablation: candidates_k={}, per_article=true, \
             picker={:?}, dedup_corpora={:?} (no cross-encoder)",
            cfg.candidates_k,
            cfg.dedup_picker,
            sorted_corpora(cfg.dedup_corpus_filter.as_ref()),
        ));
        // A config with no cross-encoder. `Rerank` holds both halves and
        // `Rerank::active()` is the one place they are read together.
        lane.rerank.config = cfg;
        return;
    }

    // ONE loader, and it now carries the capacity pre-flight this recipe used
    // to carry alone (`sovereign_inference::reranker_standalone::load_from_env`
    // — see its docs for the mirror that was not one). The refusal MESSAGE
    // comes back rather than being logged and swallowed, because the caller
    // with a banner is the one that can show it to a person.
    match sovereign_inference::reranker_standalone::load_from_env() {
        sovereign_inference::reranker_standalone::RerankLoad::Loaded(reranker) => {
            let rerank_fn = sovereign_tools::corpus::inference_to_rerank_fn(reranker);
            let cfg = sovereign_tools::corpus::rerank_config_from_env();
            progress.note(&format!(
                "Reranker:    candidates_k={}, alpha={:.2}, per_article={}, \
                 atlas_weight={:.2}, dedup_corpora={:?}, min_score={:?}",
                cfg.candidates_k,
                cfg.alpha,
                cfg.per_article,
                cfg.atlas_weight,
                sorted_corpora(cfg.dedup_corpus_filter.as_ref()),
                cfg.min_score
            ));
            lane.rerank.f = Some(rerank_fn);
            lane.rerank.config = cfg;
        }
        sovereign_inference::reranker_standalone::RerankLoad::Refused { message } => {
            progress.note(&format!("Reranker:    REFUSED — {message}"));
        }
        sovereign_inference::reranker_standalone::RerankLoad::Failed { message } => {
            progress.note(&format!("Reranker:    {message}"));
        }
        // Opt-in, and nobody opted in. Nothing to say.
        sovereign_inference::reranker_standalone::RerankLoad::NotConfigured => {}
    }
}

/// Deterministic rendering of the dedup allowlist. A `HashSet`'s iteration
/// order is not stable across runs, and this string lands in boot logs two
/// operators compare.
fn sorted_corpora(filter: Option<&std::collections::HashSet<String>>) -> Option<Vec<&String>> {
    filter.map(|s| {
        let mut v: Vec<&String> = s.iter().collect();
        v.sort();
        v
    })
}
