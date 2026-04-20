//! `ApprovalGate` — the M4 replacement for the old
//! `APPROVED:<feature-id>` magic-token convention.
//!
//! Logic, in order:
//!
//! 1. Resolve an approval for `ctx.feature_id` via
//!    [`sovereign_atos::approval::find_approval`]. Git path first,
//!    MeshStore fallback, `None` if neither matches. Result cached
//!    on the session state so the next request skips the git walk.
//!
//! 2. Enforce the write-intent gate. If the request contains a
//!    tool call targeting a write-intent tool name (`write_note`,
//!    `promote_note`, `provision_feature`, etc.) AND no approval
//!    exists, short-circuit with `ApprovalRequired`.
//!
//! 3. Detect drift. Hash `.sovereign/features/<id>/spec.md` on
//!    disk and compare to `approval.spec_content_hash`. Mismatch
//!    writes a `deviation`-kind note and sets
//!    `session.pending_deviation_ack = true` (ContextInjector
//!    surfaces it on next turn). Does NOT fail the request.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;

use corpus_engine::{NoteScope, NoteStore};
use sovereign_atos::approval::{
    current_spec_hash, detect_drift, find_approval, FeatureApproval,
};

use super::{Middleware, MiddlewareError, MiddlewareSession, PipelineContext};
use crate::openai_types::ChatCompletionRequest;

/// Tool names that trigger the approval gate. When any of these
/// appears in the request's `tool_choice` or `tools` array AND the
/// feature is unapproved, the request is rejected pre-inference.
const WRITE_INTENT_TOOLS: &[&str] = &[
    "write_note",
    "promote_note",
    "provision_feature",
    "archive_feature",
    "record_atos_event",
    "write_redteam_finding",
];

pub struct ApprovalGate {
    /// Optional: when set, the gate reads Commonwealth-native
    /// approvals from MeshStore as a fallback to the git path.
    mesh: Option<Arc<commonwealth_state::MeshStore>>,
}

impl ApprovalGate {
    pub fn new() -> Self {
        Self { mesh: None }
    }

    pub fn with_mesh(mut self, mesh: Arc<commonwealth_state::MeshStore>) -> Self {
        self.mesh = Some(mesh);
        self
    }
}

impl Default for ApprovalGate {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for ApprovalGate {
    fn id(&self) -> &'static str {
        "approval_gate"
    }

    async fn process(
        &self,
        request: &mut ChatCompletionRequest,
        session: &mut MiddlewareSession,
        ctx: &PipelineContext,
    ) -> Result<(), MiddlewareError> {
        let Some(feature_id) = ctx.feature_id.clone() else {
            // No X-Feature-Id header — pipeline was invoked outside
            // the ATOS-sessioned flow. Skip gating; downstream
            // middleware still run so clients can call read tools.
            tracing::debug!("approval_gate: no feature_id in context; skipping");
            return Ok(());
        };

        // Resolve approval. If the session already validated once,
        // trust the cached verdict — avoids a git walk per request.
        let approval = if session.approval_validated
            && session.spec_content_hash.is_some()
        {
            Some(FeatureApproval {
                feature_id: feature_id.clone(),
                spec_path: format!(".sovereign/features/{feature_id}/spec.md"),
                spec_content_hash: session.spec_content_hash.clone().unwrap_or_default(),
                approved_by: String::new(),
                approved_at: 0,
                source: sovereign_atos::approval::ApprovalSource::Git,
                witness: String::new(),
            })
        } else {
            find_approval(&ctx.repo_root, &feature_id, self.mesh.as_deref())
        };

        match approval {
            Some(appr) => {
                session.approval_validated = true;
                session.spec_content_hash = Some(appr.spec_content_hash.clone());

                // Drift detection. A mismatch writes a deviation
                // note but does NOT reject — the agent's next turn
                // acknowledges or reverts.
                if detect_drift(&appr, &ctx.repo_root) {
                    self.handle_drift(&feature_id, &appr, &ctx.repo_root, session)
                        .await;
                }
            }
            None => {
                // No approval. Block writes; allow reads through so
                // the agent can at least call read_notes / read_note_digest
                // to acknowledge its own unapproved state.
                if request_has_write_intent(request) {
                    tracing::warn!(
                        feature = %feature_id,
                        "approval_gate: blocking unapproved write-intent request"
                    );
                    return Err(MiddlewareError::ApprovalRequired {
                        feature_id: feature_id.clone(),
                        hint: format!(
                            "Commit `.sovereign/features/{feature_id}/spec.md` with a \
                             reviewer identity, or run `sovereign atos feature approve \
                             {feature_id}` for the mesh-native fallback."
                        ),
                    });
                }
            }
        }

        Ok(())
    }
}

impl ApprovalGate {
    async fn handle_drift(
        &self,
        feature_id: &str,
        approval: &FeatureApproval,
        repo_root: &Path,
        session: &mut MiddlewareSession,
    ) {
        // Open the notes DB adjacent to the repo. If the open fails
        // we log and soldier on — the drift flag still flips so the
        // operator sees it via ContextInjector, just without the
        // durable note record.
        let notes_db = notes_db_path(repo_root);
        let Ok(store) = NoteStore::open(&notes_db) else {
            tracing::warn!(
                path = %notes_db.display(),
                "approval_gate: could not open notes DB for drift record"
            );
            session.pending_deviation_ack = true;
            return;
        };

        let current = current_spec_hash(repo_root, feature_id).unwrap_or_default();
        let content = format!(
            "Spec content hash changed since approval.\n\n\
             Approved: {} (by {})\nCurrent:  {}\n\n\
             Acknowledge with an intentional deviation note explaining the change, \
             or revert `.sovereign/features/{feature_id}/spec.md` to the approved version.",
            short_hash(&approval.spec_content_hash),
            approval.approved_by,
            short_hash(&current),
        );
        match store
            .write_note_scoped(
                "deviation",
                &content,
                vec![],
                vec![format!(".sovereign/features/{feature_id}/spec.md")],
                "approval_gate",
                NoteScope::Feature,
                Some(feature_id),
            )
            .await
        {
            Ok(id) => {
                tracing::info!(
                    feature = %feature_id,
                    note_id = %id,
                    "approval_gate: wrote deviation note"
                );
                session.pending_deviation_ack = true;
                session.deviation_note_id = Some(id);
            }
            Err(e) => {
                tracing::warn!(
                    feature = %feature_id,
                    err = %e,
                    "approval_gate: failed to write deviation note"
                );
                session.pending_deviation_ack = true;
            }
        }
    }
}

// ─── helpers ─────────────────────────────────────────────────────────────────

/// Inspect a chat completion request for signs of a write-intent
/// tool call. Looks at the advertised tools array + the final
/// message's tool_calls.
fn request_has_write_intent(request: &ChatCompletionRequest) -> bool {
    if let Some(tools) = request.tools.as_ref() {
        for tool in tools {
            if WRITE_INTENT_TOOLS.contains(&tool.function.name.as_str()) {
                return true;
            }
        }
    }
    for msg in &request.messages {
        if let Some(calls) = msg.tool_calls.as_ref() {
            for call in calls {
                if WRITE_INTENT_TOOLS.contains(&call.function.name.as_str()) {
                    return true;
                }
            }
        }
    }
    false
}

fn notes_db_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".sovereign").join("notes.db")
}

fn short_hash(h: &str) -> String {
    h.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openai_types::{
        ChatCompletionRequest, ChatMessage, FunctionCall, ToolCall, ToolDefinition, ToolFunction,
    };

    fn minimal_request() -> ChatCompletionRequest {
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
        }
    }

    fn ctx_with(feature_id: Option<&str>, repo: PathBuf) -> PipelineContext {
        PipelineContext {
            pipeline_name: "test".into(),
            model_id: "qwen-27b-coder".into(),
            context_config: Default::default(),
            feature_id: feature_id.map(String::from),
            session_id: Some("sess-1".into()),
            repo_root: repo,
        }
    }

    #[tokio::test]
    async fn no_feature_id_is_noop() {
        let gate = ApprovalGate::new();
        let mut req = minimal_request();
        let mut session = MiddlewareSession::default();
        let ctx = ctx_with(None, std::env::temp_dir());
        gate.process(&mut req, &mut session, &ctx).await.unwrap();
        assert!(!session.approval_validated);
    }

    #[tokio::test]
    async fn unapproved_write_request_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let gate = ApprovalGate::new();
        let mut req = minimal_request();
        // Include a write-intent tool in the advertised list.
        req.tools = Some(vec![ToolDefinition {
            kind: "function".into(),
            function: ToolFunction {
                name: "write_note".into(),
                description: None,
                parameters: serde_json::json!({}),
            },
        }]);
        let mut session = MiddlewareSession::default();
        let ctx = ctx_with(Some("never-approved"), tmp.path().to_path_buf());
        let err = gate.process(&mut req, &mut session, &ctx).await.unwrap_err();
        assert!(matches!(err, MiddlewareError::ApprovalRequired { .. }));
    }

    #[tokio::test]
    async fn unapproved_read_only_request_passes() {
        let tmp = tempfile::tempdir().unwrap();
        let gate = ApprovalGate::new();
        let req_result = {
            let mut req = minimal_request();
            req.tools = Some(vec![ToolDefinition {
                kind: "function".into(),
                function: ToolFunction {
                    name: "read_notes".into(), // read tool
                    description: None,
                    parameters: serde_json::json!({}),
                },
            }]);
            let mut session = MiddlewareSession::default();
            let ctx = ctx_with(Some("never-approved"), tmp.path().to_path_buf());
            gate.process(&mut req, &mut session, &ctx).await
        };
        assert!(req_result.is_ok());
    }

    #[tokio::test]
    async fn write_intent_in_prior_tool_call_also_blocked() {
        let tmp = tempfile::tempdir().unwrap();
        let gate = ApprovalGate::new();
        let mut req = minimal_request();
        // Prior assistant message with a write tool call — opencode
        // replays this when resuming a conversation.
        req.messages.push(ChatMessage {
            role: "assistant".into(),
            content: String::new(),
            tool_call_id: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_1".into(),
                kind: "function".into(),
                function: FunctionCall {
                    name: "write_note".into(),
                    arguments: "{}".into(),
                },
            }]),
        });
        let mut session = MiddlewareSession::default();
        let ctx = ctx_with(Some("unapproved-replay"), tmp.path().to_path_buf());
        let err = gate.process(&mut req, &mut session, &ctx).await.unwrap_err();
        assert!(matches!(err, MiddlewareError::ApprovalRequired { .. }));
    }
}
