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
    ) -> SovResult<std::pin::Pin<Box<dyn futures::Stream<Item = SovResult<String>> + Send>>> {
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

pub(crate) fn engine() -> Arc<dyn IngestPort> {
    let tmp = std::env::temp_dir().join("sovereign-mesh-services-fixture");
    Arc::new(corpus_engine::CorpusEngine::new(
        tmp.join("recipes"),
        tmp.join("indexes"),
        Arc::new(|_: &str| Box::pin(async { Ok(vec![0.0_f32; 4]) })),
    ))
}

/// The cheapest `Runtime` that is still a real one: core's own stub
/// router and planner, an empty tool registry, no enrichment lane. It
/// loads no model and touches no disk, which is the whole point — a
/// fixture that had to commission the production recipe would make every
/// variant test a boot test.
pub(crate) fn runtime() -> Arc<sovereign_core::runtime::Runtime> {
    Arc::new(sovereign_core::runtime::Runtime::new(
        sovereign_core::RuntimeParts::new(
            Arc::new(NullProvider),
            Box::new(sovereign_core::stubs::PassthroughRouter),
            Box::new(sovereign_core::stubs::NoOpPlanner),
            Arc::new(sovereign_core::ToolRegistry::new()),
            Arc::new(sovereign_store::memory::InMemoryStateStore::new()),
            Arc::new(sovereign_core::SkillRegistry::new()),
            Arc::new(sovereign_core::executor::AutoApprovalChannel),
            sovereign_core::types::InferenceConfig::default(),
            sovereign_core::runtime::lane::LaneSources::none(),
        ),
    ))
}

pub(crate) fn serving() -> ServingProfile {
    serving_with_provider(Arc::new(NullProvider))
}

/// `serving()` with a caller-supplied provider, for the tests that
/// need one whose answers differ from `NullProvider`'s trait
/// defaults — otherwise "the slot said nothing" and "there is no
/// slot" are the same observation and a test cannot tell them apart.
pub(crate) fn serving_with_provider(
    inference_provider: Arc<dyn InferenceProvider>,
) -> ServingProfile {
    serving_with(
        inference_provider,
        Arc::new(sovereign_store::memory::InMemoryStateStore::new()),
    )
}

/// `serving()` with a caller-supplied STORE, for the routes whose
/// answer is a rollup the in-memory store declines
/// (`summarize_chat_activity` defaults to `Err(NotImplemented)`).
/// A test that could only reach that default could only assert the
/// refusal, which is the weak half of a pair (ARCH principle 5).
pub(crate) fn serving_with_store(state_store: Arc<dyn StateStore>) -> ServingProfile {
    serving_with(Arc::new(NullProvider), state_store)
}

/// The one `ServingProfile` literal — same rule as `headless_from`
/// below, and for the same reason: a second copy is how one of them
/// quietly stops setting a field the struct grows.
fn serving_with(
    inference_provider: Arc<dyn InferenceProvider>,
    state_store: Arc<dyn StateStore>,
) -> ServingProfile {
    ServingProfile {
        core: ServingCore {
            corpus_engine: engine(),
            recipe_harness: None,
            inference_provider,
            in_flight_gauge: None,
            rpc_shard_warmer: None,
            state_store,
            runtime: runtime(),
            insights: None,
            features: None,
        },
        capability: ServingCapability {
            mcp: McpSurface::Unavailable {
                reason: "fixture".into(),
            },
            project_http: axum::Router::new(),
            corpus_watch_http: axum::Router::new(),
            workflow_http: axum::Router::new(),
        },
        advertise_embed: EmbedAdvertisement::Unavailable {
            reason: "fixture".into(),
        },
    }
}

pub(crate) fn desktop() -> DaemonServices {
    DaemonServices::desktop(serving())
}

pub(crate) fn headless() -> DaemonServices {
    headless_with_factory(Arc::new(NullFactory))
}

/// Headless, serving on a caller-supplied provider.
pub(crate) fn headless_with_provider(
    inference_provider: Arc<dyn InferenceProvider>,
) -> DaemonServices {
    headless_from(
        serving_with_provider(inference_provider),
        Arc::new(NullFactory),
    )
}

/// Headless, serving over a caller-supplied store.
pub(crate) fn headless_with_store(state_store: Arc<dyn StateStore>) -> DaemonServices {
    headless_from(serving_with_store(state_store), Arc::new(NullFactory))
}

pub(crate) fn headless_with_factory(provider_factory: Arc<dyn ProviderFactory>) -> DaemonServices {
    headless_from(serving(), provider_factory)
}

/// The one `HeadlessServices` literal. Both helpers above vary one
/// half of it and share the rest; a second copy of this is how one of
/// them quietly stops setting a field the struct grows (which is
/// exactly what happened to `solve_http` when a copy was made).
fn headless_from(
    serving: ServingProfile,
    provider_factory: Arc<dyn ProviderFactory>,
) -> DaemonServices {
    DaemonServices::headless(HeadlessServices {
        serving,
        rails: HeadlessRails {
            provider_factory,
            mesh_store: Arc::new(crate::rails_client::kv::RailsKv::new(
                crate::rails_client::DEFAULT_RAILS_BASE,
            )),
            convergence_recorder: Arc::new(sovereign_mesh::peer_adapter::MeshConvergence::new()),
        },
        knowledge_view_http: axum::Router::new(),
        solve_http: axum::Router::new(),
    })
}

/// Every variant, so a test can enumerate the whole space rather than
/// spot-check the arms it happened to think of.
pub(crate) fn every_variant() -> Vec<DaemonServices> {
    vec![DaemonServices::mesh_admin(), desktop(), headless()]
}
