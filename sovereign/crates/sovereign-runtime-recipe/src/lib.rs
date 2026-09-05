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
//! - the **tool bundles** ([`RecipeInputs::tool_bundles`]) — which FAMILIES
//!   of tools this host's turn registry carries. This crate registers no tool
//!   by name; it folds the host's bundles. See
//!   [`sovereign_contracts::tool_bundle`] for why, and [`baseline_bundles`]
//!   for the composition a corpus-grounded host starts from,
//! - the five slots §3.5 says leave the `Runtime` entirely — `mesh_knowledge`,
//!   `compaction`, `routing_events`, `landscape_digests`, `corpus_principal`.
//!   They are left at named absence in the returned parts and a host that has
//!   one overrides it with struct-update syntax, so the override reads as a
//!   diff against this baseline.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sovereign_contracts::tool_bundle::{ToolBundle, Withheld};
use sovereign_core::planner::LlmPlanner;
use sovereign_core::runtime::lane::LaneSources;
use sovereign_core::runtime::Runtime;
use sovereign_core::traits::{ApprovalChannel, InferenceProvider, StateStore};
use sovereign_core::types::InferenceConfig;
use sovereign_core::{RuntimeParts, SkillRegistry, ToolRegistry};
use sovereign_tools::atlas_context_manager::AtlasContextManager;
use sovereign_tools::bundles::{
    CoreTurnTools, KnowledgeFrontDoor, WebEscalation, WebReach, WebTools,
};

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

    /// A named milestone, for a host that drives a progress UI off it.
    ///
    /// Separate from [`Self::note`] because a splash screen needs to know
    /// WHICH stage began, and the only alternative — matching on the prose of
    /// a `note` line — makes a reworded log message a UI regression. The
    /// default routes the label through `note`, so a host that only traces
    /// implements nothing.
    fn phase(&self, phase: RecipePhase) {
        self.note(phase.label());
    }
}

/// The stages of commissioning a host may want to show someone.
///
/// A closed set (ARCH §2): these are the points where the recipe is about to
/// spend enough time that a surface with a window owes the user a word about
/// it. Adding a stage is a deliberate edit here, and an exhaustive `match` in
/// the desktop's splash mapping is what makes forgetting one a build error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecipePhase {
    /// Folding the host's bundles, then connecting external MCP servers.
    WiringTools,
    /// Building the four embed-based router classifiers.
    AssemblingRouter,
    /// The router exemplar cache MISSED — re-embedding ~300 exemplars, which
    /// is minutes on a CPU-only embed slot. The one stage whose whole reason
    /// for existing is that a silent boot looks hung.
    RebuildingRouterEmbeddings,
    /// Gathering the enrichment lane: atlases, the wiki graph, GLiNER.
    BuildingLane,
}

impl RecipePhase {
    /// The line a host without a progress UI logs instead.
    pub fn label(self) -> &'static str {
        match self {
            RecipePhase::WiringTools => "Tools:       wiring",
            RecipePhase::AssemblingRouter => "Router:      assembling classifier stack",
            RecipePhase::RebuildingRouterEmbeddings => {
                "Router: exemplar embed cache cold — re-embedding exemplars \
                 (can take minutes on a CPU-only embed slot)…"
            }
            RecipePhase::BuildingLane => "Lane:        gathering enrichment sources",
        }
    }
}

/// The default: every line at `info` on the `runtime_recipe` target.
pub struct TracingProgress;

impl RecipeProgress for TracingProgress {
    fn note(&self, line: &str) {
        tracing::info!(target: "runtime_recipe", "{line}");
    }
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

/// **Which corpora this host serves** — and therefore which lane members are
/// worth loading at all.
///
/// Orthogonal to [`LaneWarmth`], and composed with it rather than conflated:
/// warmth says WHEN a member loads, scope says WHETHER it is reachable. A
/// third `LaneWarmth` variant would have forced the two questions onto one
/// axis, and "deferred" is the wrong answer for a member the host can never
/// consult — deferring a load still pays it, on a background thread, on the
/// same contended box.
///
/// The fork is real because three of `build_lane`'s members are cross-corpus
/// BY CONSTRUCTION and one is corpus-specific:
///
/// - the **meta-atlas** is canonical atoms clustered ACROSS corpora,
/// - the **bridge** is edges BETWEEN corpora,
/// - the **wikipedia graph** is one named corpus's link graph,
///
/// so a host sealed to a single non-wikipedia corpus cannot reach any of
/// them. It loaded all four anyway until 2026-09-04: measured on this host,
/// `svrn bench chaos-monkey` sealed to `chaos-secret-agent` (316 chunks)
/// spent 22.3 s of a 24.9 s startup on a 7.85M-edge graph, a 981 MB
/// meta-atlas JSON and a bridge index with zero edges — 89.6% of the fixed
/// cost, paid again by every one-shot bench lane in `svrn quality check`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaneScope {
    /// Every installed corpus is reachable: a daemon, a desktop, a hub
    /// server. Loads every lane member — byte-identical to the behaviour
    /// this recipe had before the scope existed.
    All,
    /// This host answers questions about exactly ONE corpus and is sealed to
    /// it (a bench lane, a one-shot eval). Cross-corpus members are not
    /// loaded, and their absence is a no-op rather than a degradation:
    /// there is no second corpus for a bridge edge to reach or a canonical
    /// atom to cluster with.
    Sealed(String),
}

impl LaneScope {
    /// Is `corpus_id` reachable from this host?
    ///
    /// The one decider for every per-corpus member, so a member cannot
    /// privately re-derive the answer (ARCH §10.6).
    pub fn includes(&self, corpus_id: &str) -> bool {
        match self {
            LaneScope::All => true,
            LaneScope::Sealed(id) => id == corpus_id,
        }
    }

    /// Can a CROSS-corpus member (meta-atlas, bridge) say anything here?
    ///
    /// False under `Sealed` for the structural reason, not as a cost
    /// heuristic: both members are defined over PAIRS of corpora, and a
    /// sealed host has one.
    pub fn spans_corpora(&self) -> bool {
        matches!(self, LaneScope::All)
    }

    /// How this scope reads in a progress line.
    pub fn label(&self) -> String {
        match self {
            LaneScope::All => "all corpora".to_string(),
            LaneScope::Sealed(id) => format!("sealed to `{id}`"),
        }
    }
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
    /// Which tool FAMILIES this host's turn registry carries.
    ///
    /// The recipe folds these and names no tool itself. A host starts from
    /// [`baseline_bundles`] and pushes its own — code intel over a graph it
    /// owns, a note store it opened — or pushes
    /// [`Withheld`](sovereign_contracts::tool_bundle::Withheld) to record a
    /// family it deliberately does not carry, so a decision is a value rather
    /// than a line missing from a file (ARCH §18.3).
    ///
    /// Replaced `shell: ShellAccess` on 2026-08-26 (TOPOLOGY phase 7b). That
    /// enum could only express ONE fork; every other family was hardcoded
    /// here, which is why "adopt the shared recipe" read as "lose twenty
    /// tools" to `sovereign-server` and stalled the phase.
    pub tool_bundles: Vec<Box<dyn ToolBundle>>,
    /// Whether a person's tool switches govern what actually registers.
    ///
    /// Orthogonal to `tool_bundles`, and composed with it rather than
    /// conflated: a bundle says what this host CAN provide (it holds the
    /// collaborators), a switch says what the user PERMITTED. Forcing the two
    /// onto one axis would shatter every bundle into one tool each.
    pub switches: ToolSwitches,
    /// External MCP servers this host declares IN ADDITION to the canonical
    /// `~/.svrnmesh/config.toml` `[[mcp_servers]]` array.
    ///
    /// The canonical array is always read — it is what `svrn mcp add` and the
    /// desktop settings pane write, and reading it here is what makes a server
    /// added on one surface callable on all of them. A deployment-scoped host
    /// with a config file of its own adds to it rather than replacing it:
    /// `sovereign-server` read ONLY its own `[mcp]` section until 2026-08-26,
    /// so an operator who ran `svrn mcp add` on that box got the server on
    /// every surface except the one serving their tenants (ARCH §10.6).
    ///
    /// Empty for a host with no config of its own.
    pub mcp_extra: Vec<sovereign_tools::mcp::McpServerConfig>,
    /// Lane loading policy — see [`LaneWarmth`]. A field rather than a default
    /// because getting it wrong is invisible in opposite directions: an eager
    /// service looks hung, and a deferred one-shot silently answers its only
    /// question with a boost that had not landed yet.
    pub warmth: LaneWarmth,
    /// Which corpora this host serves — see [`LaneScope`]. A field rather
    /// than a default for the same reason `warmth` is one, and a sharper
    /// one: the cost of getting it wrong is invisible in BOTH directions.
    /// An over-broad scope on a sealed one-shot is 22 s of measured startup
    /// nobody sees in a wall-clock lane total; an under-broad scope on a
    /// service silently drops the cross-corpus boost.
    pub scope: LaneScope,
    /// Rerank wiring — see [`RerankWiring`]. Getting it wrong loads the same
    /// GGUF twice in one process.
    pub rerank: RerankWiring,
}

/// Whether a person's tool switches govern this host's turn registry.
///
/// `Ungoverned` is not "everything on by mistake" — it is the honest state of
/// a daemon or a hub server, which has no per-user settings panel and no
/// user's answer to consult. Naming it keeps that distinguishable from a
/// surface whose switches were forgotten (ARCH §18.3).
pub enum ToolSwitches {
    /// A surface with a settings panel. Only these families register; every
    /// other one comes back as a withholding in its bundle's report.
    Chosen(sovereign_contracts::tool_bundle::ToolPermissions),
    /// No switches on this host: every family a composed bundle offers
    /// registers, byte-identical to the behaviour before the gate existed.
    Ungoverned,
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
    /// The external-MCP manager this recipe connected.
    ///
    /// The live transports belong to the tools now in the registry, so a host
    /// that ignores this may drop it. A host with an MCP settings pane keeps
    /// it: the per-server statuses that pane renders are readable from here
    /// and nowhere else, and the desktop held its own `load_from_setup_config`
    /// call for exactly that reason until 2026-08-26 (ARCH §10.6 — one door).
    pub mcp: sovereign_tools::mcp::McpServerManager,
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
        tool_bundles,
        switches,
        mcp_extra,
        warmth,
        scope,
        rerank,
    } = inputs;

    log_installed_corpora(&corpus_engine, progress).await;

    let (tools, mcp) = build_tools(&tool_bundles, switches, mcp_extra, progress).await;
    let (router, planner) =
        build_router_and_planner(&inference, &store, &skills, Arc::clone(&tools), progress).await;
    let (lane, atlas_context) = build_lane(
        conv_tiered,
        &corpus_engine,
        &inference,
        &indexes_dir,
        &embed_model,
        warmth,
        scope,
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
        mcp,
    }
}

// ─── Tools ────────────────────────────────────────────────────────────────

/// The tool families every corpus-grounded host carries.
///
/// A host composes this and then pushes its own — it does NOT write the
/// baseline out again, which is the duplication `turn_tool_census.rs` measured
/// on 2026-08-25 (33 tools across the hosts, 7 common, 26 divergent, so which
/// tools a model could call depended on which binary you were talking to).
///
/// Shell is deliberately absent: it is a privilege, not a baseline. A host
/// that wants it pushes [`sovereign_tools::bundles::ShellTools`]; a host that
/// does not pushes [`Withheld`](sovereign_contracts::tool_bundle::Withheld)
/// naming the reason, so the daemon's "no shell in a long-lived daemon" stays
/// a written decision (TOPOLOGY §10 "Decisions taken" 1).
///
/// `wikipedia_fetch` is absent for the same reason and by the same mechanism:
/// a host that reads Wikipedia out of an INSTALLED corpus wants `web_fetch`
/// without it, which is why the two split into
/// [`WebTools`](sovereign_tools::bundles::WebTools) and
/// [`WikipediaTools`](sovereign_tools::bundles::WikipediaTools) on 2026-08-26.
pub fn baseline_bundles(deps: BaselineDeps<'_>) -> Vec<Box<dyn ToolBundle>> {
    let BaselineDeps {
        store,
        inference,
        corpus_engine,
        note_store,
        web,
        escalation,
    } = deps;
    let web_family: Box<dyn ToolBundle> = match &web {
        WebReach::Granted(_) => Box::new(WebTools),
        WebReach::Withheld(why) => Box::new(Withheld::new("web", why)),
    };
    vec![
        Box::new(CoreTurnTools::new(
            Arc::clone(store),
            Arc::clone(inference),
            Arc::clone(corpus_engine),
            web,
        )),
        web_family,
        Box::new(KnowledgeFrontDoor::new(
            Arc::clone(store),
            Arc::clone(inference),
            note_store.map(Arc::clone),
            escalation,
        )),
    ]
}

/// What every baseline family is built from.
///
/// A struct rather than six positional arguments: `store`, `inference` and
/// `corpus_engine` are three `Arc`s a call site can transpose silently, and
/// the last three are host DECISIONS that have to read as decisions at the
/// call site rather than as trailing arguments (ARCH §2.1).
pub struct BaselineDeps<'a> {
    /// The conversation store the knowledge and document tools read.
    pub store: &'a Arc<dyn StateStore>,
    /// The provider the search and lookup tools infer through.
    pub inference: &'a Arc<dyn InferenceProvider>,
    /// The corpus this host retrieves from.
    pub corpus_engine: &'a Arc<corpus_engine::CorpusEngine>,
    /// The open note store, when this host has one. Wires
    /// `knowledge_lookup`'s third evidence channel; `None` is reported as a
    /// withholding rather than passed over in silence.
    pub note_store: Option<&'a Arc<corpus_engine_notes::NoteStore>>,
    /// Whether this host may reach the open internet, and if not, why.
    pub web: WebReach,
    /// Whether thin local results may escalate to a web search on their own.
    pub escalation: WebEscalation,
}

/// The turn's tool registry: fold the host's bundles, then connect MCP.
///
/// This function names no tool. That is the phase-7b property — a family is
/// added by the host that has it, never by editing a shared list, so adopting
/// this recipe can no longer mean losing a capability (ARCH §19 open/closed).
async fn build_tools(
    bundles: &[Box<dyn ToolBundle>],
    switches: ToolSwitches,
    mcp_extra: Vec<sovereign_tools::mcp::McpServerConfig>,
    progress: &dyn RecipeProgress,
) -> (Arc<ToolRegistry>, sovereign_tools::mcp::McpServerManager) {
    progress.phase(RecipePhase::WiringTools);
    // Tier 4 — shared tool-result cache. Per-conversation cache slices, 5-turn
    // TTL. Idempotent tools (knowledge_lookup, code-intel reads) hit the cache
    // when the model re-calls with the same args within the window.
    let tool_cache = Arc::new(sovereign_core::tool_result_cache::ToolResultCache::new());
    let mut tools = ToolRegistry::new().with_cache(Arc::clone(&tool_cache));
    match switches {
        ToolSwitches::Chosen(permitted) => {
            progress.note(&format!(
                "Tools:       user-permitted families — {}",
                permitted
                    .families()
                    .map(|f| f.wire_id())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            tools = tools.with_permitted(permitted);
        }
        ToolSwitches::Ungoverned => {
            progress.note("Tools:       no per-user switches on this host");
        }
    }

    for report in sovereign_contracts::tool_bundle::install(&mut tools, bundles).await {
        // Every family reports, present or absent, so the boot record answers
        // "does this host have X?" without reading the host's source.
        progress.note(&format!("Tools:       {}", report.summary()));
    }

    // External MCP servers (the `[[mcp_servers]]` array of the canonical
    // config): connect over HTTP and register their tools into the SAME
    // registry the agent plans against, so a server added via `svrn mcp add`
    // or the desktop settings pane is callable here too.
    //
    // NOT a bundle: `ToolBundle::register_into` returns a report, and this
    // door also yields a manager whose per-server statuses the boot banner
    // prints. Modelling that needs a keep-alive in the trait's return, which
    // is a change to the seam rather than a use of it — named here rather
    // than half-done (ARCH §18.3).
    let mcp = sovereign_tools::mcp::load_from_setup_config(&mut tools).await;
    if !mcp_extra.is_empty() {
        progress.note(&format!(
            "MCP:         {} server(s) from this host's own config",
            mcp_extra.len()
        ));
        let extra =
            sovereign_tools::mcp::McpServerManager::from_config(&mcp_extra, &mut tools).await;
        mcp.absorb(extra).await;
    }
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
    (Arc::new(tools), mcp)
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
    progress.phase(RecipePhase::AssemblingRouter);
    let (llm_router, router_report) = sovereign_core::router_bootstrap::build_llm_router(
        Arc::clone(inference),
        Arc::clone(store),
        Arc::clone(skills),
        &sovereign_core::router_bootstrap::ExemplarOverrides::from_env_and_repo(),
        || progress.phase(RecipePhase::RebuildingRouterEmbeddings),
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
    scope: LaneScope,
    rerank: RerankWiring,
    progress: &dyn RecipeProgress,
) -> (LaneSources, Arc<AtlasContextManager>) {
    progress.phase(RecipePhase::BuildingLane);
    progress.note(&format!("Lane scope:  {}", scope.label()));
    let mut lane = LaneSources::none();
    lane.conv_tiered = conv_tiered;
    lane.gliner = load_gliner(warmth);

    // Atlas Layer 0: the installed Wikipedia link graph, if one is built.
    //
    // The loader probes each INSTALLED corpus for a wikipedia-shaped graph
    // and takes the first that opens, so the scope belongs inside it rather
    // than as a corpus-id test out here: a host sealed to `wikipedia` still
    // gets its graph, and one sealed to a 316-chunk bench corpus probes one
    // corpus instead of forty-eight. Measured on the authoring host, 51,845
    // articles / 7,853,503 edges = 2.2 s.
    if let Some(graph) = load_wikipedia_graph(corpus_engine, indexes_dir, &scope, progress).await {
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

    load_cross_corpus_members(&mut lane, warmth, &scope, progress);

    load_reranker(&mut lane, rerank, progress);

    (lane, atlas_mgr)
}

/// The lane members defined over PAIRS of corpora — the meta-atlas (canonical
/// atoms clustered ACROSS corpora) and the bridge (edges BETWEEN corpora).
///
/// One function so that "is this member cross-corpus?" has one decider
/// (ARCH §10.6). Both were loaded unconditionally until 2026-09-04, including
/// by hosts sealed to a single corpus, where neither can say anything: there
/// is no second corpus for a bridge edge to reach or a canonical atom to
/// cluster with. Skipping them under [`LaneScope::Sealed`] is a no-op, not a
/// degradation — the same argument [`LaneWarmth::Deferred`] makes about a
/// boost that has not landed, except that here it never lands and never
/// could.
///
/// Measured on the authoring host: meta-atlas 1,563,346 atoms out of a 981 MB
/// JSON = 19.7-23.2 s, bridge 0 edges. That was 19.7 s of a 24.9 s
/// `svrn bench chaos-monkey` startup against a 316-chunk corpus, paid again
/// by every in-process bench lane in `svrn quality check`.
fn load_cross_corpus_members(
    lane: &mut LaneSources,
    warmth: LaneWarmth,
    scope: &LaneScope,
    progress: &dyn RecipeProgress,
) {
    if !scope.spans_corpora() {
        progress.note("Meta-atlas:  not loaded (lane spans one corpus)");
        progress.note("Bridge:      not loaded (lane spans one corpus)");
        return;
    }

    // Cross-corpus meta-atlas (Move 5). Empty / absent file → the boost is a
    // no-op and retrieval falls back to cosine + existing entity-boost.
    load_meta_atlas(lane, warmth, progress);

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

/// GLiNER entity extractor for entity-aware retrieval-over-history, at the
/// warmth this host asked for. Probe first; a missing model soft-falls-through
/// to pure cosine + MMR.
///
/// # Why this takes `warmth`
///
/// `lane.gliner` is a lane member, so [`LaneWarmth`] governs it like every
/// other one. It did not until 2026-08-26, and the gap was a §10.6 split-brain
/// rather than a policy choice: `sovereign daemon run` declares
/// [`LaneWarmth::Deferred`] and this one member read it as `Eager`, so a host
/// that had explicitly asked to reach `listening` promptly still blocked ~950 ms
/// on a model load. One declaration, two readings.
///
/// The deferred arm's degradation is not new and is already accepted in
/// `LaneWarmth`'s own words: until the model is warm the extractor returns no
/// entities, which is EXACTLY what a host with no GLiNER installed does —
/// retrieval falls back to cosine + MMR, "a degradation in ranking and never a
/// wrong answer". `LaneWarmth` says that about a ~1 GB JSON parse; this is a
/// ~950 ms load that is warm within ~1 s, well before a first query.
///
/// This is also what lets the desktop stop hand-rolling its own bootstrap: its
/// wiring WAS the deferred arm, written out by hand.
fn load_gliner(warmth: LaneWarmth) -> Option<Arc<dyn sovereign_core::traits::EntityExtractor>> {
    let model_id = sovereign_gliner::gliner_ner::DEFAULT_MODEL_ID;
    if !sovereign_gliner::gliner_ner::probe_model_available(model_id) {
        tracing::debug!(
            model = model_id,
            "runtime_recipe: GLiNER model not installed; entity-aware \
             retrieval disabled (falls back to cosine+MMR)"
        );
        return None;
    }
    match warmth {
        // Install now, warm behind. `new_default_deferred` cannot fail
        // synchronously — the thread logs a load error and leaves the
        // extractor permanently in the same fallback the `Eager` arm's `Err`
        // branch produces, so absence is reported identically on both paths.
        LaneWarmth::Deferred => {
            tracing::info!(
                model = model_id,
                "runtime_recipe: GLiNER entity extractor installed (background warm)"
            );
            Some(
                Arc::new(sovereign_gliner::gliner_ner::LazyGlinerExtractor::new_default_deferred())
                    as Arc<dyn sovereign_core::traits::EntityExtractor>,
            )
        }
        LaneWarmth::Eager => match sovereign_gliner::gliner_ner::GlinerExtractor::new_default() {
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
        },
    }
}

/// Probe `<indexes_dir>/<corpus_id>/wikipedia_graph.db` (or the columnar
/// atlas store) for each installed corpus and return the first graph that
/// opens cleanly.
async fn load_wikipedia_graph(
    engine: &corpus_engine::CorpusEngine,
    indexes_dir: &Path,
    scope: &LaneScope,
    progress: &dyn RecipeProgress,
) -> Option<Arc<dyn corpus_engine::WikipediaGraphApi>> {
    // Memory-pressure escape hatch. The graph is a 7M-edge sqlite mmap; on a
    // host already running the daemon, loading it twice has tipped past
    // available RAM in practice.
    if sovereign_tools::corpus::wiki_graph_disabled() {
        progress.note("Wiki graph:  disabled via SOVEREIGN_DISABLE_WIKI_GRAPH");
        return None;
    }
    // WIKIPEDIA_ATLAS_V2 W3: per corpus, prefer the columnar store
    // (atlas/articles.lance + edges.lance) over the SQLite graph — the shared
    // `corpus_engine::open_wikipedia_graph` gate.
    let infos = engine.installed_indexes().await.ok()?;
    let mut probed = 0usize;
    for info in infos {
        // A lane scoped to one corpus probes one corpus. Out-of-scope
        // corpora cannot contribute a graph this host could consult, so
        // opening theirs is work whose result is unreachable.
        if !scope.includes(&info.corpus_id) {
            continue;
        }
        probed += 1;
        if let Some(g) = corpus_engine::open_wikipedia_graph(indexes_dir, &info.corpus_id).await {
            return Some(g);
        }
    }
    if probed == 0 {
        progress.note("Wiki graph:  not loaded (out of lane scope)");
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
        // One reader, in `sovereign_tools::corpus` — this branch used to
        // carry its own copy of the parse (TOPOLOGY §10 phase 10).
        if let Some(n) = sovereign_tools::corpus::rerank_candidates_k_from_env() {
            cfg.candidates_k = n;
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

#[cfg(test)]
mod warmth_census {
    /// The state this makes unrepresentable: a lane member that ignores the
    /// warmth its host declared.
    ///
    /// `LaneWarmth` is a required `RecipeInputs` field, so a host cannot forget
    /// to STATE it — and until 2026-08-26 nothing made the recipe HONOUR it.
    /// `load_gliner` took no argument at all, so `sovereign daemon run`
    /// declared `Deferred` and still blocked ~950 ms on a model load. One
    /// declaration, two readings (ARCH §10.6) — and the reason the desktop kept
    /// a hand-rolled bootstrap, since its wiring WAS the deferred arm written
    /// out by hand.
    ///
    /// The compiler does NOT hold this: dropping the parameter and its argument
    /// together compiles clean and silently restores eager-always. Watched to
    /// fail by reverting `load_gliner(warmth)` to `load_gliner()`.
    #[test]
    fn every_deferrable_lane_member_honours_the_declared_warmth() {
        let src = include_str!("lib.rs");
        // Code only — the module docs legitimately discuss warmth in prose.
        let code: Vec<&str> = src
            .lines()
            .filter(|l| {
                let t = l.trim_start();
                !(t.starts_with("//") || t.starts_with('*'))
            })
            .collect();

        for member in ["load_meta_atlas", "load_gliner"] {
            let decl = code
                .iter()
                .find(|l| l.contains(&format!("fn {member}(")))
                .unwrap_or_else(|| {
                    panic!("{member} is gone — drop it here or say what replaced it")
                });
            assert!(
                decl.contains("warmth: LaneWarmth"),
                "{member} no longer takes the host's declared warmth, so a host \
                 asking to reach `listening` promptly will block on it anyway:\n  {decl}"
            );

            let calls: Vec<&&str> = code
                .iter()
                .filter(|l| l.contains(&format!("{member}(")) && !l.contains("fn "))
                .collect();
            assert!(
                !calls.is_empty(),
                "{member} is declared but never called — this census would be vacuous"
            );
            for c in calls {
                assert!(
                    c.contains("warmth"),
                    "a call to {member} drops the declared warmth:\n  {c}"
                );
            }
        }
    }

    /// The state this makes unrepresentable: a lane member that ignores the
    /// SCOPE its host declared.
    ///
    /// Same failure shape as the warmth census above and the same reason the
    /// compiler cannot hold it — dropping the parameter and its argument
    /// together compiles clean and silently restores load-everything-always.
    /// The cost of that regression is not a slow boot but a wrong claim: a
    /// lane sealed to a 316-chunk corpus spent 22.3 s of a 24.9 s startup on
    /// a 7.85M-edge graph and a 981 MB meta-atlas it could not consult, and
    /// nothing in the run said so.
    ///
    /// Watched to fail by reverting `load_cross_corpus_members(&mut lane,
    /// warmth, &scope, progress)` to drop `&scope`, and again by reverting
    /// `load_wikipedia_graph`'s parameter.
    #[test]
    fn every_cross_corpus_lane_member_honours_the_declared_scope() {
        let src = include_str!("lib.rs");
        let code: Vec<&str> = src
            .lines()
            .filter(|l| {
                let t = l.trim_start();
                !(t.starts_with("//") || t.starts_with('*'))
            })
            .collect();

        for member in ["load_cross_corpus_members", "load_wikipedia_graph"] {
            // Signatures here span lines, so the census reads the parameter
            // block rather than one line — the warmth census's single-line
            // form would pass vacuously on either of these.
            let start = code
                .iter()
                .position(|l| l.contains(&format!("fn {member}(")))
                .unwrap_or_else(|| {
                    panic!("{member} is gone — drop it here or say what replaced it")
                });
            let sig_end = code[start..]
                .iter()
                .position(|l| l.contains(')'))
                .map(|o| start + o)
                .unwrap_or(code.len() - 1);
            let sig = code[start..=sig_end].join(" ");
            assert!(
                sig.contains("scope: &LaneScope"),
                "{member} no longer takes the host's declared scope, so a lane \
                 sealed to one corpus loads every corpus again:\n  {sig}"
            );

            let calls: Vec<&&str> = code
                .iter()
                .filter(|l| l.contains(&format!("{member}(")) && !l.contains("fn "))
                .collect();
            assert!(
                !calls.is_empty(),
                "{member} is declared but never called — this census would be vacuous"
            );
            for c in calls {
                assert!(
                    c.contains("scope"),
                    "a call to {member} drops the declared scope:\n  {c}"
                );
            }
        }
    }

    /// `Sealed` must actually narrow, and `All` must actually not. A scope
    /// whose predicates both answered the same way would pass the census
    /// above while changing nothing.
    #[test]
    fn sealed_scope_admits_only_its_own_corpus() {
        use super::LaneScope;
        let all = LaneScope::All;
        assert!(all.includes("wikipedia"));
        assert!(all.includes("chaos-secret-agent"));
        assert!(all.spans_corpora());

        let sealed = LaneScope::Sealed("chaos-secret-agent".to_string());
        assert!(sealed.includes("chaos-secret-agent"));
        assert!(!sealed.includes("wikipedia"));
        assert!(
            !sealed.spans_corpora(),
            "a sealed lane has one corpus, so no member defined over pairs of \
             them can say anything"
        );
    }
}
