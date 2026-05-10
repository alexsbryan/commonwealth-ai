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

use corpus_engine::{NoteScope, NoteStore};

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
                    "feature_id": { "type": ["string", "null"] }
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
            .ok_or_else(|| Error::InvalidInput("write_note requires non-empty 'content'".to_string()))?;
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

        let id = self
            .store
            .write_note_with_relation(
                kind,
                content,
                symbols,
                files,
                session_id,
                scope,
                feature_id,
                related_entity,
            )
            .await
            .map_err(|e| Error::Tool {
                tool_id: "note".to_string(),
                message: e.to_string(),
            })?;

        // The created_at timestamp can be retrieved via read_notes if needed.
        Ok(StepOutput::Json(json!({
            "id": id,
            "kind": kind,
            "scope": scope.as_str(),
            "feature_id": feature_id,
            "related_entity": related_entity,
        })))
    }
}
