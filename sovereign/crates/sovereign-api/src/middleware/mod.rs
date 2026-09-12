// SPDX-License-Identifier: AGPL-3.0-or-later
//! ATOS middleware framework — the spine of the M4 "sovereign-coder is
//! the only surface" design.
//!
//! Architecture
//! ------------
//!
//! When a client POSTs `/v1/chat/completions` with a model name that
//! resolves to a [`PipelineResolution`](serving_policy::pipeline_aliases::PipelineResolution),
//! the handler constructs a [`Pipeline`] from the resolution and runs
//! it against the request before falling into the existing priority
//! routing. Each [`Middleware`] in the pipeline sees the mutable
//! request + a mutable session handle, and can:
//!
//! - prepend context to the system prompt (ContextInjector);
//! - veto the request entirely (ApprovalGate on write-intent tool
//!   calls when the feature isn't approved);
//! - rewrite the tool list (ToolInjector merging ATOS tool defs).
//!
//! Ordering matters: the pipeline runs middleware in the order
//! declared in `default_pipelines.toml`. Typical order —
//! `approval_gate` → `context_injector` → `tool_injector` — means
//! unapproved requests short-circuit before any context work, and
//! the model's final tool list reflects everything context-aware
//! decisions added.
//!
//! Errors
//! ------
//!
//! A middleware `Err` short-circuits the chain and is returned to the
//! caller (the `chat_completions` handler). The handler maps each
//! variant into an OpenAI-compatible error response so opencode
//! surfaces the failure as a model error rather than a transport
//! failure — the operator sees a coherent rejection, not a
//! mysterious 500.
//!
//! The trait is async-sans-Box so the concrete middleware
//! implementations stay testable without instantiating a runtime per
//! case. `#[async_trait]` is used for the dyn-safe indirection the
//! executor needs.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::openai_types::ChatCompletionRequest;

#[cfg(feature = "atos")]
pub mod approval_gate;
#[cfg(feature = "atos")]
pub mod artifact_surface;
#[cfg(feature = "atos")]
pub mod context_injector;
pub mod decision_extractor;
#[cfg(feature = "atos")]
pub mod session_briefing;
pub(crate) mod shared;
pub mod tool_injector;

#[cfg(feature = "atos")]
pub use approval_gate::ApprovalGate;
#[cfg(feature = "atos")]
pub use artifact_surface::ArtifactSurface;
#[cfg(feature = "atos")]
pub use context_injector::ContextInjector;
pub use decision_extractor::DecisionExtractor;
#[cfg(feature = "atos")]
pub use session_briefing::SessionBriefing;
pub use tool_injector::ToolInjector;

/// Everything a middleware might need to know about the request that
/// isn't in `ChatCompletionRequest` itself — resolved pipeline config,
/// feature id lifted from `X-Feature-Id`, session id, etc.
///
/// Immutable for the duration of a request. Mutable state lives on
/// [`MiddlewareSession`].
#[derive(Debug, Clone)]
pub struct PipelineContext {
    /// Pipeline name (e.g., "sovereign-coder").
    pub pipeline_name: String,
    /// The concrete model the pipeline resolves to (e.g., "qwen-27b-coder").
    pub model_id: String,
    /// Per-pipeline context-injection flags loaded from the alias
    /// table. ContextInjector reads this.
    pub context_config: serving_policy::pipeline_aliases::PipelineContextConfig,
    /// Feature the session is currently working on. Extracted from
    /// the `X-Feature-Id` request header; `None` if the plugin
    /// didn't inject one (ambiguous branch, or client isn't ATOS).
    pub feature_id: Option<String>,
    /// Opencode session id extracted from `X-Session-Id`. `None` if
    /// the client didn't send one.
    pub session_id: Option<String>,
    /// Repo root the Commonwealth daemon is anchored to — the
    /// directory that contains `.sovereign/features/`. Used by
    /// ApprovalGate for git lookups and by ContextInjector for
    /// reading spec.md.
    pub repo_root: std::path::PathBuf,
}

/// Mutable session state handed to each middleware. The executor
/// loads this from `MeshStore` on request entry and persists it on
/// exit. Mirrors the subset of [`sovereign_atos::session::AtosSessionState`]
/// middleware actually touch.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MiddlewareSession {
    pub feature_id: Option<String>,
    pub approval_validated: bool,
    pub spec_content_hash: Option<String>,
    pub pending_deviation_ack: bool,
    pub deviation_note_id: Option<String>,
    /// Populated by `ArtifactSurface.post_process` on turn N;
    /// consumed by `ContextInjector.process` on turn N+1. Optional
    /// so fresh sessions don't have to seed an empty delta. ATOS-only.
    #[cfg(feature = "atos")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_artifact_delta: Option<sovereign_atos::session::ArtifactDelta>,
    /// Unix-second timestamp of the *previous* turn. Post-path
    /// middleware use this to scope queries ("notes written since
    /// last turn"). Set by the handler before running middleware
    /// so all middleware see a consistent baseline.
    #[serde(default)]
    pub last_seen_at: i64,
    /// Phase 7.2: a candidate decision sentence that
    /// `decision_extractor.post_process` mined from the previous
    /// turn's assistant response. `decision_extractor.process` on
    /// the NEXT turn either:
    ///
    /// 1. Detects a correction phrase in the user's latest message
    ///    (e.g. "actually, that's not a decision") → drops the
    ///    candidate without persisting it, or
    /// 2. Persists it as a `source='extracted'` note and injects
    ///    `[Noted: "<snippet>". Auto-recording unless corrected.]`
    ///    into the system prompt so the agent sees the audit
    ///    trail.
    ///
    /// Cleared after use either way. `None` when no candidate is
    /// pending — the steady-state for sessions that aren't
    /// surfacing decisions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_decision: Option<String>,
}

/// Errors a middleware can raise. The handler pattern-matches on
/// these to pick the right HTTP status + OpenAI error envelope.
#[derive(Debug, thiserror::Error)]
pub enum MiddlewareError {
    /// The feature hasn't been approved and the request would
    /// trigger a write-intent tool. Includes a human-readable hint
    /// so opencode surfaces something actionable.
    #[error("feature '{feature_id}' is not approved: {hint}")]
    ApprovalRequired { feature_id: String, hint: String },

    /// The request is structurally incompatible with the pipeline
    /// (e.g., a red-team session containing a write tool call that
    /// the read-only enforcer blocked).
    #[error("pipeline rejected request: {0}")]
    PipelineRejected(String),

    /// Infrastructure error — MeshStore unavailable, git lookup
    /// failed, etc. These surface as 500s, not 403s.
    #[error("middleware infrastructure error: {0}")]
    Infra(String),
}

/// Read-only view handed to post-path middleware. Assembles the
/// model output from whichever path produced it (non-streaming =
/// `choices[0].message.content`; streaming = concatenated SSE
/// deltas). Middleware observe via this view and stage mutations
/// on `session`; they do NOT mutate the response bytes that reach
/// the client.
///
/// `finish_reason` is `Some("stop")` on clean completion,
/// `Some("tool_calls")` when the model asked for tool execution,
/// or `None` when the adapter couldn't reconstruct one (usually a
/// streaming error).
#[derive(Debug)]
pub struct ResponseView<'a> {
    pub content: &'a str,
    pub finish_reason: Option<&'a str>,
    pub tool_calls_emitted: usize,
}

/// The contract every middleware in the pipeline implements.
#[async_trait]
pub trait Middleware: Send + Sync {
    /// Short identifier matching the string in `default_pipelines.toml`.
    /// Used by the executor to look up a middleware by name when
    /// assembling the pipeline from a resolution.
    fn id(&self) -> &'static str;

    /// Process a request. Implementations mutate `request` and
    /// `session` in place; return `Ok(())` to continue the chain or
    /// `Err(MiddlewareError)` to short-circuit.
    async fn process(
        &self,
        request: &mut ChatCompletionRequest,
        session: &mut MiddlewareSession,
        ctx: &PipelineContext,
    ) -> Result<(), MiddlewareError>;

    /// Post-inference hook. Default impl is a no-op so existing
    /// middleware don't have to re-implement. Called AFTER the
    /// model response has been assembled. For streaming requests,
    /// called from a detached `tokio::spawn` at stream-end so the
    /// client never waits on it.
    ///
    /// Errors from post_process are **logged, not propagated** —
    /// the response has already gone to the client, and post-path
    /// work is best-effort telemetry. See `Pipeline::run_post`.
    async fn post_process(
        &self,
        _response: &ResponseView<'_>,
        _session: &mut MiddlewareSession,
        _ctx: &PipelineContext,
    ) -> Result<(), MiddlewareError> {
        Ok(())
    }
}

/// Ordered chain of middleware. Built once per request from a
/// [`PipelineResolution`] + a registry of available middleware.
pub struct Pipeline {
    middleware: Vec<Arc<dyn Middleware>>,
}

impl std::fmt::Debug for Pipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pipeline")
            .field(
                "middleware",
                &self.middleware.iter().map(|m| m.id()).collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl Pipeline {
    pub fn new(middleware: Vec<Arc<dyn Middleware>>) -> Self {
        Self { middleware }
    }

    pub fn len(&self) -> usize {
        self.middleware.len()
    }

    pub fn is_empty(&self) -> bool {
        self.middleware.is_empty()
    }

    /// Run every middleware in declared order. Stops at the first
    /// error, leaving `request` / `session` in whatever state the
    /// offending middleware wrote before returning — callers should
    /// NOT use the partial output for any other purpose.
    pub async fn run(
        &self,
        request: &mut ChatCompletionRequest,
        session: &mut MiddlewareSession,
        ctx: &PipelineContext,
    ) -> Result<(), MiddlewareError> {
        for mw in &self.middleware {
            let started = std::time::Instant::now();
            let outcome = mw.process(request, session, ctx).await;
            let ms = started.elapsed().as_millis() as u64;
            tracing::debug!(
                middleware = %mw.id(),
                phase = "pre",
                duration_ms = ms,
                ok = outcome.is_ok(),
                "pipeline step"
            );
            outcome?;
        }
        Ok(())
    }

    /// Post-inference phase. Runs every middleware's
    /// [`Middleware::post_process`] in pipeline order. Errors are
    /// **logged, not propagated** — the response has already left
    /// the server, and post-path work is best-effort telemetry
    /// that shouldn't convert a successful turn into a failure.
    ///
    /// Call this inline on non-streaming responses (cheap;
    /// <10ms budget). On streaming responses, call it from
    /// `tokio::spawn` at stream-end so the client never waits.
    pub async fn run_post(
        &self,
        response: &ResponseView<'_>,
        session: &mut MiddlewareSession,
        ctx: &PipelineContext,
    ) {
        for mw in &self.middleware {
            let started = std::time::Instant::now();
            let outcome = mw.post_process(response, session, ctx).await;
            let ms = started.elapsed().as_millis() as u64;
            match outcome {
                Ok(()) => tracing::debug!(
                    middleware = %mw.id(),
                    phase = "post",
                    duration_ms = ms,
                    "pipeline step"
                ),
                Err(e) => tracing::warn!(
                    middleware = %mw.id(),
                    phase = "post",
                    duration_ms = ms,
                    err = %e,
                    "pipeline post_process failed (non-fatal)"
                ),
            }
        }
    }
}

/// Registry of middleware implementations addressable by id. Built
/// once at daemon startup; the pipeline executor looks up the
/// implementations named in a `PipelineResolution` against it.
///
/// Using a registry (not direct Arc<dyn Middleware> in the
/// resolution) keeps the alias table serializable as TOML — we
/// name middleware, we don't embed them.
#[derive(Default)]
pub struct MiddlewareRegistry {
    by_id: std::collections::HashMap<String, Arc<dyn Middleware>>,
}

impl MiddlewareRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, mw: Arc<dyn Middleware>) {
        self.by_id.insert(mw.id().to_string(), mw);
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn Middleware>> {
        self.by_id.get(id).cloned()
    }

    /// Build a [`Pipeline`] from an ordered list of middleware ids.
    /// An unknown id returns an error — a typo in
    /// `default_pipelines.toml` should fail loud at request time, not
    /// silently skip the middleware.
    pub fn build_pipeline(&self, ids: &[String]) -> Result<Pipeline, MiddlewareError> {
        let mut chain = Vec::with_capacity(ids.len());
        for id in ids {
            let mw = self.get(id).ok_or_else(|| {
                MiddlewareError::Infra(format!("unknown middleware id in pipeline: '{id}'"))
            })?;
            chain.push(mw);
        }
        Ok(Pipeline::new(chain))
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::shared::fixtures::{ctx_with, request_with_messages};
    use super::*;
    use crate::openai_types::ChatCompletionRequest;

    fn make_request() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: Some("commonwealth/sovereign-coder".into()),
            ..request_with_messages(&[("user", "hello")])
        }
    }

    fn make_ctx() -> PipelineContext {
        ctx_with(Some("fx"), std::env::temp_dir())
    }

    /// Minimal counting middleware — records how many times it ran.
    struct CountingMw {
        id: &'static str,
        count: std::sync::atomic::AtomicUsize,
    }
    impl CountingMw {
        fn new(id: &'static str) -> Arc<Self> {
            Arc::new(Self {
                id,
                count: std::sync::atomic::AtomicUsize::new(0),
            })
        }
    }
    #[async_trait]
    impl Middleware for CountingMw {
        fn id(&self) -> &'static str {
            self.id
        }
        async fn process(
            &self,
            _request: &mut ChatCompletionRequest,
            _session: &mut MiddlewareSession,
            _ctx: &PipelineContext,
        ) -> Result<(), MiddlewareError> {
            self.count
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        }
    }

    /// Short-circuits with ApprovalRequired.
    struct RejectingMw;
    #[async_trait]
    impl Middleware for RejectingMw {
        fn id(&self) -> &'static str {
            "rejecting"
        }
        async fn process(
            &self,
            _request: &mut ChatCompletionRequest,
            _session: &mut MiddlewareSession,
            _ctx: &PipelineContext,
        ) -> Result<(), MiddlewareError> {
            Err(MiddlewareError::ApprovalRequired {
                feature_id: "fx".into(),
                hint: "test".into(),
            })
        }
    }

    #[tokio::test]
    async fn pipeline_runs_all_middleware_in_order() {
        let a = CountingMw::new("a");
        let b = CountingMw::new("b");
        let pipeline = Pipeline::new(vec![a.clone(), b.clone()]);
        let mut req = make_request();
        let mut session = MiddlewareSession::default();
        let ctx = make_ctx();
        pipeline.run(&mut req, &mut session, &ctx).await.unwrap();
        assert_eq!(a.count.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(b.count.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn pipeline_short_circuits_on_error() {
        let a = CountingMw::new("a");
        let reject = Arc::new(RejectingMw);
        let c = CountingMw::new("c");
        let pipeline = Pipeline::new(vec![a.clone(), reject, c.clone()]);
        let mut req = make_request();
        let mut session = MiddlewareSession::default();
        let ctx = make_ctx();
        let err = pipeline
            .run(&mut req, &mut session, &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, MiddlewareError::ApprovalRequired { .. }));
        assert_eq!(a.count.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(
            c.count.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "middleware after the error must not run"
        );
    }

    #[tokio::test]
    async fn registry_builds_pipeline_by_id() {
        let a = CountingMw::new("a");
        let b = CountingMw::new("b");
        let mut registry = MiddlewareRegistry::new();
        registry.register(a.clone());
        registry.register(b.clone());
        let pipeline = registry
            .build_pipeline(&["a".to_string(), "b".to_string()])
            .unwrap();
        assert_eq!(pipeline.len(), 2);
    }

    #[tokio::test]
    async fn registry_errors_on_unknown_middleware_id() {
        let registry = MiddlewareRegistry::new();
        let err = registry
            .build_pipeline(&["never-registered".to_string()])
            .unwrap_err();
        assert!(matches!(err, MiddlewareError::Infra(_)));
    }

    /// Phase 7.2 gap A: the production middleware list (mirroring
    /// `state.rs::AppState::new_with_*`) must include
    /// `decision_extractor`, AND the default `sovereign-coder`
    /// pipeline declared in `default_pipelines.toml` must
    /// resolve cleanly through that registry. A typo on either
    /// side fails this test loud — without it the wiring could
    /// silently drift again.
    #[cfg(feature = "atos")]
    #[tokio::test]
    async fn sovereign_coder_default_pipeline_resolves_decision_extractor() {
        // Mirror the production registry from
        // `commonwealth-api::state::AppState`.
        let mut registry = MiddlewareRegistry::new();
        registry.register(Arc::new(ApprovalGate::new()));
        registry.register(Arc::new(ContextInjector::empty()));
        registry.register(Arc::new(ToolInjector::empty()));
        registry.register(Arc::new(ArtifactSurface::new()));
        registry.register(Arc::new(SessionBriefing::new()));
        registry.register(Arc::new(DecisionExtractor::new()));

        // Resolve the toml-declared chain.
        let table = serving_policy::pipeline_aliases::PipelineAliasTable::default_table();
        let resolution = table
            .resolve("sovereign-coder")
            .expect("sovereign-coder pipeline must exist in default_pipelines.toml");
        let pipeline = registry
            .build_pipeline(&resolution.middleware)
            .expect("every id in sovereign-coder must resolve in the registry");
        assert_eq!(
            pipeline.len(),
            resolution.middleware.len(),
            "all middleware ids resolved into the chain"
        );
    }

    #[tokio::test]
    async fn empty_pipeline_is_noop_ok() {
        let pipeline = Pipeline::new(vec![]);
        let mut req = make_request();
        let mut session = MiddlewareSession::default();
        let ctx = make_ctx();
        assert!(pipeline.run(&mut req, &mut session, &ctx).await.is_ok());
    }

    // ── Post-path tests (M5.1) ──────────────────────────────────────────

    struct PostCounter {
        id: &'static str,
        pre: std::sync::atomic::AtomicUsize,
        post: std::sync::atomic::AtomicUsize,
    }
    impl PostCounter {
        fn new(id: &'static str) -> Arc<Self> {
            Arc::new(Self {
                id,
                pre: Default::default(),
                post: Default::default(),
            })
        }
    }
    #[async_trait]
    impl Middleware for PostCounter {
        fn id(&self) -> &'static str {
            self.id
        }
        async fn process(
            &self,
            _request: &mut ChatCompletionRequest,
            _session: &mut MiddlewareSession,
            _ctx: &PipelineContext,
        ) -> Result<(), MiddlewareError> {
            self.pre.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        }
        async fn post_process(
            &self,
            _response: &ResponseView<'_>,
            _session: &mut MiddlewareSession,
            _ctx: &PipelineContext,
        ) -> Result<(), MiddlewareError> {
            self.post.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        }
    }

    /// Deliberately errors on post_process to prove we don't
    /// propagate failures from the post path.
    struct PostBoom;
    #[async_trait]
    impl Middleware for PostBoom {
        fn id(&self) -> &'static str {
            "boom"
        }
        async fn process(
            &self,
            _request: &mut ChatCompletionRequest,
            _session: &mut MiddlewareSession,
            _ctx: &PipelineContext,
        ) -> Result<(), MiddlewareError> {
            Ok(())
        }
        async fn post_process(
            &self,
            _response: &ResponseView<'_>,
            _session: &mut MiddlewareSession,
            _ctx: &PipelineContext,
        ) -> Result<(), MiddlewareError> {
            Err(MiddlewareError::Infra("intentional test failure".into()))
        }
    }

    fn make_response() -> &'static str {
        "Yes — milestone 1 complete."
    }

    #[tokio::test]
    async fn run_post_calls_all_post_process_in_order() {
        let a = PostCounter::new("a");
        let b = PostCounter::new("b");
        let pipeline = Pipeline::new(vec![a.clone(), b.clone()]);
        let content = make_response();
        let view = ResponseView {
            content,
            finish_reason: Some("stop"),
            tool_calls_emitted: 0,
        };
        let mut session = MiddlewareSession::default();
        let ctx = make_ctx();
        pipeline.run_post(&view, &mut session, &ctx).await;
        assert_eq!(a.post.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(b.post.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn run_post_swallows_errors_non_fatal() {
        // Error in one middleware's post_process must not prevent
        // the next one from running — post-path is best-effort
        // telemetry, not a short-circuitable chain.
        let a = PostCounter::new("a");
        let boom = Arc::new(PostBoom);
        let c = PostCounter::new("c");
        let pipeline = Pipeline::new(vec![a.clone(), boom, c.clone()]);
        let content = make_response();
        let view = ResponseView {
            content,
            finish_reason: Some("stop"),
            tool_calls_emitted: 0,
        };
        let mut session = MiddlewareSession::default();
        let ctx = make_ctx();
        pipeline.run_post(&view, &mut session, &ctx).await;
        assert_eq!(a.post.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(
            c.post.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "middleware after a post-path failure must still run"
        );
    }

    #[tokio::test]
    async fn default_post_process_is_noop() {
        // M4 middleware that never implemented post_process must
        // still flow through run_post without error.
        let a = CountingMw::new("noop-default");
        let pipeline = Pipeline::new(vec![a.clone()]);
        let view = ResponseView {
            content: "ok",
            finish_reason: Some("stop"),
            tool_calls_emitted: 0,
        };
        let mut session = MiddlewareSession::default();
        let ctx = make_ctx();
        pipeline.run_post(&view, &mut session, &ctx).await;
        // No panics, no errors — default impl ran.
    }
}

#[cfg(test)]
mod tool_vocabulary_boundary {
    //! The MODULE-level half of the layer rule for the injector middlewares.
    //!
    //! `commonwealth` is layer 1 and `sovereign` is layer 2, so this crate must
    //! not reach UP for vocabulary. The injectors need tool descriptors — that
    //! is their whole job — and until noun-convergence rung 2c they got them by
    //! naming the agent runtime hub's `types` module, which was 60 of the 98
    //! references on the `commonwealth -> sovereign` backflow edge. The
    //! definitions now live in `oicp-types` (layer 0) and the hub re-exports
    //! them at their historical path, so the reach is gone.
    //!
    //! This test is what stops it coming back by habit: the next author who
    //! needs `Effect` or `ToolDescriptor` here reaches for whatever import the
    //! IDE suggests, and the hub's re-export is still a valid path. ARCH §7 —
    //! structural, not remembered.
    //!
    //! Deliberately narrow. `sovereign-atos` and `sovereign-tools` are still
    //! named in this directory (the feature-gated ATOS surface and
    //! `notes::response_mine`); both carry their own `[[exception]]` in
    //! `quality/ARCH_LAYERS.toml` tracked at R6 and are not this rung's
    //! subject. What this asserts is exactly what rung 2c family A bought.
    //!
    //! Failing input, if you want to watch it fail: put
    //! `Vec<…::types::ToolDescriptor>` back on `ContextInjector::new`.

    #[test]
    fn no_injector_names_the_agent_runtime_hub() {
        // Assembled at runtime, never written as a literal — THIS FILE IS
        // INSIDE THE SCANNED TREE, so a literal would make the guard match its
        // own source. Keep the token out of this file entirely, doc comments
        // and assertion text included.
        let needle = ["sovereign", "core"].join("_");

        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("middleware");
        let mut offenders = Vec::new();
        let mut scanned = 0usize;
        for entry in std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
            .flatten()
        {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            scanned += 1;
            if std::fs::read_to_string(&path)
                .unwrap_or_default()
                .contains(&needle)
            {
                offenders.push(path.file_name().unwrap().to_string_lossy().into_owned());
            }
        }

        // An empty walk would pass while proving nothing — the classic
        // zero-case false green (ARCH §18.1).
        assert!(
            scanned >= 8,
            "scanned only {scanned} middleware files; the walk is broken, not the boundary"
        );
        offenders.sort();
        assert!(
            offenders.is_empty(),
            "middleware reaching up a layer for vocabulary that lives in oicp-types: {offenders:?}"
        );
    }
}
