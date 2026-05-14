//! Defensive test for the silent-no-op failure mode of
//! `AppState::with_local_inference` and `with_mesh_mutation_hook`.
//!
//! These installers mutate `AppStateInner` through `Arc::get_mut`.
//! That call returns `None` the moment any other code has cloned
//! `app_state.inner` — the installer then becomes a tracing::error!
//! and a quiet return, NOT a panic or an error result.
//!
//! Why a test is needed: production already pinned the bug class
//! (the doc-comment at `daemon.rs:1199-1217` and the `error!`
//! messages on the installers describe it). The only thing missing
//! is a regression target that *fires* when a future refactor
//! re-orders `Arc::clone` ahead of one of these installers. The
//! existing daemon_wiring tests pin the happy path (with_*
//! installed, route returns 200); they cannot catch the silent
//! no-op because in their setup the Arc is never pre-cloned.
//!
//! Approach: install a `tracing` subscriber that captures emitted
//! events into a buffer, simulate the bad ordering (clone inner,
//! THEN call the installer), and assert the captured stream
//! contains the documented error text.
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::Stream;

use commonwealth_api::state::{AppState, LocalInferenceService};
use commonwealth_app::registry::AppRegistry;
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::Mesh;
use commonwealth_state::MeshStore;
use sovereign_core::error::Result as SovResult;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{
    CompletionRequest, CompletionResponse, ProviderCapabilities, Speed,
};
use sovereign_mesh::inference_adapter::SovereignInferenceAdapter;
use tracing_subscriber::fmt::MakeWriter;

/// Thread-shared `std::io::Write` impl that buffers everything for
/// later inspection. Wrapped behind `MakeWriter` so
/// `tracing_subscriber::fmt` can route events to it.
#[derive(Clone)]
struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for CaptureWriter {
    type Writer = Self;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn capture_subscriber(
    buf: Arc<Mutex<Vec<u8>>>,
) -> tracing::subscriber::DefaultGuard {
    // `set_default` scopes the subscriber to the current thread.
    // This test stays single-threaded (no `#[tokio::test]`) so the
    // guard reliably catches everything emitted by the lines below.
    let subscriber = tracing_subscriber::fmt()
        .with_writer(CaptureWriter(buf))
        .with_max_level(tracing::Level::ERROR)
        // No ANSI in the buffer — keeps the substring match clean.
        .with_ansi(false)
        // Trim noise that varies across runs.
        .without_time()
        .with_target(false)
        .finish();
    tracing::subscriber::set_default(subscriber)
}

fn empty_mesh() -> Mesh {
    Mesh {
        id: MeshId::from_u128(1),
        name: "injection-test".into(),
        join_key_hash: [9u8; 32],
        members: HashMap::new(),
        peers: vec![],
    }
}

fn fresh_app_state() -> AppState {
    let self_id = NodeId::from_u128(0xDEAD_BEEF_CAFE_F00D);
    let mesh_store = Arc::new(MeshStore::in_memory().unwrap());
    let app_registry = Arc::new(AppRegistry::new());
    AppState::new_with_platform_and_engine(
        self_id,
        empty_mesh(),
        mesh_store,
        app_registry,
        None,
    )
}

/// Trivial `InferenceProvider` stub. Never actually invoked — only
/// used to construct the `SovereignInferenceAdapter` we hand to
/// `with_local_inference`.
struct NoopProvider;

#[async_trait]
impl InferenceProvider for NoopProvider {
    async fn complete(&self, _: &CompletionRequest) -> SovResult<CompletionResponse> {
        unreachable!("test never invokes complete")
    }
    async fn complete_stream(
        &self,
        _: &CompletionRequest,
    ) -> SovResult<Pin<Box<dyn Stream<Item = SovResult<String>> + Send>>> {
        unreachable!("test never invokes complete_stream")
    }
    async fn embed(&self, _: &str) -> SovResult<Vec<f32>> {
        unreachable!("test never invokes embed")
    }
    fn model_id_for(&self, _: Speed) -> String {
        "noop".into()
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 1,
            supports_structured_output: false,
            relative_speed: Speed::Fast,
            relative_reasoning: sovereign_core::types::Depth::Moderate,
        }
    }
}

fn captured(buf: &Arc<Mutex<Vec<u8>>>) -> String {
    String::from_utf8_lossy(&buf.lock().unwrap()).to_string()
}

#[test]
fn with_local_inference_emits_error_when_arc_already_cloned() {
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let _guard = capture_subscriber(Arc::clone(&buf));

    let app_state = fresh_app_state();

    // Force the silent-no-op condition: bump the strong count BEFORE
    // calling the installer. `Arc::get_mut` inside `with_local_inference`
    // sees strong_count == 2 and returns None.
    let _kept = app_state.inner.clone();

    let provider: Arc<dyn InferenceProvider> = Arc::new(NoopProvider);
    let adapter: Arc<dyn LocalInferenceService> =
        Arc::new(SovereignInferenceAdapter::new(provider));
    let app_state = app_state.with_local_inference(adapter);

    // The installer should have logged the documented error.
    let out = captured(&buf);
    assert!(
        out.contains("with_local_inference called on shared AppState"),
        "expected the silent-no-op error in captured tracing; got:\n{out}"
    );

    // And the installation should have silently no-op'd:
    // `local_inference` stays `None`.
    assert!(
        app_state.inner.local_inference.is_none(),
        "with_local_inference must NOT have installed the service when Arc was cloned"
    );
}

#[test]
fn with_mesh_mutation_hook_emits_error_when_arc_already_cloned() {
    // Sister assertion — the second installer with the same Arc
    // contract. A bug that re-orders ONE of them but not the other
    // would slip past a test that only covers `with_local_inference`.
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let _guard = capture_subscriber(Arc::clone(&buf));

    let app_state = fresh_app_state();
    let _kept = app_state.inner.clone();

    let hook: commonwealth_api::state::MeshMutationHook =
        Arc::new(|_mesh: &Mesh, _self_id: NodeId| {
            // Body intentionally empty — the test isn't about firing
            // the hook, only about catching the install-time no-op.
        });
    let app_state = app_state.with_mesh_mutation_hook(hook);

    let out = captured(&buf);
    assert!(
        out.contains("with_mesh_mutation_hook called on shared AppState"),
        "expected the silent-no-op error in captured tracing; got:\n{out}"
    );

    assert!(
        app_state.inner.on_mesh_mutation.is_none(),
        "with_mesh_mutation_hook must NOT have installed the hook when Arc was cloned"
    );
}

#[test]
fn happy_path_does_not_emit_error_when_arc_uncloned() {
    // Negative control: when the Arc has strong_count == 1, both
    // installers succeed silently. Without this we can't tell if
    // the substring match above is firing on real evidence vs
    // some other log line that happens to mention "shared AppState".
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let _guard = capture_subscriber(Arc::clone(&buf));

    let app_state = fresh_app_state();
    let provider: Arc<dyn InferenceProvider> = Arc::new(NoopProvider);
    let adapter: Arc<dyn LocalInferenceService> =
        Arc::new(SovereignInferenceAdapter::new(provider));
    let app_state = app_state.with_local_inference(adapter);

    let hook: commonwealth_api::state::MeshMutationHook =
        Arc::new(|_mesh: &Mesh, _self_id: NodeId| {});
    let app_state = app_state.with_mesh_mutation_hook(hook);

    let out = captured(&buf);
    assert!(
        !out.contains("called on shared AppState"),
        "happy path must not emit the silent-no-op error; got:\n{out}"
    );

    assert!(
        app_state.inner.local_inference.is_some(),
        "happy path: local_inference installed"
    );
    assert!(
        app_state.inner.on_mesh_mutation.is_some(),
        "happy path: mesh mutation hook installed"
    );
}
