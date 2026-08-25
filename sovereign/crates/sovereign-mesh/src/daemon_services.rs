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
//!   state_store                  .           Y               .
//!   mcp                          Y           Y*              .
//!   mesh/admin/reading routers   Y           Y               .
//!   project_http_router          Y           Y               .
//!   corpus_watch_http_router     Y*          Y*              .
//!   provider_factory             Y           .               .
//!   mesh_store                   Y           .               .
//!   convergence_recorder         Y           .               .
//!   knowledge_view_http_router   Y           .               .
//!   solve_http_router            Y           .               .
//! ```
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
//! | CAPABILITY | can do less | `mcp`, `project_http`, `corpus_watch_http`, `+knowledge_view_http`, `+solve_http` |
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

use corpus_engine::CorpusEngine;
use corpus_engine_notes::NoteStore;

use commonwealth_core::oicp::EmbedModelInfo;
use sovereign_core::registry::ToolRegistry;
use sovereign_core::traits::{InferenceProvider, StateStore};

use crate::admin_http::ProviderFactory;

/// Per-session MCP mount. When present, the daemon merges
/// `mcp_router::mcp_router(...)` into its client router so `/mcp`,
/// `/mcp/message` and `/mcp/stats` share the port with `/v1/*`.
#[derive(Clone)]
pub struct McpMount {
    pub tools: Arc<ToolRegistry>,
    pub notes: Arc<NoteStore>,
    /// Groups this process's tool calls in `NoteStore::log_tool_call`
    /// (e.g. `daemon-<uuid>`, `desktop-<uuid>`).
    pub session_id: String,
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

/// **Ring 1 — CORE.** Absence means the daemon cannot serve at all: without an
/// engine `/v1/knowledge/search` and `/internal/knowledge/search` answer 503
/// and gossip advertises no `hosted_corpora`; without a provider
/// `/v1/chat/completions` has nothing behind it. Neither is an `Option`, in
/// this struct or anywhere downstream.
pub struct ServingCore {
    pub corpus_engine: Arc<CorpusEngine>,
    /// The provider that answers peers hitting `/v1/chat/completions`. The
    /// daemon holds it behind a lock only because `POST /v1/admin/reload`
    /// swaps it; a host installs it here, once, or not at all.
    pub inference_provider: Arc<dyn InferenceProvider>,
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
pub struct ServingCapability {
    pub mcp: McpSurface,
    pub project_http: axum::Router,
    pub corpus_watch_http: axum::Router,
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
}

/// Three handles `sovereign daemon run` must share with writers that live
/// outside the daemon, or a surface reports something untrue.
///
/// - `provider_factory` — without it `POST /v1/admin/reload` cannot honour a
///   `models.*` change. The desktop has never had one; that is why this rail
///   is on the headless variant and the desktop's reload names its profile in
///   the refusal instead of reporting a missing installation.
/// - `mesh_store` — the store the work atlas writes into, so its entries reach
///   gossip's `all_entries_for_gossip`. Without it the daemon builds a private
///   in-memory store and atlas data is invisible across the mesh.
/// - `convergence_recorder` — the ONE `ConvergenceRecord` the notes publish
///   sink, the ingest poller and `/status` all stamp and read. A second copy
///   would let the status section disagree with the sink.
pub struct HeadlessRails {
    pub provider_factory: Arc<dyn ProviderFactory>,
    pub mesh_store: Arc<commonwealth_state::MeshStore>,
    pub convergence_recorder: Arc<commonwealth_api::state::ConvergenceRecord>,
}

/// Everything the desktop's in-process daemon supplies.
///
/// No [`HeadlessRails`]: the desktop has never carried a provider factory, a
/// shared mesh store or a convergence recorder. That absence is now a property
/// of this shape rather than three empty slots.
pub struct DesktopServices {
    pub serving: ServingProfile,
    /// Resolves `conversation-history` chunks back to their conversation for
    /// the reading surface. Required here and absent on [`HeadlessServices`] —
    /// the one place the two serving shapes cross rather than nest
    /// (daemon-convergence Phase 3 closes it by giving the headless daemon a
    /// state store of its own).
    pub state_store: Arc<dyn StateStore>,
}

/// Everything `sovereign daemon run` supplies.
///
/// No `state_store`: the headless daemon opens no `sovereign.db` today, so
/// reading-surface conversation chunks render without a title. Declared, not
/// silent.
pub struct HeadlessServices {
    pub serving: ServingProfile,
    pub rails: HeadlessRails,
    /// Ring 2 extension — `POST /v1/knowledge/landscape_digest`. Hosted only
    /// here because only this bootstrap owns a `KnowledgeViewManager`.
    pub knowledge_view_http: axum::Router,
    /// Ring 2 extension — `/v1/solve/jobs*`, the daemon-hosted TDD solver.
    /// Hosted only here because only this bootstrap owns the job table and the
    /// `commonwealth-tdd` dependency.
    pub solve_http: axum::Router,
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
    MeshAdmin,
    /// The desktop's in-process daemon (`Local` bootstrap mode).
    Desktop(Box<DesktopServices>),
    /// `sovereign daemon run`.
    Headless(Box<HeadlessServices>),
}

impl DaemonServices {
    pub fn desktop(services: DesktopServices) -> Self {
        Self::Desktop(Box::new(services))
    }

    pub fn headless(services: HeadlessServices) -> Self {
        Self::Headless(Box::new(services))
    }

    /// Stable name for logs and `/status`. Closed set — ARCH §2.1.
    pub fn label(&self) -> &'static str {
        match self {
            Self::MeshAdmin => "mesh-admin",
            Self::Desktop(_) => "desktop",
            Self::Headless(_) => "headless",
        }
    }

    /// True for the two variants that serve a host HTTP surface. The
    /// mesh-admin one-shot mounts nothing beyond the base client/internal
    /// routers.
    pub fn serves_host_surface(&self) -> bool {
        !matches!(self, Self::MeshAdmin)
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

    pub fn serving(&self) -> Option<&ServingProfile> {
        match self {
            Self::MeshAdmin => None,
            Self::Desktop(d) => Some(&d.serving),
            Self::Headless(h) => Some(&h.serving),
        }
    }

    pub fn state_store(&self) -> Option<&Arc<dyn StateStore>> {
        match self {
            Self::Desktop(d) => Some(&d.state_store),
            Self::MeshAdmin | Self::Headless(_) => None,
        }
    }

    /// The headless-only rails, or `None` on a variant that declares it has
    /// none. Callers must name the variant in any refusal they derive from a
    /// `None` here — nothing is missing, this shape has no rails.
    pub fn rails(&self) -> Option<&HeadlessRails> {
        match self {
            Self::Headless(h) => Some(&h.rails),
            Self::MeshAdmin | Self::Desktop(_) => None,
        }
    }

    /// Names of [`Self::host_routers`], same order — so the daemon's startup
    /// log says which surfaces it actually serves rather than leaving a
    /// reader to infer it from a 404.
    pub fn host_router_names(&self) -> Vec<&'static str> {
        match self {
            Self::MeshAdmin => Vec::new(),
            Self::Desktop(_) => vec!["project_http", "corpus_watch_http"],
            Self::Headless(_) => vec![
                "project_http",
                "corpus_watch_http",
                "knowledge_view_http",
                "solve_http",
            ],
        }
    }

    /// Every host-built router this variant mounts, in merge order.
    pub fn host_routers(&self) -> Vec<axum::Router> {
        match self {
            Self::MeshAdmin => Vec::new(),
            Self::Desktop(d) => vec![
                d.serving.capability.project_http.clone(),
                d.serving.capability.corpus_watch_http.clone(),
            ],
            Self::Headless(h) => vec![
                h.serving.capability.project_http.clone(),
                h.serving.capability.corpus_watch_http.clone(),
                h.knowledge_view_http.clone(),
                h.solve_http.clone(),
            ],
        }
    }
}

/// Cheap stand-ins so a test can build every variant without loading a model.
/// `#[cfg(test)]`: nothing outside this crate's unit tests can reach them, so
/// no production path can obtain a services value it did not assemble itself.
#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;
    use async_trait::async_trait;
    use sovereign_core::error::Result as SovResult;
    use sovereign_core::types::{
        CompletionRequest, CompletionResponse, Depth, ProviderCapabilities, Speed,
    };

    pub(crate) struct NullProvider;

    #[async_trait]
    impl InferenceProvider for NullProvider {
        async fn complete(&self, _r: &CompletionRequest) -> SovResult<CompletionResponse> {
            unimplemented!("fixture")
        }
        async fn complete_stream(
            &self,
            _r: &CompletionRequest,
        ) -> SovResult<std::pin::Pin<Box<dyn futures::Stream<Item = SovResult<String>> + Send>>>
        {
            unimplemented!("fixture")
        }
        async fn embed(&self, _t: &str) -> SovResult<Vec<f32>> {
            unimplemented!("fixture")
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 0,
                supports_structured_output: false,
                relative_speed: Speed::Fast,
                relative_reasoning: Depth::Shallow,
            }
        }
    }

    pub(crate) struct NullFactory;

    #[async_trait]
    impl ProviderFactory for NullFactory {
        async fn build_provider(
            &self,
            _cfg: &sovereign_core::setup_config::SetupConfig,
        ) -> Result<Arc<dyn InferenceProvider>, String> {
            Ok(Arc::new(NullProvider))
        }
    }

    pub(crate) fn engine() -> Arc<CorpusEngine> {
        let tmp = std::env::temp_dir().join("sovereign-mesh-services-fixture");
        Arc::new(CorpusEngine::new(
            tmp.join("recipes"),
            tmp.join("indexes"),
            Arc::new(|_: &str| Box::pin(async { Ok(vec![0.0_f32; 4]) })),
        ))
    }

    pub(crate) fn serving() -> ServingProfile {
        ServingProfile {
            core: ServingCore {
                corpus_engine: engine(),
                inference_provider: Arc::new(NullProvider),
            },
            capability: ServingCapability {
                mcp: McpSurface::Unavailable {
                    reason: "fixture".into(),
                },
                project_http: axum::Router::new(),
                corpus_watch_http: axum::Router::new(),
            },
            advertise_embed: EmbedAdvertisement::Unavailable {
                reason: "fixture".into(),
            },
        }
    }

    pub(crate) fn desktop() -> DaemonServices {
        DaemonServices::desktop(DesktopServices {
            serving: serving(),
            state_store: Arc::new(sovereign_store::memory::InMemoryStateStore::new()),
        })
    }

    pub(crate) fn headless() -> DaemonServices {
        headless_with_factory(Arc::new(NullFactory))
    }

    pub(crate) fn headless_with_factory(
        provider_factory: Arc<dyn ProviderFactory>,
    ) -> DaemonServices {
        DaemonServices::headless(HeadlessServices {
            serving: serving(),
            rails: HeadlessRails {
                provider_factory,
                mesh_store: Arc::new(
                    commonwealth_state::MeshStore::in_memory().expect("in-memory MeshStore"),
                ),
                convergence_recorder: Arc::new(commonwealth_api::state::ConvergenceRecord::new()),
            },
            knowledge_view_http: axum::Router::new(),
            solve_http: axum::Router::new(),
        })
    }

    /// Every variant, so a test can enumerate the whole space rather than
    /// spot-check the arms it happened to think of.
    pub(crate) fn every_variant() -> Vec<DaemonServices> {
        vec![DaemonServices::MeshAdmin, desktop(), headless()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The differential falsifier of `TOPOLOGY.md §4`, soundness half: every
    /// variant carries exactly the capability set measured on its live path,
    /// no more and no less. Written as a table so a reader checks it against
    /// the matrix in the module docs without running anything — and driven off
    /// `every_variant()`, so adding a fourth variant fails here until someone
    /// states what it carries.
    #[test]
    fn each_variant_declares_exactly_its_measured_capability_set() {
        // label, core, state_store, rails, host routers
        let expected: &[(&str, bool, bool, bool, usize)] = &[
            ("mesh-admin", false, false, false, 0),
            ("desktop", true, true, false, 2),
            ("headless", true, false, true, 4),
        ];
        let variants = fixtures::every_variant();
        assert_eq!(
            variants.len(),
            expected.len(),
            "a variant was added without a row in the measured table"
        );

        for (s, (label, core, store, rails, routers)) in variants.iter().zip(expected) {
            assert_eq!(s.label(), *label);
            // ONE question, not three. Until 2026-08-24 this asserted
            // `corpus_engine().is_some()`, `inference_provider().is_some()` and
            // `serving().is_some()` separately against the SAME `core` column —
            // three checks that could not disagree, because all three read one
            // variant through one-line `.map()` wrappers. Deleting the wrappers
            // is what made that visible.
            assert_eq!(s.serving().is_some(), *core, "{label}: serving profile");
            assert_eq!(s.state_store().is_some(), *store, "{label}: state_store");
            // Likewise ONE question, not four: `provider_factory`, `mesh_store`
            // and `convergence_recorder` all read `rails`.
            assert_eq!(s.rails().is_some(), *rails, "{label}: rails");
            assert_eq!(s.host_routers().len(), *routers, "{label}: host routers");
            assert_eq!(
                s.host_router_names().len(),
                *routers,
                "{label}: router names must match router count"
            );
            assert_eq!(
                s.serves_host_surface(),
                *core,
                "{label}: a variant serves a host surface iff it has a core"
            );
        }
    }

    // RETIRED 2026-08-24 — `a_serving_variant_never_has_half_a_core`.
    //
    // It asserted `corpus_engine().is_some() == inference_provider().is_some()`
    // — "core is one ring, not two independent slots". Both accessors are now
    // deleted, and the only way to reach either field is through
    // `serving()`, which yields a `&ServingProfile` whose `core` holds both as
    // plain `Arc`s. There is no longer an input that could make this test fail:
    // half a core is not writable. A check with no nameable failing input is
    // not a gate (§18.1), so it is deleted rather than left to read as
    // assurance. The property it guarded is now carried by the type.

    #[test]
    fn named_absence_is_not_a_bare_option() {
        let m = McpSurface::Unavailable {
            reason: "notes.db locked".into(),
        };
        assert!(m.mount().is_none());
        let e = EmbedAdvertisement::Unavailable {
            reason: "no embed model configured".into(),
        };
        assert!(e.info().is_none());
    }
}
