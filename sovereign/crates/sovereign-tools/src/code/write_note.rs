// SPDX-License-Identifier: AGPL-3.0-or-later
//! `write_note` — persist an agent working note.
//!
//! Notes survive across sessions and can be retrieved by `read_notes`.
//! Use `kind = "todo"` for tasks that span multiple sessions — they appear
//! in the startup summary when the MCP server starts.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use corpus_engine_notes::{NoteScope, NoteSource, NoteStore};

/// Kinds the tool admits in `validate()`. Single source of truth for
/// the schema-`enum` field, the validator, and any future test that
/// wants to exercise round-tripping. New kinds land here in the same
/// PR as the corpus-engine schema migration that adds them — drift
/// between the two is the bug class this constant prevents (see
/// ARCH_PRINCIPLES §2.1).
pub(crate) const WRITE_NOTE_KINDS: &[&str] = &[
    "decision",
    "attempt",
    "invariant",
    "todo",
    "commitment",
    "follow_up",
    "goal",
];

pub struct WriteNoteTool {
    store: Arc<NoteStore>,
}

impl WriteNoteTool {
    pub fn new(store: Arc<NoteStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for WriteNoteTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "note".to_string(),
            name: "Write Note".to_string(),
            description: "Persist a working note that survives across sessions. \
                          Use to record decisions, failed attempts, known invariants, \
                          and open tasks. Notes tagged with symbols or files are \
                          retrieved by read_notes when you revisit that code. \
                          Use kind='todo' for cross-session tasks — they appear \
                          in the server startup summary."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "kind": {
                        "type": "string",
                        "enum": [
                            "decision", "attempt", "invariant", "todo",
                            "commitment", "follow_up", "goal"
                        ],
                        "description": "decision=architectural choice, attempt=failed approach, \
                                        invariant=must-not-break constraint, todo=open task, \
                                        commitment=promise made to a named person/org \
                                        (relational), follow_up=temporal marker tied to a \
                                        named entity (relational), goal=declared desired \
                                        outcome with success criterion (strategic)"
                    },
                    "related_entity": {
                        "type": "string",
                        "description": "Optional free-text name of the Person, Organization, \
                                        or Initiative this note is anchored to. Surfaces in \
                                        the relational/strategic digest when the entity is \
                                        active. Required-shape (but not enforced) for \
                                        kind=commitment | follow_up | goal."
                    },
                    "content": {
                        "type": "string",
                        "description": "The note content. Be specific enough to be useful in a future session."
                    },
                    "symbols": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Symbol names this note relates to (for filtered retrieval)"
                    },
                    "files": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "File paths this note relates to (for filtered retrieval)"
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Optional session identifier (defaults to 'mcp')"
                    },
                    "scope": {
                        "type": "string",
                        "enum": ["global", "feature", "session"],
                        "description": "ATOS scope. Defaults to 'global'. Pass 'feature' with a \
                                        feature_id to tag this note to an ATOS feature."
                    },
                    "feature_id": {
                        "type": "string",
                        "description": "Required when scope='feature'. The id returned by \
                                        provision_feature (same value as $SOVEREIGN_FEATURE_ID \
                                        in the ATOS driver env)."
                    },
                    "supersedes": {
                        "type": "string",
                        "description": "Optional id of a prior note this one REPLACES. The \
                                        superseded note is auto-retired (hidden from read_notes, \
                                        with a 'superseded by <id>' reason) while its row is kept \
                                        for the gossip-propagated supersedes chain. Use when a new \
                                        decision/invariant reverses or updates an older one, so the \
                                        two don't coexist and contradict."
                    }
                },
                "required": ["kind", "content"]
            }),
            examples: vec![
                ToolExample {
                    situation: "You've just discovered a non-obvious constraint — something that would take another session hours to re-derive. Record it now as an invariant so it surfaces the next time anyone touches this code.".into(),
                    call: serde_json::json!({
                        "kind": "invariant",
                        "content": "EmbedSlot must use n_gpu_layers=0, offload_kqv=false, and op_offload=false. The GGML Metal scheduler crashes on embedding tensor graphs (GGML_ASSERT buf_src) without all three.",
                        "symbols": ["EmbedSlot", "EmbeddedLlamaCpp"],
                        "files": ["crates/sovereign-inference/src/embedded.rs"]
                    }),
                },
                ToolExample {
                    situation: "You chose one implementation approach over others. Record the decision and reasoning so the next session doesn't relitigate it.".into(),
                    call: serde_json::json!({
                        "kind": "decision",
                        "content": "Used Mutex<HashMap> for tool call counters in ToolRegistry rather than DashMap — avoids adding a dependency to sovereign-core for a non-hot path.",
                        "symbols": ["ToolRegistry"]
                    }),
                },
                ToolExample {
                    situation: "You tried an approach that failed in a non-obvious way. Record it so the next session skips straight to what works.".into(),
                    call: serde_json::json!({
                        "kind": "attempt",
                        "content": "Tried with_split_mode(LlamaSplitMode::None) to force CPU-only for embedding — has no effect on compute graph routing. op_offload=false is required.",
                        "symbols": ["EmbedSlot"]
                    }),
                },
            ],
            effect: Effect::Write,
            idempotency: Idempotency::NonIdempotent,
            latency: Latency::Instant,
            scope: Scope::Persistent,
            output_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "id":         { "type": "string" },
                    "kind":       { "type": "string" },
                    "scope":      { "type": "string" },
                    "feature_id": { "type": ["string", "null"] },
                    "supersedes": { "type": ["string", "null"] },
                    "retired":    { "type": ["string", "null"] }
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
            .ok_or_else(|| Error::InvalidInput("write_note requires 'kind'".to_string()))?;
        if !WRITE_NOTE_KINDS.contains(&kind) {
            return Err(Error::InvalidInput(format!(
                "invalid kind '{kind}': must be one of {}",
                WRITE_NOTE_KINDS.join(", ")
            )));
        }
        params
            .get("content")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                Error::InvalidInput("write_note requires non-empty 'content'".to_string())
            })?;
        Ok(())
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let kind = params
            .get("kind")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("missing 'kind'".to_string()))?;
        let content = params
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("missing 'content'".to_string()))?;

        let symbols: Vec<String> = params
            .get("symbols")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let files: Vec<String> = params
            .get("files")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let session_id = params
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or("mcp");

        let scope = params
            .get("scope")
            .and_then(|v| v.as_str())
            .and_then(NoteScope::parse)
            .unwrap_or(NoteScope::Global);
        let feature_id = params
            .get("feature_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let related_entity = params
            .get("related_entity")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let supersedes = params
            .get("supersedes")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());

        let id = self
            .store
            .write_note_with_source(
                kind,
                content,
                symbols,
                files,
                session_id,
                scope,
                feature_id,
                related_entity,
                NoteSource::Agent,
                supersedes,
            )
            .await
            .map_err(|e| Error::Tool {
                tool_id: "note".to_string(),
                message: e.to_string(),
            })?;

        // Auto-retire the superseded note. Declaring B supersedes A means A is
        // now stale, so hide it from read_notes (set retired_at) while keeping
        // the row for the gossip-propagated supersedes chain. Without this the
        // link is recorded but both notes still surface and contradict — the
        // exact accretion this closes. Non-fatal: if the old id is missing or
        // already retired, the supersedes link on the new note still stands.
        let mut retired: Option<&str> = None;
        if let Some(old_id) = supersedes {
            match self
                .store
                .retire_by_id(old_id, &format!("superseded by note {id}"))
                .await
            {
                Ok(true) => retired = Some(old_id),
                Ok(false) => {} // not found or already retired — link still recorded
                Err(e) => tracing::warn!(
                    target: "notes",
                    old_id,
                    error = %e,
                    "supersede: retire of superseded note failed"
                ),
            }
        }

        // The created_at timestamp can be retrieved via read_notes if needed.
        Ok(StepOutput::Json(json!({
            "id": id,
            "kind": kind,
            "scope": scope.as_str(),
            "feature_id": feature_id,
            "related_entity": related_entity,
            "supersedes": supersedes,
            "retired": retired,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_core::types::ToolContext;

    fn ctx() -> ToolContext {
        ToolContext {
            conversation_id: "write-note-test".into(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
        }
    }

    fn id_of(out: &StepOutput) -> String {
        match out {
            StepOutput::Json(v) => v["id"].as_str().unwrap().to_string(),
            other => panic!("expected Json output, got {other:?}"),
        }
    }

    /// Declaring B supersedes A must (1) record the supersedes link on B and
    /// (2) auto-retire A so it stops surfacing in read_notes — the whole point
    /// of exposing supersedes over MCP. Without the auto-retire, both notes
    /// would coexist and contradict.
    #[tokio::test]
    async fn supersedes_records_link_and_auto_retires_the_old_note() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(NoteStore::open(&tmp.path().join("notes.db")).unwrap());
        let tool = WriteNoteTool::new(Arc::clone(&store));

        let a_id = id_of(
            &tool
                .execute(
                    &json!({"kind": "decision", "content": "A: original decision"}),
                    &ctx(),
                )
                .await
                .unwrap(),
        );
        // A is live before it is superseded.
        assert!(store
            .read_note_by_id(&a_id)
            .await
            .unwrap()
            .unwrap()
            .retired_at
            .is_none());

        let out_b = tool
            .execute(
                &json!({"kind": "decision", "content": "B: replaces A", "supersedes": a_id}),
                &ctx(),
            )
            .await
            .unwrap();
        let b_id = id_of(&out_b);
        let retired = match &out_b {
            StepOutput::Json(v) => v["retired"].as_str().map(String::from),
            _ => None,
        };

        // Response reports A retired; A is hidden; B records the link.
        assert_eq!(retired.as_deref(), Some(a_id.as_str()));
        let a_row = store.read_note_by_id(&a_id).await.unwrap().unwrap();
        assert!(
            a_row.retired_at.is_some(),
            "superseded note must be retired"
        );
        let b_row = store.read_note_by_id(&b_id).await.unwrap().unwrap();
        assert_eq!(b_row.supersedes.as_deref(), Some(a_id.as_str()));
    }

    /// A supersedes pointing at a nonexistent id must not fail the write — the
    /// new note is still created and its link recorded; `retired` is null.
    #[tokio::test]
    async fn supersedes_missing_target_still_writes_the_new_note() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(NoteStore::open(&tmp.path().join("notes.db")).unwrap());
        let tool = WriteNoteTool::new(Arc::clone(&store));

        let out = tool
            .execute(
                &json!({"kind": "invariant", "content": "X", "supersedes": "does-not-exist"}),
                &ctx(),
            )
            .await
            .unwrap();
        match out {
            StepOutput::Json(v) => {
                assert!(v["id"].as_str().is_some(), "note still created");
                assert!(
                    v["retired"].is_null(),
                    "nothing retired for a missing target"
                );
            }
            other => panic!("expected Json, got {other:?}"),
        }
    }
}
