// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared test helpers for sovereign-mesh integration tests.
//!
//! Used by tests under `tests/*.rs` via `mod common;`. Each helper
//! is intentionally small and parameterizable. Premature flexibility
//! is more expensive than a few duplicated lines per ARCH §10.3, so
//! the bar to add a knob here is "two callers need it" not "one
//! caller might".
//!
//! Rust's integration-test layout treats `tests/common/mod.rs` as a
//! shared module — NOT a separate test binary the way `tests/common.rs`
//! would be. Each consumer adds `mod common;` at the top of their
//! test file.

#![allow(dead_code)]
// Every test binary uses a different subset of these helpers; the
// `dead_code` lint would otherwise fire per-binary on the unused ones.

use std::sync::Arc;

use axum::Router;

use kernel_types::NodeId;

use corpus_index::ingest_port::daemon::{IngestPort, RecipeHarnessPort};
use corpus_index::ingest_port::double::{IngestPortDouble, RecipeHarnessDouble};
use sovereign_contracts::recipe::project::RecipeProjectPort;

pub use sovereign_daemon::double::ledger_double;
pub use sovereign_daemon::double::{
    empty_capabilities, id_to_hex, keyed_member, member, peer_row, roster, spawn_router,
    tie_as_cw_rails, StaticRoster,
};
pub mod service_double;
pub mod work_rails;

// ── Corpus layout ───────────────────────────────────────────────

/// One corpus's layout on one node. `corpus_index::corpus::Corpus` is the ONE speller
/// of `<index_dir>/<id>` and `<id>-partition-<node>`, and `cargo xtask
/// layout-gate` is what keeps it that way — a test that retypes the infix is a
/// second answer to where a partition lives.
pub fn corpus_at(
    index_dir: impl Into<std::path::PathBuf>,
    id: &str,
) -> corpus_index::corpus::Corpus {
    corpus_index::corpus::Corpus::named(index_dir, id).expect("a non-empty corpus id")
}

// ── The verified peer identity on the internal plane ────────────

/// The tie a test installs with [`tie_as_cw_rails`] and presents with
/// [`cw_rails_stamp`].
pub const TIE: &str = "0123456789abcdef-test-origin-tie";

/// A roster the test owns, holding `self_id` as `self`, and the seed that
/// hands it to the daemon: cw-rails' roster through the membership port
/// (pb-mesh-exit-transport).
pub fn roster_seed(
    self_id: NodeId,
    mesh_name: &str,
) -> (Arc<StaticRoster>, sovereign_daemon::state::FabricSeed) {
    let owned = Arc::new(StaticRoster::new(mesh_name, vec![member(self_id, "self")]));
    let seed = sovereign_daemon::state::FabricSeed {
        membership: Some(owned.clone()),
        ..Default::default()
    };
    (owned, seed)
}

/// A daemon state over `roster`, as cw-rails' roster through the port.
pub fn state_over_roster(
    self_id: NodeId,
    mesh_name: &str,
    rows: Vec<sovereign_contracts::membership::MembershipEntry<mesh_reach::PeerContact>>,
) -> sovereign_daemon::state::AppState {
    sovereign_daemon::state::AppState::new_with_platform_and_engine_and_gauge_and_fabric(
        self_id,
        None,
        None,
        sovereign_daemon::state::FabricSeed {
            membership: Some(roster(mesh_name, rows)),
            ..Default::default()
        },
    )
}

/// Name `id` in `roster` with a verified key, so a request stamped by
/// [`cw_rails_stamp`] with that key resolves to this member.
///
/// A member whose row carries no key is a member this node cannot identify on
/// the wire, and `internal_principal` resolves such a caller `Unverified` by
/// design — so a test that wants attribution says which key the roster knows,
/// as cw-rails' roster does.
pub fn name_member_with_key(roster: &StaticRoster, id: NodeId, name: &str, pubkey: [u8; 32]) {
    roster.insert(keyed_member(id, name, pubkey));
}

/// Stamp a request exactly as cw-rails stamps a member's forward to svrn's
/// registered peer routes: the verified identity triple plus the registration
/// tie (`kernel_types::member::ORIGIN_TIE_HEADER`). The state under test must
/// hold [`TIE`] (`tie_as_cw_rails(&state, TIE)`).
///
/// A bare `X-Node-Id` header is no identity behind the internal router — it is
/// what a caller TYPES, and `internal_principal` strips it — so a test that
/// wants to be attributed presents what cw-rails presents.
pub fn cw_rails_stamp(
    req: reqwest::RequestBuilder,
    name: &str,
    id: NodeId,
    pubkey: [u8; 32],
) -> reqwest::RequestBuilder {
    req.header("X-Mesh-Member", name)
        .header("X-Mesh-Node", id.to_string())
        .header("X-Mesh-Pubkey", hex::encode(pubkey))
        .header(kernel_types::member::ORIGIN_TIE_HEADER, TIE)
}

pub use sovereign_contracts::double::TestProvider;

// ── Corpus fixtures ─────────────────────────────────────────────

/// The embedding width every fixture index and mock embed fn in this
/// crate's e2e tests uses. One number, one name.
pub const FIXTURE_EMBED_DIM: usize = 8;

/// The daemon's corpus handle over a tempdir, with `indexes/` and
/// `recipes/` already made and a mock embed fn: ingest's port double, so a
/// test that only slots a handle builds no engine
/// (pb-ingest-dial-daemon-tests-slot). A route that reads through it gets
/// the double's refusal, naming the method.
///
/// `ServingCore.corpus_engine` is not an `Option`, so every serving
/// fixture needs one whether or not its routes read it.
pub fn engine_at(tmp: &tempfile::TempDir) -> Arc<IngestPortDouble> {
    let indexes = tmp.path().join("indexes");
    let recipes = tmp.path().join("recipes");
    std::fs::create_dir_all(&indexes).expect("fixture indexes dir");
    std::fs::create_dir_all(&recipes).expect("fixture recipes dir");
    Arc::new(
        IngestPortDouble::new()
            .with_index_dir(indexes)
            .with_embed_fn(Arc::new(|_t: &str| {
                Box::pin(async { Ok(vec![0.0_f32; FIXTURE_EMBED_DIM]) })
            })),
    )
}

/// Ingest's port double over the fixture indexes under `indexes`, reading
/// them with the leaf's own reader: the listings through `FsIndexSource`,
/// the opens through `CorpusIndex::open` — what the engine delegates both
/// to, so a test that reads fixture indexes builds no engine
/// (pb-ingest-dial-daemon-tests-reads). Chain more programming onto it.
pub fn reading_double(
    indexes: std::path::PathBuf,
    embed: corpus_index::types::EmbedFn,
) -> IngestPortDouble {
    IngestPortDouble::new()
        .with_index_dir(indexes)
        .listing_indexes_under_index_dir()
        .opening_indexes_under_index_dir()
        .with_embed_fn(embed)
}

/// A fresh mesh-shared index under `indexes/<corpus_id>`, on the
/// embedding shape [`engine_at`] mints.
///
/// The seven arguments were byte-identical in `atlas_surface_e2e`,
/// `meshapp_surface_e2e` and `conv_surface_e2e` — three copies of one
/// corpus's identity, which is three places for the embedding model or
/// the licence to drift apart (ARCH §10.6).
pub async fn fixture_index(
    indexes: &std::path::Path,
    corpus_id: &str,
) -> corpus_index::index::CorpusIndex {
    corpus_index::index::CorpusIndex::create(
        &indexes.join(corpus_id),
        corpus_id,
        "Governance",
        "qwen3-embedding-0.6b",
        FIXTURE_EMBED_DIM,
        /* mesh_sharing */ true,
        "CC-BY-NC",
    )
    .await
    .expect("fixture index creates")
}

// ── Daemon services fixtures ────────────────────────────────────

/// A `DaemonServices::Desktop` around `engine` — the smallest variant that
/// carries a corpus engine, which is what the reading and storage e2e tests
/// need. Everything else is the honest empty value: no MCP mount, no embed
/// advertisement, empty host routers.
///
/// Tests that need no engine use `mesh_admin_services()` directly. There
/// is deliberately no "daemon with only an engine" shortcut: that shape is not
/// one any host builds, and offering it would put back a configuration nobody
/// serves.
pub fn desktop_services_with_engine(
    engine: Arc<dyn IngestPort>,
) -> sovereign_daemon::DaemonServices {
    desktop_services(DesktopParts::new(engine))
}

/// Everything the seven `desktop_services_with_*` fixtures vary, in one
/// struct. [`DesktopParts::new`] is the honest empty daemon — a stub
/// runtime over its own in-memory store, no MCP mount, no insight
/// service, no feature store — and each fixture below names the ONE or
/// two fields it is about.
///
/// Seven copies of the `assemble` call is seven places for the shape to
/// drift, and the fields that differ were buried in thirty identical
/// lines each (ARCH §10.6). They are named here instead.
pub struct DesktopParts {
    pub engine: Arc<dyn IngestPort>,
    /// Ingest's atlas port beside the engine: an unprogrammed double, so a
    /// test that reaches an atlas method it did not program fails naming it.
    pub atlas: Arc<dyn corpus_engine_atlas_reader::ports::AtlasPort>,
    pub recipe_harness: Arc<dyn RecipeHarnessPort>,
    pub provider: Arc<dyn sovereign_contracts::traits::InferenceProvider>,
    pub store: Arc<dyn sovereign_contracts::traits::StateStore>,
    pub runtime: Arc<sovereign_core::runtime::Runtime>,
    pub insights: Option<Arc<sovereign_core::insight::InsightService>>,
    pub features: Result<Arc<dyn RecipeProjectPort>, String>,
    pub mcp: sovereign_daemon::McpSurface,
}

impl DesktopParts {
    /// The smallest serving desktop that still carries a corpus engine.
    pub fn new(engine: Arc<dyn IngestPort>) -> Self {
        Self {
            engine,
            atlas: Arc::new(corpus_engine_atlas_reader::ports::double::AtlasPortDouble::new()),
            recipe_harness: Arc::new(RecipeHarnessDouble::default()),
            provider: Arc::new(TestProvider::new()),
            store: Arc::new(sovereign_store::memory::InMemoryStateStore::new()),
            runtime: stub_runtime(Arc::new(TestProvider::new()), None),
            insights: None,
            features: Err(sovereign_daemon::features_http::NO_FEATURES_DB.to_string()),
            mcp: sovereign_daemon::McpSurface::Unavailable {
                reason: "test fixture: no tool registry".into(),
            },
        }
    }

    /// The `/mcp` mount `EmbeddedDaemon::notes_store` reads svrn's real
    /// store from.
    pub fn mounted(
        mut self,
        tools: Arc<sovereign_contracts::ToolRegistry>,
        notes: Arc<sovereign_store::sqlite::SqliteStateStore>,
    ) -> Self {
        self.mcp = sovereign_daemon::McpSurface::Mounted(sovereign_daemon::McpMount {
            tools,
            notes,
            session_id: "test-fixture".into(),
            code: None,
        });
        self
    }
}

/// Commission a serving `Launch::Desktop` from `parts`.
///
/// Through THE assembler, like every production site — a fixture that
/// composed a variant directly would be the one place able to build a
/// shape no launch can produce, which is exactly what Falsifier 3
/// forbids. This is the only site in the fixtures that names
/// `ServingProfile`.
pub fn desktop_services(parts: DesktopParts) -> sovereign_daemon::DaemonServices {
    sovereign_daemon::assemble(
        &sovereign_contracts::launch::Launch::Desktop,
        sovereign_daemon::LaunchParts::Serving {
            headless: None,
            serving: sovereign_daemon::ServingProfile {
                core: sovereign_daemon::ServingCore {
                    recipe_harness: Some(parts.recipe_harness),
                    corpus_engine: Some(parts.engine),
                    atlas: Some(parts.atlas),
                    local_inference: Some(service_double::ProviderService::new(Arc::clone(
                        &parts.provider,
                    ))),
                    inference_provider: parts.provider,
                    in_flight_gauge: None,
                    state_store: parts.store,
                    runtime: parts.runtime,
                    insights: parts.insights,
                    features: parts.features,
                },
                capability: sovereign_daemon::ServingCapability {
                    mcp: parts.mcp,
                    project_http: Router::new(),
                    corpus_watch_http: Router::new(),
                    workflow_http: Router::new(),
                    edit_door: None,
                },
                advertise_embed: sovereign_daemon::EmbedAdvertisement::Unavailable {
                    reason: "test fixture: no embed probe".into(),
                },
                mesh: sovereign_daemon::hosted_mesh::MeshAccess::absent(),
            },
        },
    )
    .expect("Launch::Desktop assembles a serving profile with no rails")
}

/// A serving desktop commission whose `Runtime` is the caller's — the
/// fixture for the two routes that READ that runtime's own registers
/// (`turn_extras_http`: the skill registry, the last turn's provenance
/// frame). The three fixtures above all mint their own `stub_runtime`,
/// which cannot carry a registered skill or a captured frame.
///
/// Assembled through THE assembler for the reason the siblings give
/// (Falsifier 3): a fixture that composed a variant directly would be
/// the one place able to build a shape no launch can produce.
pub fn desktop_services_with_runtime(
    engine: Arc<dyn IngestPort>,
    runtime: Arc<sovereign_core::runtime::Runtime>,
) -> sovereign_daemon::DaemonServices {
    desktop_services(DesktopParts {
        runtime,
        ..DesktopParts::new(engine)
    })
}

/// [`stub_runtime`] carrying the caller's `SkillRegistry` instead of an
/// empty one — the registry `/v1/skills` serves.
pub fn stub_runtime_with_skills(
    provider: Arc<dyn sovereign_contracts::traits::InferenceProvider>,
    skills: Arc<sovereign_contracts::skills::SkillRegistry>,
) -> Arc<sovereign_core::runtime::Runtime> {
    Arc::new(sovereign_core::runtime::Runtime::new(
        sovereign_core::RuntimeParts::new(
            provider,
            Box::new(sovereign_core::stubs::PassthroughRouter),
            Box::new(sovereign_core::stubs::NoOpPlanner),
            Arc::new(sovereign_contracts::ToolRegistry::new()),
            Arc::new(sovereign_store::memory::InMemoryStateStore::new()),
            skills,
            Arc::new(sovereign_core::executor::AutoApprovalChannel),
            sovereign_contracts::types::InferenceConfig::default(),
            sovereign_core::runtime::lane::LaneSources::none(),
        ),
    ))
}

/// `None` is the commission whose `features.db` would not open — the 503
/// the routes name.
fn features_or_unopened(
    features: Option<Arc<dyn RecipeProjectPort>>,
) -> Result<Arc<dyn RecipeProjectPort>, String> {
    features.ok_or_else(|| sovereign_daemon::features_http::NO_FEATURES_DB.to_string())
}

/// A serving desktop commission carrying svrn's real store (behind a
/// mounted `/mcp` surface, which is where `EmbeddedDaemon::notes_store`
/// reads it from) and a recipe-project port (the double, or `None` for the
/// store that would not open).
///
/// The three fixtures above leave both absent, which is the right shape
/// for testing the named 503 and the wrong one for testing the routes.
/// Assembled through THE assembler like every production site — a
/// fixture that composed a variant directly would be the one place able
/// to build a shape no launch can produce (Falsifier 3).
pub fn desktop_services_with_note_and_feature_stores(
    engine: Arc<dyn IngestPort>,
    notes: Arc<sovereign_store::sqlite::SqliteStateStore>,
    features: Option<Arc<dyn RecipeProjectPort>>,
) -> sovereign_daemon::DaemonServices {
    desktop_services(DesktopParts {
        features: features_or_unopened(features),
        ..DesktopParts::new(engine)
            .mounted(Arc::new(sovereign_contracts::ToolRegistry::new()), notes)
    })
}

/// A serving desktop commission carrying real note + feature stores AND a
/// caller-supplied `ToolRegistry` behind the `/mcp` mount (sv-surface D8).
///
/// [`desktop_services_with_note_and_feature_stores`] mounts an EMPTY
/// registry, which is the right shape for the notes routes and the wrong
/// one for `mcp_config_http`: its whole answer is a fold over the tool ids
/// that mount actually holds, and a fixture that could only ever fold over
/// zero would pass whatever the fold did.
///
/// Assembled through THE assembler like every production site.
pub fn desktop_services_with_tool_registry(
    engine: Arc<dyn IngestPort>,
    notes: Arc<sovereign_store::sqlite::SqliteStateStore>,
    features: Option<Arc<dyn RecipeProjectPort>>,
    tools: Arc<sovereign_contracts::ToolRegistry>,
) -> sovereign_daemon::DaemonServices {
    desktop_services(DesktopParts {
        features: features_or_unopened(features),
        ..DesktopParts::new(engine).mounted(tools, notes)
    })
}

/// The cheapest `Runtime` that is still a real one — core's stub router and
/// planner, an empty tool registry, no enrichment lane. It loads no model and
/// touches no disk, which is the point: a fixture that had to run the
/// production recipe (`sovereign-runtime-recipe`) would turn every mesh
/// variant test into a boot test.
///
/// `store` lets a caller hand in the SAME store the daemon's `ServingCore`
/// carries, so a test can assert on rows a turn wrote. `None` gets a private
/// in-memory one.
pub fn stub_runtime(
    provider: Arc<dyn sovereign_contracts::traits::InferenceProvider>,
    store: Option<Arc<dyn sovereign_contracts::traits::StateStore>>,
) -> Arc<sovereign_core::runtime::Runtime> {
    Arc::new(stub_runtime_parts(provider, store))
}

/// [`stub_runtime`] with the daemon's corpus engine attached, so
/// `Runtime::seed_conversation`'s allow-list check and the retrieval fan-out
/// see the corpora the `ServingCore` holds — as the production recipe wires
/// them.
pub fn stub_runtime_with_engine(
    provider: Arc<dyn sovereign_contracts::traits::InferenceProvider>,
    store: Option<Arc<dyn sovereign_contracts::traits::StateStore>>,
    engine: Arc<dyn IngestPort>,
) -> Arc<sovereign_core::runtime::Runtime> {
    let mut runtime = stub_runtime_parts(provider, store);
    runtime.corpus_engine = Some(engine as Arc<dyn corpus_index::source::CorpusReadPort>);
    Arc::new(runtime)
}

fn stub_runtime_parts(
    provider: Arc<dyn sovereign_contracts::traits::InferenceProvider>,
    store: Option<Arc<dyn sovereign_contracts::traits::StateStore>>,
) -> sovereign_core::runtime::Runtime {
    stub_runtime_parts_with_lanes(
        provider,
        store,
        sovereign_core::runtime::lane::LaneSources::none(),
    )
}

fn stub_runtime_parts_with_lanes(
    provider: Arc<dyn sovereign_contracts::traits::InferenceProvider>,
    store: Option<Arc<dyn sovereign_contracts::traits::StateStore>>,
    lanes: sovereign_core::runtime::lane::LaneSources,
) -> sovereign_core::runtime::Runtime {
    let store =
        store.unwrap_or_else(|| Arc::new(sovereign_store::memory::InMemoryStateStore::new()));
    sovereign_core::runtime::Runtime::new(sovereign_core::RuntimeParts::new(
        provider,
        Box::new(sovereign_core::stubs::PassthroughRouter),
        Box::new(sovereign_core::stubs::NoOpPlanner),
        Arc::new(sovereign_contracts::ToolRegistry::new()),
        store,
        Arc::new(sovereign_contracts::skills::SkillRegistry::new()),
        Arc::new(sovereign_core::executor::AutoApprovalChannel),
        sovereign_contracts::types::InferenceConfig::default(),
        lanes,
    ))
}

/// A serving desktop commission whose `Runtime` carries `conv` on
/// `lane_sources.conv_tiered` — the ONE handle `atlas_http`'s six
/// conversation-tiered routes read (sv-surface D4 remainder). The
/// production wiring is `state.rs:1549`, which upcasts the daemon's
/// own `SqliteStateStore` into exactly this slot; a test that stubbed
/// the reader instead would prove the projections and nothing about
/// the path the handler takes to reach them.
pub fn desktop_services_with_conv_reader(
    engine: Arc<dyn IngestPort>,
    store: Arc<dyn sovereign_contracts::traits::StateStore>,
    conv: Arc<dyn sovereign_core::conv_tiered::ConvTieredReader>,
) -> sovereign_daemon::DaemonServices {
    let mut lanes = sovereign_core::runtime::lane::LaneSources::none();
    lanes.conv_tiered = Some(conv);
    let runtime = Arc::new(stub_runtime_parts_with_lanes(
        Arc::new(TestProvider::new()),
        Some(Arc::clone(&store)),
        lanes,
    ));
    desktop_services(DesktopParts {
        store,
        runtime,
        ..DesktopParts::new(engine)
    })
}

/// A `Desktop` serving daemon whose `ServingCore` carries the given store and
/// engine and a `Runtime` built over the SAME store and the SAME engine —
/// which is what a turn test needs: the route reads the store the turn wrote
/// to, and the corpora the daemon holds are the corpora the runtime's
/// allow-list check and retrieval fan-out see. (The engine half was missing
/// until 2026-09-01: the runtime had `corpus_engine: None` while the core
/// carried one, so a create with `enabled_corpora` was refused as "no corpus
/// index installed" against a daemon that had two.)
pub fn desktop_services_with_store(
    engine: Arc<dyn IngestPort>,
    store: Arc<dyn sovereign_contracts::traits::StateStore>,
    provider: Arc<dyn sovereign_contracts::traits::InferenceProvider>,
) -> sovereign_daemon::DaemonServices {
    desktop_services_with_insights(engine, store, provider, None)
}

/// [`desktop_services_with_store`] with a REAL PLANNER — the sv-surface R3
/// shape the pausing-turn test needs: a ComplexTask turn whose plan carries
/// a `UserInput` step, so the executor parks on the socket's approval
/// channel mid-turn (the C1 OPEN VERIFICATION: no test had ever driven a
/// real turn that pauses on an approval over the wire). Everything else
/// stays the stub shape — the point is the seam, not the planner.
pub fn desktop_services_with_planner(
    engine: Arc<dyn IngestPort>,
    store: Arc<dyn sovereign_contracts::traits::StateStore>,
    provider: Arc<dyn sovereign_contracts::traits::InferenceProvider>,
    planner: Box<dyn sovereign_contracts::traits::Planner>,
) -> sovereign_daemon::DaemonServices {
    let mut runtime = stub_runtime_parts(Arc::clone(&provider), Some(Arc::clone(&store)));
    runtime.corpus_engine = Some(Arc::clone(&engine) as _);
    runtime.planner = planner;
    desktop_services(DesktopParts {
        provider,
        store,
        runtime: Arc::new(runtime),
        ..DesktopParts::new(engine)
    })
}

/// [`desktop_services_with_store`] with an `InsightService` commissioned —
/// the sv-surface rung 6 shape the insight-surface parity tests need: a
/// serving daemon whose `/v1/insights/*` routes have a real service behind
/// them, assembled through THE assembler like every production site.
#[allow(clippy::too_many_arguments)]
pub fn desktop_services_with_insights(
    engine: Arc<dyn IngestPort>,
    store: Arc<dyn sovereign_contracts::traits::StateStore>,
    provider: Arc<dyn sovereign_contracts::traits::InferenceProvider>,
    insights: Option<Arc<sovereign_core::insight::InsightService>>,
) -> sovereign_daemon::DaemonServices {
    let runtime = stub_runtime_with_engine(
        Arc::clone(&provider),
        Some(Arc::clone(&store)),
        Arc::clone(&engine),
    );
    desktop_services(DesktopParts {
        provider,
        store,
        runtime,
        insights,
        ..DesktopParts::new(engine)
    })
}

/// Commission a `MeshAdmin` daemon THE WAY PRODUCTION DOES.
///
/// `svrn mesh create` / `join` reach this shape through exactly one door —
/// `sovereign_daemon::assemble` — and since daemon-convergence Phase 7 that is
/// the only door there is: `DaemonServices::MeshAdmin` carries a private
/// [`sovereign_daemon::MeshAdminWitness`], so no crate outside `sovereign-mesh`
/// can name the variant into being.
///
/// These tests used to write `DaemonServices::MeshAdmin` directly, which meant
/// 21 sites commissioned a daemon by a route no user can take. Driving the
/// real door is strictly better evidence: every one of these tests now also
/// proves the assembler accepts a verb launch and returns the admin shape.
pub fn mesh_admin_services() -> sovereign_daemon::DaemonServices {
    sovereign_daemon::assemble(
        &sovereign_contracts::launch::Launch::Verb {
            name: "mesh".to_string(),
            args: Vec::new(),
        },
        sovereign_daemon::LaunchParts::Admin {
            mesh: sovereign_daemon::process::MeshAccess::absent(),
        },
    )
    .expect("a verb launch with admin parts assembles to MeshAdmin")
}
