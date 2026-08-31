// SPDX-License-Identifier: AGPL-3.0-or-later
//! The landscape-digest splice invariant, pinned as a test rather than a comment.
//!
//! `ConversationContext::debug_assert_routed` (sovereign-contracts
//! `types/conversation.rs`) requires `knowledge_view_digests` to be `Some` by the
//! time prompt assembly runs. `streaming.rs` states the same rule in prose —
//! "IMPORTANT: this MUST run before any intent-specific dispatch" — and then
//! dispatches four intents from a `return` placed *above* the splice. Prose can't
//! fail a build; this file can (ARCH_PRINCIPLES §12.3).
//!
//! Why it shipped: no test ever installed a `LandscapeDigestProvider`, so
//! `Runtime::landscape_digests` was `None` in every CI run and the guard at
//! `system_message.rs` — which only fires when a provider IS installed — was
//! structurally dead code (§18.1: a gate never observed to fail). Installing the
//! stub below is the whole point of this harness.
//!
//! Observed 2026-08-06 in an 8-hour desktop soak: 12 panics, 7 of 63 persona
//! turns returned no answer at all. Crash dumps named `handle_metalingual_query`
//! (×5) and `handle_complex_task` (×1).

use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::{Stream, StreamExt};

use sovereign_core::error::Result;
use sovereign_core::executor::AutoApprovalChannel;
use sovereign_core::planner::LlmPlanner;
use sovereign_core::runtime::Runtime;
use sovereign_core::traits::*;
use sovereign_core::types::*;
use sovereign_core::SkillRegistry;
use sovereign_core::ToolRegistry;
use sovereign_store::sqlite::SqliteStateStore;

use crate::harness;
use crate::harness::DeterministicInference;

/// Minimal provider that satisfies the invariant the way the real
/// `KnowledgeViewManager` does — leave the field `Some`, possibly empty — and
/// **records whether the Runtime called it at all**.
///
/// That recording is the actual assertion. Waiting for the downstream
/// `debug_assert` to panic makes the test hostage to how far each handler
/// happens to get: `handle_metalingual_query` only reaches prompt assembly when
/// a locator resolves a prior source, so against an empty store it returns
/// early and the test goes green having exercised nothing (§18.1 — a check that
/// passes for the wrong reason is worse than no check; observed on this very
/// file's first run).
///
/// The contract is "the Runtime calls `splice_landscape_digests` between
/// `build_context()` and dispatch". `was_called()` tests exactly that, for every
/// intent, whether or not the handler goes on to assemble a prompt.
#[derive(Default)]
struct StubDigestProvider {
    called: std::sync::atomic::AtomicBool,
}

impl StubDigestProvider {
    fn was_called(&self) -> bool {
        self.called.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait]
impl LandscapeDigestProvider for StubDigestProvider {
    async fn splice_landscape_digests(
        &self,
        ctx: &mut ConversationContext,
        _active_skill: Option<&str>,
    ) {
        self.called.store(true, std::sync::atomic::Ordering::SeqCst);
        ctx.set_landscape_digests(Vec::new());
    }
}

/// A router that must never be consulted — every test here pins the intent via
/// `handle_message_stream_as`. If classification runs, the pinning seam broke
/// and the test is no longer exercising the intent it names.
struct UnusedRouter;

#[async_trait]
impl Router for UnusedRouter {
    async fn classify(
        &self,
        _message: &str,
        _context: &ConversationContext,
        _available_tools: &[ToolDescriptor],
    ) -> Result<RouterClassification> {
        panic!("intent is pinned — classify must not be called");
    }
}

async fn runtime_with_digest_provider() -> (Runtime, Arc<StubDigestProvider>) {
    let provider = Arc::new(StubDigestProvider::default());
    let inference: Arc<dyn InferenceProvider> = Arc::new(DeterministicInference);
    let shared_store = Arc::new(SqliteStateStore::open_in_memory().unwrap());
    let store_trait: Arc<dyn StateStore> = Arc::clone(&shared_store) as Arc<dyn StateStore>;
    let skills = Arc::new(SkillRegistry::new());
    let planner = LlmPlanner::new(Arc::clone(&inference), Arc::clone(&skills));
    let tools = Arc::new(ToolRegistry::new());
    let approval: Arc<dyn ApprovalChannel> = Arc::new(AutoApprovalChannel);

    let runtime = Runtime::new(sovereign_core::RuntimeParts {
        landscape_digests: Some(Arc::clone(&provider) as Arc<dyn LandscapeDigestProvider>),
        ..sovereign_core::RuntimeParts::new(
            inference,
            Box::new(UnusedRouter),
            Box::new(planner),
            tools,
            store_trait,
            skills,
            approval,
            InferenceConfig::default(),
            // Phase 4b: enrichment is a required argument, not eight
            // forgettable builders.
            sovereign_core::runtime::lane::LaneSources::none(),
        )
    });

    (runtime, provider)
}

/// Drive one streaming turn with `intent` pinned, then assert the Runtime
/// spliced before dispatching.
///
/// A handler returning `Err` is a legitimate outcome under
/// `DeterministicInference` — the invariant under test is about what the Runtime
/// does *before* the handler runs, so the turn's own success is not the subject.
async fn assert_splices_before_dispatch(intent: Intent) {
    let (runtime, provider) = runtime_with_digest_provider().await;
    let conv = uuid::Uuid::new_v4().to_string();

    let handle = runtime
        .handle_message_stream_as("what did we talk about earlier?", &conv, intent.clone())
        .await;

    if let Ok(h) = handle {
        let stream: Pin<Box<dyn Stream<Item = Result<String>> + Send>> = h.stream;
        let _: Vec<_> = stream.collect().await;
    }

    assert!(
        provider.was_called(),
        "{intent:?} was dispatched without calling splice_landscape_digests — \
         the context reaches prompt assembly with knowledge_view_digests=None. \
         Debug builds panic at conversation.rs debug_assert_routed; release \
         builds silently drop the landscape digest."
    );
}

// ─── The four intents dispatched from above the splice ───────

#[tokio::test]
async fn metalingual_query_reaches_prompt_assembly_spliced() {
    assert_splices_before_dispatch(Intent::MetalingualQuery).await;
}

#[tokio::test]
async fn complex_task_reaches_prompt_assembly_spliced() {
    assert_splices_before_dispatch(Intent::ComplexTask).await;
}

// These two dispatch from the same block but do not call `build_system_message`
// today, so they never panicked. They are pinned anyway: the block is one unit,
// and a future handler that starts assembling a system message would otherwise
// reintroduce the bug silently. Recorded as-is rather than normalised — the
// asymmetry is real (ARCH_PRINCIPLES §18.2: don't collapse distinct verdicts).
#[tokio::test]
async fn conation_query_reaches_prompt_assembly_spliced() {
    assert_splices_before_dispatch(Intent::ConationQuery).await;
}

#[tokio::test]
async fn commissive_query_reaches_prompt_assembly_spliced() {
    assert_splices_before_dispatch(Intent::CommissiveQuery).await;
}

// ─── Latent siblings: other returns above the splice ─────────

#[tokio::test]
async fn generative_query_reaches_prompt_assembly_spliced() {
    assert_splices_before_dispatch(Intent::GenerativeQuery).await;
}

#[tokio::test]
async fn expressive_query_reaches_prompt_assembly_spliced() {
    assert_splices_before_dispatch(Intent::ExpressiveQuery).await;
}
