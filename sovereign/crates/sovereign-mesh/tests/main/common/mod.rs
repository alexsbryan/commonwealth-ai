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

use std::collections::HashMap;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use futures::Stream;

use commonwealth_core::capabilities::{AvailableResources, HardwareProfile, NodeCapabilities};
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::{MemberRecord, Mesh, NodeStatus};
use sovereign_core::error::{Error, Result as SovResult};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{
    CompletionRequest, CompletionResponse, ProviderCapabilities, Speed, StreamFrame,
};

// ── Capabilities + member helpers ───────────────────────────────

/// A `NodeCapabilities` with every field zeroed / empty. Useful for
/// constructing test `MemberRecord`s where the hardware profile
/// doesn't matter.
pub fn empty_capabilities() -> NodeCapabilities {
    NodeCapabilities {
        hardware: HardwareProfile {
            gpus: vec![],
            system_ram_gb: 0,
            cpu_cores: 0,
            total_storage_gb: 0,
            free_storage_gb: 0,
            network_bandwidth_mbps: None,
        },
        available: AvailableResources::default(),
        active_processes: vec![],
        hosted_corpora: vec![],
        reported_at: 0,
        inference_availability: 1.0,
        inference_capable: false,
        loaded_models: vec![],
        embed_model: None,
        benchmark: None,
        current_in_flight: None,
        anchor: None,
    }
}

/// Build a `MemberRecord` with a specified `last_seen`. Use when the
/// test cares about the timestamp (e.g. gossip-decay scenarios).
pub fn member_with_last_seen(
    id: NodeId,
    name: &str,
    last_seen: u64,
    addr: SocketAddr,
) -> MemberRecord {
    MemberRecord {
        removed_at: None,
        node_pubkey: None,
        relay_url: None,
        iroh_direct_addrs: Vec::new(),
        dial_info_version: 0,
        dial_info_sig: None,
        node_id: id,
        name: name.into(),
        invited_by: id,
        joined_at: 0,
        last_seen,
        status: NodeStatus::Online,
        capabilities: empty_capabilities(),
        addresses: vec![addr],
    }
}

/// Build a `MemberRecord` with `last_seen = 0`. The common case in
/// tests that don't exercise decay.
pub fn member(id: NodeId, name: &str, addr: SocketAddr) -> MemberRecord {
    member_with_last_seen(id, name, 0, addr)
}

/// Build a single-member `Mesh` rooted at `self_id`. The mesh_id is
/// 1 and the invite_key_hash is `[0x77; 32]` — neither matters for
/// tests that don't exercise the gossip auth boundary; for those
/// tests, construct the mesh inline with the right values.
pub fn solo_mesh(self_id: NodeId, name: &str) -> Mesh {
    let mut members = HashMap::new();
    members.insert(
        self_id,
        member(self_id, "self", "127.0.0.1:9742".parse().unwrap()),
    );
    Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: MeshId::from_u128(1),
        name: name.into(),
        invite_key_hash: [0x77u8; 32],
        invite_version: 0,
        require_encryption: false,
        members,
        peers: vec![],
    }
}

/// Hex-encode a `NodeId` for the `X-Node-Id` header. 32 hex chars,
/// lowercase — matches `commonwealth_api::headers::parse_x_node_id`.
pub fn id_to_hex(id: &NodeId) -> String {
    id.as_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

// ── Router spawning ─────────────────────────────────────────────

/// Bind `router` on `127.0.0.1:0` and return the bound address. The
/// listener is wired with `into_make_service_with_connect_info::<SocketAddr>()`
/// so the loopback guard middleware (which fail-closes on absent
/// ConnectInfo) sees the production listener shape.
pub async fn spawn_router(router: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });
    // 20ms is enough headroom on every CI box we use; the tokio
    // accept-loop is ready well before reqwest's first connect.
    tokio::time::sleep(Duration::from_millis(20)).await;
    addr
}

/// An `AppState` with one member (self) and a client token installed.
///
/// Shared by the tests that drive the REAL `commonwealth_api` client router
/// over a transport — they differ only in the mesh's encryption posture, and a
/// second copy of this would drift from the first.
pub fn client_app_state(
    self_id: NodeId,
    token: Option<&str>,
    require_encryption: bool,
) -> commonwealth_api::state::AppState {
    let mut members = HashMap::new();
    members.insert(
        self_id,
        member(self_id, "self", "127.0.0.1:9742".parse().unwrap()),
    );
    let mesh = Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: MeshId::from_u128(1),
        name: "transport test".into(),
        invite_key_hash: [0u8; 32],
        invite_version: 0,
        require_encryption,
        members,
        peers: vec![],
    };
    let state = commonwealth_api::state::AppState::new(self_id, mesh);
    state.install_client_token(token.map(std::sync::Arc::<str>::from));
    state
}

// ── Configurable InferenceProvider stub ─────────────────────────

/// A builder-style `InferenceProvider` used across integration tests.
///
/// Defaults to "every method returns `NotImplemented`". Tests opt
/// into specific behaviors via `with_*` builder methods. The intent
/// is to make the per-test code expressive about what the stub
/// supports — a test that never exercises `embed` doesn't have to
/// configure it, and a future regression that starts calling it
/// surfaces as a `NotImplemented` error rather than silent success.
///
/// Replaces the per-file `LocalStub` / `StubProvider` / `EmbedStub`
/// / `NoopProvider` / `ManifestProvider` / `FixedFinishProvider` /
/// `LegacyStreamProvider` copies that accumulated as the test suite
/// grew. ARCH §10.3's "four or more" threshold for trait extraction
/// is exceeded; this is that extraction.
pub struct TestProvider {
    model_id: String,
    code_model_id: Option<String>,
    complete_text: Option<String>,
    stream_chunks: Option<Vec<String>>,
    /// Wall-clock delay before each streamed chunk. Lets a test hold a turn
    /// open long enough to assert on what the host does WHILE one is running
    /// — the receive loop staying responsive, principally.
    stream_delay: Option<std::time::Duration>,
    embed_fn: Option<Arc<dyn Fn(&str) -> Vec<f32> + Send + Sync>>,
    /// When set, `complete_stream_with_finish` returns exactly these
    /// frames. Use to test finish_reason wire fidelity (Length,
    /// ContentFilter, etc.). When None, the trait's default impl
    /// wraps `complete_stream` and appends a synthetic Stop.
    typed_frames: Option<Vec<StreamFrame>>,
    /// Fires while a generation is genuinely in flight. See
    /// [`TestProvider::with_on_complete`].
    on_complete: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Capabilities reported to manifest synthesis. The test rarely
    /// inspects this beyond a sanity check; defaults are conservative.
    capabilities: ProviderCapabilities,
}

impl TestProvider {
    pub fn new() -> Self {
        Self {
            model_id: "test-provider".into(),
            code_model_id: None,
            complete_text: None,
            stream_chunks: None,
            stream_delay: None,
            embed_fn: None,
            typed_frames: None,
            on_complete: None,
            capabilities: ProviderCapabilities {
                max_context_tokens: 4_096,
                supports_structured_output: false,
                relative_speed: Speed::Fast,
                relative_reasoning: sovereign_core::types::Depth::Moderate,
            },
        }
    }

    pub fn with_model_id(mut self, id: impl Into<String>) -> Self {
        self.model_id = id.into();
        self
    }

    pub fn with_code_model_id(mut self, id: impl Into<String>) -> Self {
        self.code_model_id = Some(id.into());
        self
    }

    /// `complete()` returns a `CompletionResponse` carrying this text.
    pub fn with_complete_text(mut self, text: impl Into<String>) -> Self {
        self.complete_text = Some(text.into());
        self
    }

    /// `complete_stream()` (legacy `Result<String>` surface) yields
    /// these chunks in order. The default-impl
    /// `complete_stream_with_finish` then wraps them with a synthetic
    /// terminal `Stop`. To override the terminal frame, use
    /// [`Self::with_typed_frames`].
    pub fn with_stream_chunks(mut self, chunks: Vec<String>) -> Self {
        self.stream_chunks = Some(chunks);
        self
    }

    /// Sleep this long before each chunk, so a turn takes a knowable amount of
    /// wall clock. Used to make "while a turn is in flight" a testable window
    /// rather than a race.
    pub fn with_stream_delay(mut self, d: std::time::Duration) -> Self {
        self.stream_delay = Some(d);
        self
    }

    /// `embed(input)` runs this closure on the input and returns the
    /// resulting vector. Tests that want a marker-encoded vector
    /// (e.g. `|input| vec![input.len() as f32; 8]`) pass a closure;
    /// tests that just want a zero vector pass `|_| vec![0.0; N]`.
    pub fn with_embed_marker(
        mut self,
        f: impl Fn(&str) -> Vec<f32> + Send + Sync + 'static,
    ) -> Self {
        self.embed_fn = Some(Arc::new(f));
        self
    }

    /// `complete_stream_with_finish()` yields these typed frames.
    /// Use the `StreamFrame::Finish { reason, .. }` variant to pin
    /// non-Stop finish reasons.
    pub fn with_typed_frames(mut self, frames: Vec<StreamFrame>) -> Self {
        self.typed_frames = Some(frames);
        self
    }

    /// Observation hook, fired at the top of `complete` and
    /// `complete_stream` — i.e. while a generation is genuinely in
    /// flight on this provider.
    ///
    /// Exists because some process state is only observable *during*
    /// the serve: an RAII guard that bumps a counter on the way in and
    /// drops it on the way out leaves nothing to assert on once the
    /// response has been returned. A caller that samples such a
    /// counter after `send().await` cannot distinguish "never
    /// incremented" from "incremented and correctly released".
    ///
    /// Caveat: `complete_stream_with_finish` reaches the hook only on
    /// the path that delegates to `complete_stream`. When
    /// [`Self::with_typed_frames`] is set it returns those frames
    /// directly and no generation entry point runs, so the hook does
    /// not fire.
    pub fn with_on_complete(mut self, f: impl Fn() + Send + Sync + 'static) -> Self {
        self.on_complete = Some(Arc::new(f));
        self
    }

    fn fire_on_complete(&self) {
        if let Some(f) = self.on_complete.as_ref() {
            f();
        }
    }
}

impl Default for TestProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl InferenceProvider for TestProvider {
    async fn complete(&self, _req: &CompletionRequest) -> SovResult<CompletionResponse> {
        self.fire_on_complete();
        match self.complete_text.as_ref() {
            Some(t) => Ok(CompletionResponse {
                text: t.clone(),
                tokens_used: 1,
                prompt_tokens: 1,
                model_id: self.model_id.clone(),
                latency_ms: 0,
                oicp_meta: None,
                finish_reason: None,
                completion_tokens: None,
            }),
            None => Err(Error::NotImplemented(
                "TestProvider::complete not configured — \
                 call .with_complete_text(...) on the builder"
                    .into(),
            )),
        }
    }

    async fn complete_stream(
        &self,
        _req: &CompletionRequest,
    ) -> SovResult<Pin<Box<dyn Stream<Item = SovResult<String>> + Send>>> {
        self.fire_on_complete();
        match self.stream_chunks.as_ref() {
            Some(chunks) => {
                let delay = self.stream_delay;
                let items: Vec<String> = chunks.clone();
                Ok(Box::pin(futures::StreamExt::then(
                    futures::stream::iter(items),
                    move |c| async move {
                        if let Some(d) = delay {
                            tokio::time::sleep(d).await;
                        }
                        Ok(c)
                    },
                )))
            }
            None => Err(Error::NotImplemented(
                "TestProvider::complete_stream not configured — \
                 call .with_stream_chunks(...) on the builder"
                    .into(),
            )),
        }
    }

    async fn complete_stream_with_finish(
        &self,
        request: &CompletionRequest,
    ) -> SovResult<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>> {
        if let Some(frames) = self.typed_frames.as_ref() {
            return Ok(Box::pin(futures::stream::iter(frames.clone())));
        }
        // Reproduce the trait's default impl inline — we can't
        // dispatch to it without infinite recursion. Wraps
        // `complete_stream` with `Token(text)` frames and appends
        // a synthetic terminal `Stop` (unless the body already
        // emitted an `Error` terminator). Matches the documented
        // behaviour of `InferenceProvider::complete_stream_with_finish`'s
        // default impl in `sovereign-core::traits`.
        use futures::StreamExt;
        use std::sync::atomic::{AtomicBool, Ordering};

        let inner = self.complete_stream(request).await?;
        let terminal_emitted = Arc::new(AtomicBool::new(false));
        let body_flag = Arc::clone(&terminal_emitted);
        let mapped = inner.flat_map(move |item| {
            let frames: Vec<StreamFrame> = match item {
                Ok(text) => vec![StreamFrame::Token(text)],
                Err(e) => {
                    body_flag.store(true, Ordering::Relaxed);
                    vec![StreamFrame::Finish {
                        reason: sovereign_core::types::FinishReason::Error(format!("{e}")),
                        usage: None,
                    }]
                }
            };
            futures::stream::iter(frames)
        });
        let tail_flag = terminal_emitted;
        let tail = futures::stream::once(async move {
            if tail_flag.load(Ordering::Relaxed) {
                None
            } else {
                Some(StreamFrame::Finish {
                    reason: sovereign_core::types::FinishReason::Stop,
                    usage: None,
                })
            }
        })
        .filter_map(|f| async move { f });
        Ok(Box::pin(mapped.chain(tail)))
    }

    async fn embed(&self, input: &str) -> SovResult<Vec<f32>> {
        match self.embed_fn.as_ref() {
            Some(f) => Ok(f(input)),
            None => Err(Error::NotImplemented(
                "TestProvider::embed not configured — \
                 call .with_embed_marker(...) on the builder"
                    .into(),
            )),
        }
    }

    fn model_id_for(&self, _speed: Speed) -> String {
        self.model_id.clone()
    }

    fn code_model_id(&self) -> Option<String> {
        self.code_model_id.clone()
    }

    /// Report the configured models as resident slots.
    ///
    /// `build_self_manifest` reads this to decide whether the node holds
    /// anything at all: an empty report means "forwards to a remote, owns no
    /// weights" and advertises nothing, which is how a `terminal`-class daemon
    /// and the attach-mode desktop avoid claiming their entry node's model as
    /// their own. A `TestProvider` stands in for a node that DOES hold its
    /// models, so it has to say so — inheriting the empty default would model a
    /// thin client while every other method claims to serve.
    fn resident_slots(&self) -> Vec<sovereign_core::traits::ResidentSlot> {
        let slot = |role: &str, model_id: String| sovereign_core::traits::ResidentSlot {
            role: role.to_string(),
            model_id,
            resident: true,
            size_bytes: None,
            transitioning: false,
            placement: None,
        };
        let mut slots = vec![slot("primary", self.model_id.clone())];
        if let Some(code) = &self.code_model_id {
            slots.push(slot("code", code.clone()));
        }
        slots
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.capabilities.clone()
    }
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
    engine: Arc<corpus_engine::CorpusEngine>,
) -> sovereign_mesh::DaemonServices {
    // Through THE assembler, like every production site — a fixture that
    // composed a variant directly would be the one place able to build a shape
    // no launch can produce, which is exactly what Falsifier 3 forbids.
    sovereign_mesh::assemble(
        &sovereign_contracts::launch::Launch::Desktop,
        sovereign_mesh::LaunchParts::Serving {
            headless: None,
            serving: sovereign_mesh::ServingProfile {
                core: sovereign_mesh::ServingCore {
                    corpus_engine: engine,
                    inference_provider: Arc::new(TestProvider::new()),
                    state_store: Arc::new(sovereign_store::memory::InMemoryStateStore::new()),
                    runtime: stub_runtime(Arc::new(TestProvider::new()), None),
                },
                capability: sovereign_mesh::ServingCapability {
                    mcp: sovereign_mesh::McpSurface::Unavailable {
                        reason: "test fixture: no tool registry".into(),
                    },
                    project_http: Router::new(),
                    corpus_watch_http: Router::new(),
                },
                advertise_embed: sovereign_mesh::EmbedAdvertisement::Unavailable {
                    reason: "test fixture: no embed probe".into(),
                },
            },
        },
    )
    .expect("Launch::Desktop assembles a serving profile with no rails")
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
    provider: Arc<dyn sovereign_core::traits::InferenceProvider>,
    store: Option<Arc<dyn sovereign_core::traits::StateStore>>,
) -> Arc<sovereign_core::runtime::Runtime> {
    let store =
        store.unwrap_or_else(|| Arc::new(sovereign_store::memory::InMemoryStateStore::new()));
    Arc::new(sovereign_core::runtime::Runtime::new(
        sovereign_core::RuntimeParts::new(
            provider,
            Box::new(sovereign_core::stubs::PassthroughRouter),
            Box::new(sovereign_core::stubs::NoOpPlanner),
            Arc::new(sovereign_core::ToolRegistry::new()),
            store,
            Arc::new(sovereign_core::SkillRegistry::new()),
            Arc::new(sovereign_core::executor::AutoApprovalChannel),
            sovereign_core::types::InferenceConfig::default(),
            sovereign_core::runtime::lane::LaneSources::none(),
        ),
    ))
}

/// A `Desktop` serving daemon whose `ServingCore` carries the given store and
/// a `Runtime` built over the SAME store — which is what a turn test needs:
/// the route reads the store the turn wrote to.
pub fn desktop_services_with_store(
    engine: Arc<corpus_engine::CorpusEngine>,
    store: Arc<dyn sovereign_core::traits::StateStore>,
    provider: Arc<dyn sovereign_core::traits::InferenceProvider>,
) -> sovereign_mesh::DaemonServices {
    sovereign_mesh::assemble(
        &sovereign_contracts::launch::Launch::Desktop,
        sovereign_mesh::LaunchParts::Serving {
            headless: None,
            serving: sovereign_mesh::ServingProfile {
                core: sovereign_mesh::ServingCore {
                    corpus_engine: engine,
                    inference_provider: Arc::clone(&provider),
                    state_store: Arc::clone(&store),
                    runtime: stub_runtime(provider, Some(store)),
                },
                capability: sovereign_mesh::ServingCapability {
                    mcp: sovereign_mesh::McpSurface::Unavailable {
                        reason: "test fixture: no tool registry".into(),
                    },
                    project_http: Router::new(),
                    corpus_watch_http: Router::new(),
                },
                advertise_embed: sovereign_mesh::EmbedAdvertisement::Unavailable {
                    reason: "test fixture: no embed probe".into(),
                },
            },
        },
    )
    .expect("Launch::Desktop assembles a serving profile with no rails")
}

/// Commission a `MeshAdmin` daemon THE WAY PRODUCTION DOES.
///
/// `svrn mesh create` / `join` reach this shape through exactly one door —
/// `sovereign_mesh::assemble` — and since daemon-convergence Phase 7 that is
/// the only door there is: `DaemonServices::MeshAdmin` carries a private
/// [`sovereign_mesh::MeshAdminWitness`], so no crate outside `sovereign-mesh`
/// can name the variant into being.
///
/// These tests used to write `DaemonServices::MeshAdmin` directly, which meant
/// 21 sites commissioned a daemon by a route no user can take. Driving the
/// real door is strictly better evidence: every one of these tests now also
/// proves the assembler accepts a verb launch and returns the admin shape.
pub fn mesh_admin_services() -> sovereign_mesh::DaemonServices {
    sovereign_mesh::assemble(
        &sovereign_contracts::launch::Launch::Verb {
            name: "mesh".to_string(),
            args: Vec::new(),
        },
        sovereign_mesh::LaunchParts::Admin,
    )
    .expect("a verb launch with admin parts assembles to MeshAdmin")
}
