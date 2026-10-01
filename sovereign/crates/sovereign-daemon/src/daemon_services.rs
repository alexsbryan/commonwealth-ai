// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a host hands [`EmbeddedDaemon`](crate::daemon::EmbeddedDaemon) at
//! construction — as **one total value**, not seventeen slots punched in
//! afterwards.
//!
//! ## Why this type exists
//!
//! Until 2026-08-24 the daemon carried 17 `RwLock<Option<T>>` fields, filled
//! by 10 `set_*` and 7 `install_*_router` methods after construction. Nothing
//! forced a host to call them and nothing reported that it hadn't: a forgotten
//! `install_knowledge_view_http_router` was indistinguishable, from inside the
//! daemon, from a host that deliberately does not serve that route. The route
//! simply 404'd. 2¹⁷ representable configurations, of which four were real.
//!
//! ## How the variants were chosen — they weren't
//!
//! They are the output of the pair-independence pass in `quality/TOPOLOGY.md
//! §4`, run over every live construction site on 2026-08-24. For each pair of
//! the 17 slots: does any live path set one without the other? The slots fall
//! into five classes, and the classes fall onto three hosts.
//!
//! ```text
//!                           daemon run   desktop   svrn mesh create/join
//!   corpus_engine                Y           Y               .
//!   inference_provider           Y           Y               .
//!   embed_model                  Y*          Y*              .
//!   state_store                  Y†          Y               .
//!   mcp                          Y           Y*              .
//!   mesh/admin/reading routers   Y           Y               .
//!   project_http_router          Y           Y               .
//!   corpus_watch_http_router     Y*          Y*              .
//!   workflow_http_router         Y           Y               .
//!   provider_factory             Y           .               .
//!   mesh_store                   Y           .               .
//!   convergence_recorder         Y           .               .
//!   knowledge_view_http_router   Y           .               .
//!   solve_http_router            Y           .               .
//! ```
//!
//! `†` marks the one row the measurement itself changed: `daemon run` had no
//! store on 2026-08-24, and that hole is exactly where the two serving shapes
//! CROSSED rather than nested — it mounted `reading_http` while owning nothing
//! to resolve a conversation title with. daemon-convergence Phase 3 gave it
//! one, so the column is now a strict subset relation and
//! [`DaemonServices::Desktop`] is *nothing but* a [`ServingProfile`].
//!
//! `Y*` marks a slot whose absence is a *disk or probe failure*, never a
//! topology choice — those keep a named-absence type ([`EmbedAdvertisement`],
//! [`McpSurface`]) rather than a bare `Option`, because "this host does not
//! serve MCP" and "`notes.db` would not open" are different facts and §18.3
//! forbids collapsing them.
//!
//! A fourth column existed at the time of measurement and is deliberately
//! absent here: the desktop in `Local { source: Fresh | DesktopLegacy }`
//! installed the engine and the provider but none of the routers, because
//! `state.rs`'s `cli_setup_wiring` gated the whole HTTP surface on a
//! `ConfigSource` captured at *probe* time — before the setup wizard wrote
//! `config.toml`. It is not a fourth topology; it is one topology read at two
//! different moments. Collapsing it into [`Desktop`](DaemonServices::Desktop)
//! is what makes the constructor total.
//!
//! ## The rings
//!
//! The classes are not a flat parameter list — they are grouped by what their
//! absence COSTS, and each group is its own total sub-structure
//! ([`ServingCore`], [`ServingCapability`], [`HeadlessRails`]):
//!
//! | Ring | Absence costs | Members |
//! |---|---|---|
//! | CORE | cannot serve at all | `corpus_engine`, `inference_provider` |
//! | POLICY | serves *wrongly* | `SetupConfig` (bind, token, peer-inflight ceiling), `advertise_embed` |
//! | CAPABILITY | can do less | `mcp`, `project_http`, `corpus_watch_http`, `workflow_http`, `+knowledge_view_http` (`+solve_http` left for code at pb-meshapp-solve) |
//! | RAILS | a surface reports something untrue | `provider_factory`, `mesh_store`, `convergence_recorder` |
//!
//! `SetupConfig` sits on the daemon rather than in a variant because all three
//! shapes have one and `POST /v1/admin/reload` advances it at runtime.
//!
//! ## What is *not* here
//!
//! Three things stayed on the daemon because they are runtime state, not
//! construction inputs: `join_key_plaintext` (written by create/join/resume,
//! cleared on stop) and the two RPC-worker maps. And `setup_config` is a
//! plain non-`Option` field on the daemon: `SetupConfig::default()` is
//! byte-identical to every fallback `start_daemon` used to apply when the slot
//! was `None`, so "no config" was never a distinct state — only an unnamed one.

use std::sync::Arc;

use corpus_index::ingest_port::daemon::IngestPort;

use oicp_types::EmbedModelInfo;
use sovereign_core::registry::ToolRegistry;
use sovereign_core::traits::{InferenceProvider, StateStore};

use crate::admin_http::ProviderFactory;

/// Per-session MCP mount. When present, the daemon merges
/// `mcp_router::mcp_router(...)` into its client router so `/mcp`,
/// `/mcp/message` and `/mcp/stats` share the port with `/v1/*`.
#[derive(Clone)]
pub struct McpMount {
    pub tools: Arc<ToolRegistry>,
    /// svrn's own store: its memory notes (`/v1/notes*`, the dossier) and
    /// its MCP call log (pb-notes-memory). The same handle as the serving
    /// core's state store: one writer per data root.
    pub notes: Arc<sovereign_store::sqlite::SqliteStateStore>,
    /// Groups this process's tool calls in svrn's call log
    /// (e.g. `daemon-<uuid>`, `desktop-<uuid>`).
    pub session_id: String,
    /// The code program's tools, when a distribution composed code into
    /// this process: listed and called on the same `/mcp`, logged by code.
    /// `None`: this `/mcp` names `svrn code mcp` for a code tool.
    pub code: Option<Arc<dyn host_kit::mcp::McpMountedTools>>,
}

/// Whether `/mcp` is mounted, and — when it is not — *why*.
///
/// A bare `Option` conflated "this host serves no code-intelligence tools"
/// with "`notes.db` would not open", which are different operational facts
/// with different fixes (ARCH §18.3: absence is reported, never defaulted).
#[derive(Clone)]
pub enum McpSurface {
    Mounted(McpMount),
    /// The host could not build a tool mount. `reason` is rendered into the
    /// daemon's startup log so the missing `/mcp` attributes itself.
    Unavailable {
        reason: String,
    },
}

impl McpSurface {
    pub fn mount(&self) -> Option<&McpMount> {
        match self {
            Self::Mounted(m) => Some(m),
            Self::Unavailable { .. } => None,
        }
    }
}

/// Whether this node advertises an embedding model to mesh peers, and — when
/// it does not — *why*. Peers use this to decide whether collaborative
/// ingestion can be partitioned here, so silence and "probe failed" must not
/// look alike (ARCH §18.3).
#[derive(Clone)]
pub enum EmbedAdvertisement {
    Advertised(EmbedModelInfo),
    Unavailable { reason: String },
}

impl EmbedAdvertisement {
    pub fn info(&self) -> Option<&EmbedModelInfo> {
        match self {
            Self::Advertised(i) => Some(i),
            Self::Unavailable { .. } => None,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// The rings. A slot's ring is decided by what its ABSENCE COSTS, and each
// ring is its own total sub-structure — so a broken configuration in one
// ring cannot produce a half-built neighbour.
// ─────────────────────────────────────────────────────────────────────────

/// **Ring 1 — CORE.** Without a provider `/v1/chat/completions` has nothing
/// behind it, so it is not an `Option`. The engine is ingest's, composed by a
/// distribution (`process::HostedIngest`, pb-ingest-dial-daemon): `None` is a
/// svrn with no ingest program, where `/v1/knowledge/search` and
/// `/internal/knowledge/search` answer 503, gossip advertises no
/// `hosted_corpora`, and the ingest routes name the absence.
pub struct ServingCore {
    pub corpus_engine: Option<Arc<dyn IngestPort>>,
    /// Ingest's atlas port, composed beside the engine; `None` exactly when
    /// the engine is.
    pub atlas: Option<Arc<dyn corpus_engine_atlas_reader::ports::AtlasPort>>,
    /// The recipe authoring harness over that engine. `None` answers the
    /// harness route with a named 503; every production host composes it.
    pub recipe_harness: Option<Arc<dyn corpus_index::ingest_port::daemon::RecipeHarnessPort>>,
    /// `sovereign.db` at this daemon's data root — conversations, sessions,
    /// tiered-memory rows. CORE, not an optional extra: the reading surface
    /// resolves `conversation-history` chunks through it, and a turn cannot be
    /// titled, resumed or cancelled without it.
    ///
    /// It lived on the desktop variant until daemon-convergence Phase 3. That
    /// placement was the single crossing in an otherwise nesting lattice:
    /// `sovereign daemon run` served `reading_http` with no store, so every
    /// conversation chunk rendered title-less on a headless daemon — a defect
    /// the type reported as a legitimate topology. One writer per data root is
    /// what makes this safe, and the run lock (Phase 1, keyed on the data root)
    /// is what makes THAT true.
    pub state_store: Arc<dyn StateStore>,
    /// The provider that answers peers hitting `/v1/chat/completions`. The
    /// daemon holds it behind a lock only because `POST /v1/admin/reload`
    /// swaps it; a host installs it here, once, or not at all.
    pub inference_provider: Arc<dyn InferenceProvider>,
    /// svrn's OpenAI face over that provider, handed in with it
    /// (pb-serve-ranks): the adapter over the router that ranks here, or the
    /// relay to the server svrn dials. `None` answers the OpenAI routes with
    /// their named 503.
    pub local_inference: Option<Arc<dyn crate::state::LocalInferenceService>>,
    /// The node's in-flight gauge, created by the bootstrap *before* the
    /// `InferenceRouter` and handed to both the router and `AppState` — one
    /// atomic the provider's guards write and gossip reads
    /// (`quality/DAEMON_CORE.md` §4.2 "Where an install slot breaks a cycle").
    /// `None` when the provider is not a router (fixtures, a `NullProvider`);
    /// the absence is what gossip publishes as `current_in_flight: None`.
    pub in_flight_gauge: Option<sovereign_core::in_flight::LocalInFlightGauge>,
    /// The thing that ANSWERS — routing, retrieval, tools, synthesis.
    ///
    /// CORE, and the field `quality/TOPOLOGY.md` §3.5 turns on: "DAEMON — the
    /// only process that assembles a Runtime". Before 2026-08-25 every serving
    /// daemon held the three fields above and no `Runtime`, so a turn could
    /// only be served by whichever HOST had built one around its own copy of
    /// the recipe — and the type reported that as a legitimate topology.
    ///
    /// Not an `Option`, for the same reason `state_store` is not: a serving
    /// daemon that cannot answer a question is not a shape anybody deploys,
    /// and an `Option` here would put the crossing back that Phase 3 closed.
    /// Both serving variants carry one, so the lattice still nests.
    pub runtime: Arc<sovereign_core::runtime::Runtime>,
    /// The insight clip/search service over the SAME `sovereign.db`
    /// connection `state_store` holds (sv-surface rung 6). `None` on a
    /// commission that never built one — a mesh-admin daemon answers 503
    /// with that named reason on `/v1/insights/*`, which is a different
    /// fact from "the route is not mounted" (ARCH §18.3). The service
    /// itself is sovereign-core's, constructed by the host that owns the
    /// concrete store handle; this field is the door, not a second decider.
    pub insights: Option<Arc<sovereign_core::insight::InsightService>>,
    /// The recipe-author project layer — `features.db` at this daemon's
    /// data root (sv-surface D6), reached through ingest's recipe-project
    /// port (pb-ingest-rehome-daemon). `Err` carries why there is none —
    /// the store would not open, or no ingest program is composed — which
    /// `features_http` renders as a named 503; the same warn-and-skip
    /// posture `sovereign daemon run` already took, visible in the type
    /// instead of only in a log line.
    ///
    /// A serving daemon without an authoring surface is a real shape, and
    /// "the file would not open" must stay a different fact from "this
    /// route is not mounted" (ARCH §18.3). The store itself is
    /// `sovereign-recipe-author`'s; this field is the door, not a second
    /// decider.
    pub features: Result<Arc<dyn sovereign_contracts::recipe::project::RecipeProjectPort>, String>,
}

/// **Ring 2 — CAPABILITY.** What the daemon can *do* beyond answering: the
/// tool surface and the host-built routes. Composed as one unit so a host
/// adds or declines a capability in one place rather than scattering four
/// installs across its bootstrap.
///
/// The three routers that are pure functions of `Arc<EmbeddedDaemon>` — mesh,
/// admin and reading — are deliberately NOT here. The daemon builds those
/// itself from its own `Weak<Self>` at start, so a serving host cannot forget
/// them; that is what dissolves the measured desktop-vs-daemon router delta.
///
/// `corpus_watch_http` takes no arguments and reads the
/// `watched_folder_runtime` singleton at request time. When that singleton was
/// never installed its handlers answer 503 with a named reason, which is why
/// mounting it unconditionally is strictly better than the 404 an unmounted
/// router produced (ARCH §18.3).
///
/// `workflow_http` is the same shape: a host-built router
/// (`sovereign-workflow-host::workflow_http::workflow_http_router`) this
/// crate treats as opaque — sovereign-mesh must not depend on the workflow
/// crates, so the capability crosses as an `axum::Router` and the job
/// runtime lives behind it in the crate both hosts already link (sv-surface
/// rung 5, 2026-09-09).
pub struct ServingCapability {
    pub mcp: McpSurface,
    pub project_http: axum::Router,
    pub corpus_watch_http: axum::Router,
    pub workflow_http: axum::Router,
    /// Code's editor door when a distribution composed code here
    /// (pb-meshapp-rest). Not a host router: it reaches every general client
    /// surface through `NodeSeed::edit_door`, not the operator's merge.
    pub edit_door: Option<axum::Router>,
}

/// Rings 1–3 as every serving daemon has them, whichever host runs it. The
/// two serving variants differ only *outside* this struct.
pub struct ServingProfile {
    pub core: ServingCore,
    pub capability: ServingCapability,
    /// **Ring 3 — POLICY/ADVERTISEMENT.** What this node tells peers about
    /// itself. Explicit in both directions: a node that advertises no embed
    /// model says so with a reason, because a peer reading silence would
    /// otherwise fall back to a default model id and partition collaborative
    /// ingestion here anyway.
    pub advertise_embed: EmbedAdvertisement,
    /// The node's mesh as svrn reads it: cw-rails' roster and reach door,
    /// composed by the distribution (pb-mesh-exit-transport), or their
    /// absence on svrn alone.
    pub mesh: crate::hosted_mesh::MeshAccess,
}

/// Three handles `sovereign daemon run` must share with writers that live
/// outside the daemon, or a surface reports something untrue.
///
/// - `provider_factory` — without it `POST /v1/admin/reload` cannot honour a
///   `models.*` change. The desktop has never had one; that is why this rail
///   is on the headless variant and the desktop's reload names its profile in
///   the refusal instead of reporting a missing installation.
/// - `mesh_store` — the ONE `RailsKv` (five-programs fp-88): the work atlas,
///   the notes sink and poller, and `AppState`'s KV port all dial cw-rails
///   through it, so their writes cross the mesh from the rails daemon.
/// - `convergence_recorder` — the ONE convergence record the notes publish
///   sink, the ingest poller and `/status` all stamp and read. A second copy
///   would let the status section disagree with the sink.
///
/// Both are held as ports — the dial, and the mesh ADAPTER from
/// [`sovereign_mesh::peer_adapter`] — never the commonwealth types: the daemon
/// builds them, hands them here, and talks to them through `sovereign-contracts::peer`'s ports, so its own
/// bootstrap names no `commonwealth-*` type at all (cw-lift 3b).
pub struct HeadlessRails {
    pub provider_factory: Arc<dyn ProviderFactory>,
    pub mesh_store: Arc<dyn sovereign_contracts::peer::ReplicatedKv>,
    pub convergence_recorder: Arc<crate::convergence::MeshConvergence>,
}

// `DesktopServices` WAS HERE, and is deleted (daemon-convergence Phase 3).
//
// Once `state_store` moved into `ServingCore` the struct held exactly one
// field — a `ServingProfile` — so it was a name wrapped around a name. The
// deletion is the point rather than tidying: `Desktop(Box<ServingProfile>)`
// states in the type what §3.5 could previously only assert in prose, that
// the desktop's daemon is a serving profile and NOTHING else, and that
// Headless is that same profile plus rails. The variants nest, visibly.

/// Everything `sovereign daemon run` supplies **beyond** a [`ServingProfile`].
///
/// This is the nesting made literal: Headless = Desktop + rails + one route.
/// (`/v1/solve/jobs*` was the second until pb-meshapp-solve moved the solver
/// to code, whose routes arrive in `project_http`.)
pub struct HeadlessServices {
    pub serving: ServingProfile,
    pub rails: HeadlessRails,
    /// Ring 2 extension — `POST /v1/knowledge/landscape_digest`. Hosted only
    /// here because only this bootstrap owns a `KnowledgeViewManager`.
    pub knowledge_view_http: axum::Router,
}

/// Which host built this daemon, and everything that host supplies.
///
/// See the module docs for how the three variants were derived. They are not
/// a taxonomy someone picked; they are the three live construction sites, and
/// the field placement is the measured pair-independence result.
pub enum DaemonServices {
    /// `svrn mesh create` / `svrn mesh join` when no daemon is listening
    /// (`sovereign-cli-llm/src/mesh_cmd.rs`). A one-shot: it mutates mesh
    /// membership, prints, and the process exits. It serves no knowledge, no
    /// inference and no host routes — and that emptiness is the shape, not a
    /// set of holes.
    ///
    /// The payload is a [`MeshAdminWitness`], which carries only the node's
    /// mesh and exists so this variant cannot be *named into being* outside
    /// this crate.
    /// See that type for why a bare variant was the last open door.
    MeshAdmin(MeshAdminWitness),
    /// The desktop's in-process daemon (`Local` bootstrap mode) — a
    /// [`ServingProfile`] and nothing more.
    Desktop(Box<ServingProfile>),
    /// `sovereign daemon run`.
    Headless(Box<HeadlessServices>),
}

/// Proof that a `MeshAdmin` daemon came out of [`assemble`].
///
/// # Why a variant needed a witness at all
///
/// `DaemonServices::MeshAdmin` was a bare unit variant, and a bare variant is
/// constructible wherever the enum is nameable. `sovereign-mesh` re-exports
/// `DaemonServices` publicly, so until 2026-08-25 *any* crate in the workspace
/// could write `DaemonServices::MeshAdmin` and commission a daemon without
/// going through the one exhaustive `Launch` match. Phase 4b shut the two
/// composite doors by making [`DaemonServices::desktop`] and
/// [`DaemonServices::headless`] `pub(crate)`; there is no such thing as a
/// `pub(crate)` enum variant, so this one stayed open and was covered by a
/// source-scanning test (`tests/launch_assembler_census.rs`) instead.
///
/// A test that greps for a string is not an invariant (ARCH §7.1, §18.1) — and
/// that particular test matched raw text, so a file merely *mentioning*
/// `assemble(` in a comment satisfied it. This type replaces the grep with the
/// compiler: the single field is private, there is no public constructor and
/// no `Default`, so the only `MeshAdminWitness` that can exist is the one
/// [`assemble`] mints.
///
/// Matching is deliberately still open. Reading which shape a daemon has is
/// not the hazard; *deciding* it outside the assembler is. So
/// `matches!(services, DaemonServices::MeshAdmin(_))` compiles anywhere, and
/// `DaemonServices::MeshAdmin(..)` cannot be built anywhere but here.
pub struct MeshAdminWitness {
    /// The node's mesh as the distribution composed it, so the wizard's join
    /// child reads cw-rails' roster for its venues (pb-mesh-exit-transport).
    mesh: crate::hosted_mesh::MeshAccess,
}

impl std::fmt::Debug for MeshAdminWitness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MeshAdminWitness")
    }
}

impl DaemonServices {
    // `pub` -> `pub(crate)` (daemon-convergence Phase 4b). Nothing outside this
    // crate composes a serving daemon any more; hosts hand parts to
    // [`assemble`] and it decides the shape. Phase 7 closed the last door on
    // 2026-08-25: `MeshAdmin` now carries a [`MeshAdminWitness`] whose only
    // mint is [`assemble`], so all three variants are unreachable from outside.
    pub(crate) fn mesh_admin(mesh: crate::hosted_mesh::MeshAccess) -> Self {
        Self::MeshAdmin(MeshAdminWitness { mesh })
    }

    pub(crate) fn desktop(serving: ServingProfile) -> Self {
        Self::Desktop(Box::new(serving))
    }

    pub(crate) fn headless(services: HeadlessServices) -> Self {
        Self::Headless(Box::new(services))
    }

    /// Stable name for logs and `/status`. Closed set — ARCH §2.1.
    pub fn label(&self) -> &'static str {
        match self {
            Self::MeshAdmin(_) => "mesh-admin",
            Self::Desktop(_) => "desktop",
            Self::Headless(_) => "headless",
        }
    }

    /// True for the two variants that serve a host HTTP surface. The
    /// mesh-admin one-shot mounts nothing beyond the base client/internal
    /// routers and `GET /v1/mesh/venues`.
    pub fn serves_host_surface(&self) -> bool {
        !matches!(self, Self::MeshAdmin(_))
    }

    /// Rings 1-3 as this variant carries them; `None` on the mesh-admin
    /// one-shot, which has no serving role at all.
    // SEVEN ACCESSORS WERE DELETED HERE (2026-08-24, daemon-convergence).
    //
    // `corpus_engine`, `inference_provider`, `embed`, `mcp`,
    // `provider_factory`, `mesh_store` and `convergence_recorder` were each a
    // one-line `self.serving().map(..)` or `self.rails().map(..)`. The fields
    // they reached are NOT optional one level down, so the `Option` they
    // returned carried no information — it was an artifact of the accessor
    // sitting on the enum instead of on the ring struct.
    //
    // Two costs, and the second is the one that mattered. The variants
    // collapsed 2^17 -> 3, but every call site was still handed a 2^9
    // question, so a reader could not tell from the type which invocation
    // guaranteed what. And they STACKED: `mcp()` returned
    // `Option<&McpSurface>` where `McpSurface` is itself a two-state
    // absence-with-reason (§18.3), so `services.mcp().and_then(|m| m.mount())`
    // put a meaningless outer `Option` on top of a meaningful inner one.
    //
    // The three below survive because each names a REAL fork a reader has to
    // know about. Callers now match once on one of them and read plain fields
    // off `&ServingProfile` / `&HeadlessRails`.

    /// The node's mesh as the distribution composed it, on every shape.
    pub fn mesh(&self) -> &crate::hosted_mesh::MeshAccess {
        match self {
            Self::MeshAdmin(w) => &w.mesh,
            Self::Desktop(serving) => &serving.mesh,
            Self::Headless(h) => &h.serving.mesh,
        }
    }

    pub fn serving(&self) -> Option<&ServingProfile> {
        match self {
            Self::MeshAdmin(_) => None,
            Self::Desktop(serving) => Some(serving),
            Self::Headless(h) => Some(&h.serving),
        }
    }

    // `state_store()` WAS HERE, and is deleted (daemon-convergence Phase 3).
    //
    // It was the third and last REAL fork on this enum, and Phase 3 is what
    // made it artifactual: with the store in `ServingCore`, both serving
    // variants carry one, so the accessor became `self.serving().map(..)`
    // over a field that is not optional one level down — the exact shape of
    // the seven deleted above. Accessors 3 -> 2. Callers read
    // `services.serving()?.core.state_store` and land on a struct field.

    /// The headless-only rails, or `None` on a variant that declares it has
    /// none. Callers must name the variant in any refusal they derive from a
    /// `None` here — nothing is missing, this shape has no rails.
    pub fn rails(&self) -> Option<&HeadlessRails> {
        match self {
            Self::Headless(h) => Some(&h.rails),
            Self::MeshAdmin(_) | Self::Desktop(_) => None,
        }
    }

    /// Names of [`Self::host_routers`], same order — so the daemon's startup
    /// log says which surfaces it actually serves rather than leaving a
    /// reader to infer it from a 404.
    pub fn host_router_names(&self) -> Vec<&'static str> {
        match self {
            Self::MeshAdmin(_) => Vec::new(),
            Self::Desktop(_) => vec!["project_http", "corpus_watch_http", "workflow_http"],
            Self::Headless(_) => vec![
                "project_http",
                "corpus_watch_http",
                "workflow_http",
                "knowledge_view_http",
            ],
        }
    }

    /// Every host-built router this variant mounts, in merge order.
    pub fn host_routers(&self) -> Vec<axum::Router> {
        match self {
            Self::MeshAdmin(_) => Vec::new(),
            Self::Desktop(serving) => vec![
                serving.capability.project_http.clone(),
                serving.capability.corpus_watch_http.clone(),
                serving.capability.workflow_http.clone(),
            ],
            Self::Headless(h) => vec![
                h.serving.capability.project_http.clone(),
                h.serving.capability.corpus_watch_http.clone(),
                h.serving.capability.workflow_http.clone(),
                h.knowledge_view_http.clone(),
            ],
        }
    }
}

/// What a host supplies to [`assemble`] — the parts, without the shape.
///
/// The host knows what it BUILT; only [`assemble`] decides what that composes
/// into, and only for the invocation this process actually is.
pub enum LaunchParts {
    /// This invocation serves nothing. `svrn mesh create` / `svrn mesh join`
    /// mutate membership, print, and exit — the emptiness is the shape. `mesh`
    /// is the node's mesh as the distribution composed it.
    Admin {
        mesh: crate::hosted_mesh::MeshAccess,
    },
    /// A serving daemon's parts. `headless` is `Some` exactly on the
    /// `sovereign daemon run` bootstrap, which is the only one that owns a
    /// `ProviderFactory`, a shared mesh store, a convergence recorder and a
    /// `KnowledgeViewManager`.
    Serving {
        serving: ServingProfile,
        headless: Option<HeadlessExtras>,
    },
}

/// The parts only `sovereign daemon run` has. Named as one value so "this host
/// is headless" is a single question rather than four independent `Option`s
/// that could disagree.
pub struct HeadlessExtras {
    pub rails: HeadlessRails,
    pub knowledge_view_http: axum::Router,
}

/// Why a launch mode and a set of parts could not be composed.
///
/// A refusal, never a default (ARCH §18.3): substituting a plausible variant
/// here would produce a daemon serving routes this invocation was never meant
/// to serve, which is the entire hazard class this program exists to close.
#[derive(Debug)]
pub enum AssemblyRefusal {
    /// A launch mode that assembles no daemon was handed daemon parts.
    NotAnAssembler { launch: &'static str },
    /// The parts do not match the shape this launch mode assembles.
    Mismatch {
        launch: &'static str,
        wanted: &'static str,
        got: &'static str,
    },
}

impl std::fmt::Display for AssemblyRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAnAssembler { launch } => write!(
                f,
                "launch mode `{launch}` assembles no daemon runtime — three of \
                 the eight do (daemon/worker, desktop, the mesh verb)"
            ),
            Self::Mismatch {
                launch,
                wanted,
                got,
            } => write!(
                f,
                "launch mode `{launch}` assembles {wanted}, but was handed {got}"
            ),
        }
    }
}

impl std::error::Error for AssemblyRefusal {}

/// **THE ASSEMBLER — the one exhaustive match over [`Launch`] that constructs
/// anything** (`quality/TOPOLOGY.md` §10, Falsifier 3; the middle file of the
/// acceptance criterion's three-file spine).
///
/// `Launch::parse` answers *what this process is*. This answers *what that
/// invocation assembles*. Eight ways to start; three of them build a daemon
/// runtime; three assembled shapes. Both numbers are visible here, in one
/// match, and adding a `Launch` variant makes the compiler walk this function.
///
/// It is deliberately NOT a builder and does not construct the parts: a host
/// still opens its own corpus engine and provider, because those need the
/// host's own I/O. What moves here is the DECISION — which variant this
/// invocation is allowed to be — so that the four sites which used to answer
/// it independently now ask one place, and the illegal pairs (a desktop launch
/// carrying headless rails; a verb launch carrying a serving profile) are
/// refused rather than silently accepted.
pub fn assemble(
    launch: &sovereign_contracts::launch::Launch,
    parts: LaunchParts,
) -> Result<DaemonServices, AssemblyRefusal> {
    use sovereign_contracts::launch::Launch;
    let name = launch.as_str();
    match launch {
        // `sovereign daemon run`, and the desktop's supervised child, which is
        // the identical entry (`--daemon-child` IS `daemon run`; pinned by
        // `Launch::parse`'s own tests).
        Launch::Daemon { .. } => match parts {
            LaunchParts::Serving {
                serving,
                headless: Some(extras),
            } => Ok(DaemonServices::headless(HeadlessServices {
                serving,
                rails: extras.rails,
                knowledge_view_http: extras.knowledge_view_http,
            })),
            LaunchParts::Serving { headless: None, .. } => Err(AssemblyRefusal::Mismatch {
                launch: name,
                wanted: "a headless daemon (rails + knowledge-view)",
                got: "a serving profile with no rails",
            }),
            LaunchParts::Admin { .. } => Err(AssemblyRefusal::Mismatch {
                launch: name,
                wanted: "a headless daemon",
                got: "mesh-admin parts",
            }),
        },

        // The desktop's in-process daemon: a serving profile and nothing more.
        // It has never carried a provider factory, a shared mesh store or a
        // convergence recorder, and since Phase 3 it is not distinguished by a
        // state store either — so the shape it assembles is exactly
        // `ServingProfile`.
        Launch::Desktop => match parts {
            LaunchParts::Serving {
                serving,
                headless: None,
            } => Ok(DaemonServices::desktop(serving)),
            LaunchParts::Serving {
                headless: Some(_), ..
            } => Err(AssemblyRefusal::Mismatch {
                launch: name,
                wanted: "a serving profile",
                got: "headless rails, which the desktop has never had",
            }),
            LaunchParts::Admin { .. } => Err(AssemblyRefusal::Mismatch {
                launch: name,
                wanted: "a serving profile",
                got: "mesh-admin parts",
            }),
        },

        // `svrn mesh create` / `svrn mesh join` reaching this far means no
        // daemon was listening, so the verb builds a one-shot that mutates
        // membership and exits. `AdminJoin` is the setup wizard's join as a
        // child process: the same admin shape, serving until stopped.
        Launch::Verb { .. } | Launch::AdminJoin { .. } => match parts {
            LaunchParts::Admin { mesh } => Ok(DaemonServices::mesh_admin(mesh)),
            LaunchParts::Serving { .. } => Err(AssemblyRefusal::Mismatch {
                launch: name,
                wanted: "a mesh-admin one-shot",
                got: "a serving profile",
            }),
        },

        // The remaining five assemble nothing. Worker mode is its own
        // `sovereign-pod-worker` binary (pb-pods-worker).
        Launch::ComputeChild { .. }
        | Launch::RpcWorker { .. }
        | Launch::SetupProbe { .. }
        | Launch::Worker { .. }
        | Launch::Smoketest { .. }
        | Launch::Bare => Err(AssemblyRefusal::NotAnAssembler { launch: name }),
    }
}

/// Cheap stand-ins so a test can build every variant without loading a model.
/// `#[cfg(test)]`: nothing outside this crate's unit tests can reach them, so
/// no production path can obtain a services value it did not assemble itself.
#[cfg(test)]
#[path = "daemon_services/fixtures_tests.rs"]
pub(crate) mod fixtures;

#[cfg(test)]
mod tests;
