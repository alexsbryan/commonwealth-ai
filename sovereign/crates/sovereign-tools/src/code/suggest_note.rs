// SPDX-License-Identifier: AGPL-3.0-or-later
//! `suggest_note` — model-emitted relational/strategic note suggestion
//! gated through `ApprovalChannel`.
//!
//! When the user says "I'll send Sarah the pricing by Friday" or
//! "let's get churn under 5% by Q3", the model can detect the
//! commitment/goal and emit a `suggest_note` tool call. Unlike
//! [`WriteNoteTool`], this tool does **not** write to the
//! `NoteStore` directly. Instead it builds an [`ActionPreview`] and
//! routes through [`ApprovalChannel::request_approval`]. Only on
//! explicit user confirmation does the suggestion become a
//! persisted note.
//!
//! Three signals shape the design:
//!
//! 1. **Inferred state and committed state are different epistemic
//!    classes.** Entity atoms are inferred (the model's read of a
//!    chunk); commitment / follow_up / goal notes are committed (the
//!    user has explicitly confirmed them). This tool is the bridge
//!    between the two — it never silently promotes one to the
//!    other.
//! 2. **Reuse, don't invent, the approval surface.** All three
//!    `ApprovalChannel` impls (CLI, Tauri, Server) already render
//!    `ActionPreview` for tool approvals. A new schema-tagged
//!    description is enough.
//! 3. **No tag parser.** The codebase has no precedent for parsing
//!    `[suggested_note: …]` markers out of model output. Following
//!    the OICP tool-call convention costs less and composes with the
//!    existing streaming + structured-output paths.
//!
//! Frequency / priority enforcement (one suggestion per turn,
//! goal > commitment > follow_up) lives in the runtime's
//! `SuggestionGate` (Phase 6.B follow-up). At the tool level we
//! always honour each call individually; the gate decides which
//! survive.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::{ApprovalChannel, Tool};
use sovereign_core::types::*;

use corpus_engine_notes::{NoteScope, NoteStore};

/// Kinds the suggest_note tool may emit. Strict subset of the
/// NoteStore's v5 kind set — the relational + strategic kinds only.
/// A `decision` or `invariant` is something the agent records
/// directly; only `commitment`, `follow_up`, and `goal` need
/// human-in-the-loop confirmation because they reflect the user's
/// intent.
pub(crate) const SUGGEST_NOTE_KINDS: &[&str] = &["commitment", "follow_up", "goal"];

pub struct SuggestNoteTool {
    store: Arc<NoteStore>,
    approval: Arc<dyn ApprovalChannel>,
}

impl SuggestNoteTool {
    pub fn new(store: Arc<NoteStore>, approval: Arc<dyn ApprovalChannel>) -> Self {
        Self { store, approval }
    }
}

#[async_trait]
impl Tool for SuggestNoteTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "suggest_note".to_string(),
            name: "Suggest Note".to_string(),
            description: "Surface a commitment, follow-up, or goal you detected in the user's \
                          message — pending user confirmation. \
                          \
                          Use when the user said: \
                          • 'I'll send X by Friday' (commitment) \
                          • 'check back with Y in two weeks' (follow-up) \
                          • 'we want under 5% churn by Q3' (goal) \
                          \
                          Do NOT use for: things you decide, things the assistant \
                          plans to do, or topics the user merely thinks about. \
                          \
                          The note is NOT written until the user approves it via \
                          the approval channel. On rejection the tool returns \
                          {dismissed:true} and the suggestion is dropped."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "kind": {
                        "type": "string",
                        "enum": SUGGEST_NOTE_KINDS,
                        "description": "commitment = relational speech act ('I'll do X'). \
                                        follow_up = temporal marker ('check back in 2 weeks'). \
                                        goal = declared desired outcome ('under 5% churn by Q3')."
                    },
                    "content": {
                        "type": "string",
                        "description": "One sentence in the user's voice. Concise enough to \
                                        be the digest entry. Do NOT include the user's name \
                                        — it's already known."
                    },
                    "related_entity": {
                        "type": "string",
                        "description": "Person/Organization name for commitment + follow_up; \
                                        Initiative name for goal. Match the canonical name \
                                        used in conversation. Optional but strongly preferred — \
                                        without it the digest can't surface the note alongside \
                                        the right entity."
                    }
                },
                "required": ["kind", "content"]
            }),
            examples: vec![
                ToolExample {
                    situation: "The user just said: 'I'll send revised pricing to Sarah by Friday.'".into(),
                    call: json!({
                        "kind": "commitment",
                        "content": "Send revised pricing to Sarah Chen by Friday",
                        "related_entity": "Sarah Chen"
                    }),
                },
                ToolExample {
                    situation: "Mid-conversation the user said: 'let's circle back with Meridian in mid-March.'".into(),
                    call: json!({
                        "kind": "follow_up",
                        "content": "Circle back with Meridian in mid-March",
                        "related_entity": "Meridian"
                    }),
                },
                ToolExample {
                    situation: "The user staked a target: 'we need to get churn under 5% by end of Q3.'".into(),
                    call: json!({
                        "kind": "goal",
                        "content": "Reduce churn to under 5% by end of Q3",
                        "related_entity": "churn reduction"
                    }),
                },
            ],
            // Behavioural properties: identical to write_note —
            // suggest_note becomes a write_note on approval, so the
            // executor's retry / approval / planner gates should
            // reason about it the same way.
            effect: Effect::Write,
            idempotency: Idempotency::NonIdempotent,
            latency: Latency::Instant,
            scope: Scope::Persistent,
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "id":              { "type": ["string", "null"] },
                    "kind":            { "type": "string" },
                    "related_entity":  { "type": ["string", "null"] },
                    "dismissed":       { "type": "boolean" }
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        let kind = params
            .get("kind")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("suggest_note requires 'kind'".into()))?;
        if !SUGGEST_NOTE_KINDS.contains(&kind) {
            return Err(Error::InvalidInput(format!(
                "suggest_note kind '{kind}' must be one of {}",
                SUGGEST_NOTE_KINDS.join(", ")
            )));
        }
        params
            .get("content")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                Error::InvalidInput("suggest_note requires non-empty 'content'".into())
            })?;
        Ok(())
    }

    async fn execute(&self, params: &serde_json::Value, ctx: &ToolContext) -> Result<StepOutput> {
        let kind = params
            .get("kind")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("missing 'kind'".into()))?;
        let content = params
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("missing 'content'".into()))?;
        let related_entity = params
            .get("related_entity")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());

        // Construct a fresh Step + ActionPreview to surface to the
        // user. We don't mutate the runtime's task plan — this tool
        // is per-call, not part of the planner's step list. The id
        // is synthetic (0); the approval renderer cares about the
        // preview text, not the step graph position.
        let step = Step {
            id: 0,
            description: format!(
                "Suggested {kind}: {content}{aff}",
                aff = related_entity
                    .map(|e| format!(" (re: {e})"))
                    .unwrap_or_default()
            ),
            kind: StepKind::Tool {
                tool_id: "suggest_note".into(),
                params: params.clone(),
            },
            requires_approval: true,
            inputs: Vec::new(),
            sampling: None,
            evaluation: None,
        };
        let preview = ActionPreview {
            tool_id: "suggest_note".into(),
            description: format!(
                "Note this {} as confirmed?{}\n\n  {}",
                kind,
                related_entity
                    .map(|e| format!(" (entity: {e})"))
                    .unwrap_or_default(),
                content,
            ),
            params: params.clone(),
        };

        let approved = self
            .approval
            .request_approval(&step, &preview)
            .await
            .map_err(|e| Error::Tool {
                tool_id: "suggest_note".into(),
                message: format!("approval channel error: {e}"),
            })?;

        if !approved {
            tracing::info!(
                kind,
                conversation_id = %ctx.conversation_id,
                "suggest_note: user dismissed the suggestion"
            );
            return Ok(StepOutput::Json(json!({
                "dismissed": true,
                "kind": kind,
            })));
        }

        // Approved — persist via the same write_note_with_relation
        // path WriteNoteTool uses. Default scope is global; we don't
        // attach the suggestion to a feature.
        let id = self
            .store
            .write_note_with_relation(
                kind,
                content,
                Vec::new(),
                Vec::new(),
                "suggest_note",
                NoteScope::Global,
                None,
                related_entity,
            )
            .await
            .map_err(|e| Error::Tool {
                tool_id: "suggest_note".into(),
                message: e.to_string(),
            })?;

        tracing::info!(
            kind,
            note_id = %id,
            conversation_id = %ctx.conversation_id,
            "suggest_note: user confirmed; note persisted"
        );

        Ok(StepOutput::Json(json!({
            "id": id,
            "kind": kind,
            "related_entity": related_entity,
            "dismissed": false,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_core::error::Result as CoreResult;
    use sovereign_core::traits::ApprovalChannel;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// Approval stub that records every call and returns a configured
    /// verdict.
    struct StubApproval {
        verdict: bool,
        calls: AtomicUsize,
        last_description: tokio::sync::Mutex<String>,
    }

    impl StubApproval {
        fn new(verdict: bool) -> Arc<Self> {
            Arc::new(Self {
                verdict,
                calls: AtomicUsize::new(0),
                last_description: tokio::sync::Mutex::new(String::new()),
            })
        }
    }

    #[async_trait]
    impl ApprovalChannel for StubApproval {
        async fn request_approval(
            &self,
            _step: &Step,
            preview: &ActionPreview,
        ) -> CoreResult<bool> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.last_description.lock().await = preview.description.clone();
            Ok(self.verdict)
        }
        async fn ask_user(&self, _q: &str) -> CoreResult<String> {
            Ok(String::new())
        }
        fn emit_progress(&self, _step: &Step, _output: &StepOutput) {}
    }

    async fn make_store() -> Arc<NoteStore> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.db");
        // The dir handle drops at end of test; SQLite holds the file
        // by path so the WAL file is also rooted there for the test
        // duration.
        std::mem::forget(dir);
        Arc::new(NoteStore::open(&path).unwrap())
    }

    fn ctx() -> ToolContext {
        ToolContext {
            conversation_id: "test-conv".into(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
        }
    }

    #[tokio::test]
    async fn validate_rejects_unknown_kind() {
        let store = make_store().await;
        let approval = StubApproval::new(true);
        let tool = SuggestNoteTool::new(store, approval);
        let r = tool.validate(&json!({"kind": "decision", "content": "x"}));
        assert!(r.is_err(), "decision is not a valid suggest_note kind");
    }

    #[tokio::test]
    async fn validate_rejects_empty_content() {
        let store = make_store().await;
        let approval = StubApproval::new(true);
        let tool = SuggestNoteTool::new(store, approval);
        let r = tool.validate(&json!({"kind": "commitment", "content": ""}));
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn validate_accepts_each_relational_kind() {
        let store = make_store().await;
        let approval = StubApproval::new(true);
        let tool = SuggestNoteTool::new(store, approval);
        for kind in SUGGEST_NOTE_KINDS {
            let r = tool.validate(&json!({"kind": kind, "content": "x"}));
            assert!(r.is_ok(), "kind {kind} should validate");
        }
    }

    #[tokio::test]
    async fn approved_suggestion_persists_with_related_entity() {
        let store = make_store().await;
        let approval = StubApproval::new(true);
        let tool = SuggestNoteTool::new(store.clone(), approval.clone());

        let result = tool
            .execute(
                &json!({
                    "kind": "commitment",
                    "content": "Send revised pricing to Sarah Chen by Friday",
                    "related_entity": "Sarah Chen",
                }),
                &ctx(),
            )
            .await
            .expect("approved suggestion should not error");

        // Approval was consulted exactly once.
        assert_eq!(approval.calls.load(Ordering::SeqCst), 1);

        // Note was written.
        let notes = store
            .read_notes(None, &[], &[], &["commitment".into()], 10, false)
            .await
            .unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].related_entity.as_deref(), Some("Sarah Chen"));
        assert_eq!(
            notes[0].content,
            "Send revised pricing to Sarah Chen by Friday"
        );

        // Result envelope exposes the new id.
        let StepOutput::Json(v) = result else {
            panic!("expected JSON output");
        };
        assert_eq!(v["dismissed"], json!(false));
        assert!(v["id"].is_string());
    }

    #[tokio::test]
    async fn dismissed_suggestion_writes_no_note() {
        let store = make_store().await;
        let approval = StubApproval::new(false);
        let tool = SuggestNoteTool::new(store.clone(), approval.clone());

        let result = tool
            .execute(
                &json!({
                    "kind": "goal",
                    "content": "Under 5% churn by Q3",
                    "related_entity": "churn reduction",
                }),
                &ctx(),
            )
            .await
            .expect("dismissal is not an error");

        // No note in the store.
        let notes = store
            .read_notes(None, &[], &[], &["goal".into()], 10, false)
            .await
            .unwrap();
        assert!(notes.is_empty());

        let StepOutput::Json(v) = result else {
            panic!("expected JSON output");
        };
        assert_eq!(v["dismissed"], json!(true));
    }

    #[tokio::test]
    async fn approval_preview_includes_kind_entity_and_content() {
        let store = make_store().await;
        let approval = StubApproval::new(true);
        let tool = SuggestNoteTool::new(store, approval.clone());

        tool.execute(
            &json!({
                "kind": "follow_up",
                "content": "Check back with Meridian in two weeks",
                "related_entity": "Meridian",
            }),
            &ctx(),
        )
        .await
        .unwrap();

        let desc = approval.last_description.lock().await.clone();
        assert!(desc.contains("follow_up"), "preview names the kind: {desc}");
        assert!(
            desc.contains("Meridian"),
            "preview names the entity: {desc}"
        );
        assert!(
            desc.contains("Check back"),
            "preview shows the content: {desc}"
        );
    }

    #[tokio::test]
    async fn related_entity_is_optional_for_validate_and_execute() {
        // The schema marks related_entity as optional because not
        // every commitment has a clear entity anchor. The digest
        // simply won't surface such notes in the relational digest,
        // but the note still persists.
        let store = make_store().await;
        let approval = StubApproval::new(true);
        let tool = SuggestNoteTool::new(store.clone(), approval);

        tool.execute(
            &json!({
                "kind": "commitment",
                "content": "Reply to Friday's all-hands recap",
            }),
            &ctx(),
        )
        .await
        .unwrap();

        let notes = store
            .read_notes(None, &[], &[], &["commitment".into()], 10, false)
            .await
            .unwrap();
        assert_eq!(notes.len(), 1);
        assert!(notes[0].related_entity.is_none());
    }

    #[tokio::test]
    async fn approval_channel_error_propagates_as_tool_error() {
        struct FailingApproval;
        #[async_trait]
        impl ApprovalChannel for FailingApproval {
            async fn request_approval(&self, _: &Step, _: &ActionPreview) -> CoreResult<bool> {
                Err(Error::Tool {
                    tool_id: "approval".into(),
                    message: "stub failure".into(),
                })
            }
            async fn ask_user(&self, _: &str) -> CoreResult<String> {
                Ok(String::new())
            }
            fn emit_progress(&self, _: &Step, _: &StepOutput) {}
        }
        let store = make_store().await;
        let approval = Arc::new(FailingApproval);
        let tool = SuggestNoteTool::new(store, approval);
        let err = tool
            .execute(&json!({"kind": "goal", "content": "x"}), &ctx())
            .await
            .unwrap_err();
        match err {
            Error::Tool { tool_id, .. } => assert_eq!(tool_id, "suggest_note"),
            other => panic!("expected Error::Tool, got {other:?}"),
        }
    }

    /// Touch the AtomicBool import so it doesn't drift to dead code if
    /// a future refactor drops the only consumer.
    #[allow(dead_code)]
    fn _phantom_atomic(_b: AtomicBool) {}
}
