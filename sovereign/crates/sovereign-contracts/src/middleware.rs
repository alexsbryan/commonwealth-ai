// SPDX-License-Identifier: AGPL-3.0-or-later
//! The middleware seam — Answering's port.
//!
//! A request that resolves to a pipeline alias runs through an ordered chain of
//! [`Middleware`] implementations before falling into priority routing. Each
//! middleware sees the mutable request and a mutable session handle, and can
//! prepend context to the system prompt, veto the request, or rewrite the tool
//! list. The composition — `Pipeline` and the registry that builds it — is
//! host code and stays with the daemon; this module is only the vocabulary the
//! two sides agree on, lifted out of `sovereign-api` by domains
//! `REVIEW-build-middleware-seam` so the Workspace decision extractor and the
//! ATOS middlewares can name it without the host (`quality/DAEMON_CORE.md`
//! §4.2 "Risks carried": the seam is Answering's port, not host code).
//!
//! The executor runs middleware in the order declared in
//! `default_pipelines.toml`. Typical order — `approval_gate` →
//! `context_injector` → `tool_injector` — means unapproved requests
//! short-circuit before any context work, and the model's final tool list
//! reflects everything context-aware decisions added.
//!
//! A middleware `Err` short-circuits the chain and is returned to the caller
//! (the `chat_completions` handler), which maps each [`MiddlewareError`] variant
//! into an OpenAI-compatible error response so a client surfaces the failure as
//! a model error rather than a transport failure. The trait is async-sans-Box so
//! the concrete implementations stay testable without instantiating a runtime
//! per case; `#[async_trait]` provides the dyn-safe indirection the executor
//! needs.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::oicp::openai_types::{ChatCompletionRequest, ChatMessage};

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
    /// table. The context injector reads this.
    pub context_config: crate::oicp::PipelineContextConfig,
    /// Feature the session is currently working on. Extracted from
    /// the `X-Feature-Id` request header; `None` if the plugin
    /// didn't inject one (ambiguous branch, or client isn't ATOS).
    pub feature_id: Option<String>,
    /// Opencode session id extracted from `X-Session-Id`. `None` if
    /// the client didn't send one.
    pub session_id: Option<String>,
    /// Repo root the Commonwealth daemon is anchored to — the
    /// directory that contains `.sovereign/features/`. Used by the
    /// approval gate for git lookups and by the context injector for
    /// reading `spec.md`.
    pub repo_root: std::path::PathBuf,
}

/// Mutable session state handed to each middleware. The executor
/// loads this from the mesh session store on request entry and persists it on
/// exit. Mirrors the subset of ATOS session state that middleware actually
/// touch.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MiddlewareSession {
    /// Feature the session is currently working on.
    pub feature_id: Option<String>,
    /// `true` once the approval gate has validated the feature.
    pub approval_validated: bool,
    /// SHA-256 of the feature's `spec.md` at approval time.
    pub spec_content_hash: Option<String>,
    /// Set when spec drift is detected; cleared once acknowledged.
    pub pending_deviation_ack: bool,
    /// Most recent deviation note id, surfaced in the drift reminder.
    pub deviation_note_id: Option<String>,
    /// Populated by `ArtifactSurface.post_process` on turn N;
    /// consumed by `ContextInjector.process` on turn N+1. Optional
    /// so fresh sessions don't have to seed an empty delta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_artifact_delta: Option<ArtifactDelta>,
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
    /// so a client surfaces something actionable.
    #[error("feature '{feature_id}' is not approved: {hint}")]
    ApprovalRequired {
        /// The feature id the request named.
        feature_id: String,
        /// A human-readable explanation of what to do about it.
        hint: String,
    },

    /// The request is structurally incompatible with the pipeline
    /// (e.g., a red-team session containing a write tool call that
    /// the read-only enforcer blocked).
    #[error("pipeline rejected request: {0}")]
    PipelineRejected(String),

    /// Infrastructure error — the session store unavailable, a git lookup
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
    /// The assembled model output text.
    pub content: &'a str,
    /// Why the model stopped, when the adapter could reconstruct it.
    pub finish_reason: Option<&'a str>,
    /// How many tool calls the model emitted this turn.
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
    /// work is best-effort telemetry. See the host's `Pipeline::run_post`.
    async fn post_process(
        &self,
        _response: &ResponseView<'_>,
        _session: &mut MiddlewareSession,
        _ctx: &PipelineContext,
    ) -> Result<(), MiddlewareError> {
        Ok(())
    }
}

/// What changed between the previous turn and now — staged by the artifact
/// surface's post_process, rendered by the context injector's process on the
/// next request. Pops on render so the preamble only shows it once.
///
/// It lives beside the seam rather than in `sovereign-atos` (its original home)
/// because [`MiddlewareSession`] carries it and this crate may not name an
/// upward crate; `sovereign-atos` re-exports it at `sovereign_atos::session`
/// so the existing paths are unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ArtifactDelta {
    /// Number of notes written this turn, grouped by kind.
    pub notes_by_kind: std::collections::BTreeMap<String, u32>,
    /// Up to 5 recent note ids per kind — surfaced so the agent can
    /// reference them by `[note:<id>]` in its next turn.
    pub recent_note_ids: std::collections::BTreeMap<String, Vec<String>>,
    /// Milestones whose `stop_passed` flipped true since
    /// `last_seen_at`. One entry per newly-passing milestone.
    pub milestones_passed: Vec<MilestonePassEvent>,
}

/// One milestone that flipped to passing since the previous turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MilestonePassEvent {
    /// The feature the milestone belongs to.
    pub feature_id: String,
    /// The milestone's ordinal within the feature.
    pub ordinal: i64,
    /// Relative path to the rendered artifact, e.g.
    /// `.sovereign/features/<id>/milestone-2.md`.
    pub artifact_path: String,
}

// ─── Shared conventions ──────────────────────────────────────────────────────
//
// The on-disk `.sovereign/` layout and the system-prompt prepend are shared by
// the ATOS middlewares (now in `sovereign-atos`) and the host middlewares that
// stay with the daemon, so they live beside the seam rather than on either
// side. Moved out of `sovereign-api/src/middleware/shared.rs` by domains
// `REVIEW-build-answering-inversion`; both sides name them through
// `sovereign_core::middleware::{notes_db_path, features_db_path,
// prepend_to_system}`.

/// Canonical location of the notes store relative to the repo the daemon is
/// anchored to: `<repo_root>/.sovereign/notes.db`.
///
/// Every middleware that opens a [`corpus_engine_notes::NoteStore`] must derive
/// the path through here so they all read/write the same file.
pub fn notes_db_path(repo_root: &std::path::Path) -> std::path::PathBuf {
    repo_root.join(".sovereign").join("notes.db")
}

/// Canonical location of the ATOS feature store:
/// `<repo_root>/.sovereign/features.db`. Same single-source rationale as
/// [`notes_db_path`].
pub fn features_db_path(repo_root: &std::path::Path) -> std::path::PathBuf {
    repo_root.join(".sovereign").join("features.db")
}

/// Prepend `text` to the first system message; or insert a fresh system
/// message at position 0 when none exists.
///
/// The join is exactly `"{text}\n{existing}"` — middlewares that run earlier in
/// the pipeline end up ABOVE later ones in the final system prompt, which is
/// what the declared pipeline order means.
pub fn prepend_to_system(request: &mut ChatCompletionRequest, text: &str) {
    for msg in &mut request.messages {
        if msg.role == "system" {
            msg.content = format!("{text}\n{}", msg.content);
            return;
        }
    }
    request
        .messages
        .insert(0, ChatMessage::new("system", text.to_string()));
}

/// Canonical test fixtures for middleware tests — the single
/// field-enumerating `ChatCompletionRequest` literal and the
/// [`PipelineContext`] builder.
///
/// Off by default so a lifted contract leaf ships no test support; the crates
/// that own middleware tests enable it through a dev-dependency
/// (`test-fixtures`). The one-literal shape means adding a field to
/// `ChatCompletionRequest` touches exactly one place instead of one per
/// middleware test module.
#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixtures {
    use super::{ChatCompletionRequest, ChatMessage, PipelineContext};

    /// A request with a single `user: "hi"` message and every optional field
    /// `None`.
    pub fn minimal_request() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: None,
            messages: vec![ChatMessage::new("user", "hi")],
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
            response_format: None,
            chat_template_kwargs: None,
            think_budget: None,
            tool_profile: None,
            sampling_mode: None,
            assistant_prefix: None,
            cmd_prefix: None,
            url_allowlist: None,
            evidence_id_allowlist: None,
            lark_grammar: None,
            stable_prefix_len: None,
        }
    }

    /// [`minimal_request`] with the message list replaced by the given
    /// `(role, content)` pairs.
    pub fn request_with_messages(messages: &[(&str, &str)]) -> ChatCompletionRequest {
        ChatCompletionRequest {
            messages: messages
                .iter()
                .map(|(role, content)| ChatMessage::new(*role, *content))
                .collect(),
            ..minimal_request()
        }
    }

    /// A `PipelineContext` for the given feature id + repo root, with neutral
    /// defaults everywhere else.
    pub fn ctx_with(feature_id: Option<&str>, repo: std::path::PathBuf) -> PipelineContext {
        PipelineContext {
            pipeline_name: "test".into(),
            model_id: "qwen-27b-coder".into(),
            context_config: Default::default(),
            feature_id: feature_id.map(String::from),
            session_id: Some("sess-1".into()),
            repo_root: repo,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_db_lives_under_dot_sovereign() {
        let root = std::path::Path::new("/repo");
        assert_eq!(
            notes_db_path(root),
            std::path::PathBuf::from("/repo/.sovereign/notes.db")
        );
    }

    #[test]
    fn features_db_lives_under_dot_sovereign() {
        let root = std::path::Path::new("/repo");
        assert_eq!(
            features_db_path(root),
            std::path::PathBuf::from("/repo/.sovereign/features.db")
        );
    }

    mod prepend {
        use super::super::{fixtures, prepend_to_system};

        #[test]
        fn inserts_system_message_at_front_when_absent() {
            let mut req = fixtures::minimal_request();
            prepend_to_system(&mut req, "PREAMBLE");
            assert_eq!(req.messages.len(), 2);
            assert_eq!(req.messages[0].role, "system");
            assert_eq!(req.messages[0].content, "PREAMBLE");
            assert_eq!(req.messages[1].role, "user");
        }

        #[test]
        fn prepends_to_existing_system_message_with_newline_join() {
            let mut req = fixtures::request_with_messages(&[
                ("system", "Original directive."),
                ("user", "hi"),
            ]);
            prepend_to_system(&mut req, "PREAMBLE");
            // No new message; exact byte contract on the join.
            assert_eq!(req.messages.len(), 2);
            assert_eq!(req.messages[0].content, "PREAMBLE\nOriginal directive.");
        }

        #[test]
        fn only_the_first_system_message_is_touched() {
            let mut req = fixtures::request_with_messages(&[
                ("system", "first"),
                ("system", "second"),
                ("user", "hi"),
            ]);
            prepend_to_system(&mut req, "P");
            assert_eq!(req.messages[0].content, "P\nfirst");
            assert_eq!(req.messages[1].content, "second");
            assert_eq!(req.messages[2].content, "hi");
        }

        #[test]
        fn repeated_prepends_stack_newest_first() {
            // Pipeline order semantics: a middleware that runs later
            // ends up ABOVE earlier content only if it prepends —
            // two prepends stack newest-first.
            let mut req = fixtures::minimal_request();
            prepend_to_system(&mut req, "first");
            prepend_to_system(&mut req, "second");
            assert_eq!(req.messages[0].content, "second\nfirst");
        }
    }
}
