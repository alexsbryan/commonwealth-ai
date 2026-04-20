//! ATOS middleware framework — the spine of the M4 "sovereign-coder is
//! the only surface" design.
//!
//! Architecture
//! ------------
//!
//! When a client POSTs `/v1/chat/completions` with a model name that
//! resolves to a [`PipelineResolution`](commonwealth_core::pipeline_aliases::PipelineResolution),
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

pub mod approval_gate;
pub mod context_injector;
pub mod tool_injector;

pub use approval_gate::ApprovalGate;
pub use context_injector::ContextInjector;
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
    pub context_config: commonwealth_core::pipeline_aliases::PipelineContextConfig,
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
/// loads this from `MeshStore` (via sovereign-atos, once M4.3 lands)
/// on request entry and persists it on exit. For M4.2 the executor
/// owns the state directly; M4.4 swaps in MeshStore-backed loading.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MiddlewareSession {
    pub feature_id: Option<String>,
    pub approval_validated: bool,
    pub spec_content_hash: Option<String>,
    pub pending_deviation_ack: bool,
    pub deviation_note_id: Option<String>,
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
            tracing::debug!(middleware = %mw.id(), "pipeline step begin");
            mw.process(request, session, ctx).await?;
        }
        Ok(())
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
    pub fn build_pipeline(
        &self,
        ids: &[String],
    ) -> Result<Pipeline, MiddlewareError> {
        let mut chain = Vec::with_capacity(ids.len());
        for id in ids {
            let mw = self.get(id).ok_or_else(|| {
                MiddlewareError::Infra(format!(
                    "unknown middleware id in pipeline: '{id}'"
                ))
            })?;
            chain.push(mw);
        }
        Ok(Pipeline::new(chain))
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openai_types::{ChatCompletionRequest, ChatMessage};

    fn make_request() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: Some("commonwealth/sovereign-coder".into()),
            messages: vec![ChatMessage::new("user", "hello")],
            temperature: None,
            max_tokens: None,
            stream: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            tools: None,
            tool_choice: None,
            oicp: None,
        }
    }

    fn make_ctx() -> PipelineContext {
        PipelineContext {
            pipeline_name: "test".into(),
            model_id: "qwen-27b-coder".into(),
            context_config: Default::default(),
            feature_id: Some("fx".into()),
            session_id: Some("sess-1".into()),
            repo_root: std::env::temp_dir(),
        }
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
            self.count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
        let err = pipeline.run(&mut req, &mut session, &ctx).await.unwrap_err();
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

    #[tokio::test]
    async fn empty_pipeline_is_noop_ok() {
        let pipeline = Pipeline::new(vec![]);
        let mut req = make_request();
        let mut session = MiddlewareSession::default();
        let ctx = make_ctx();
        assert!(pipeline.run(&mut req, &mut session, &ctx).await.is_ok());
    }
}
