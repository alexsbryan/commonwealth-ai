//! `write_redteam_finding` — the ONE write tool the red-team driver
//! is meant to use.
//!
//! Persisted as a `redteam_finding`-kind note on the feature scope.
//! The epistemic report renderer groups findings by the `confidence`
//! field (high → medium → low) so Marcus sees them in order during
//! PR review.
//!
//! Why a dedicated tool rather than asking the red-team driver to use
//! `write_note(kind='redteam_finding', …)`?
//!
//! 1. The structured schema ( `invariant`, `status`, `evidence`,
//!    `confidence`) matches §5.3 of the ATOS design doc without the
//!    agent having to remember the field names inside a freeform
//!    `content` blob.
//! 2. Tool-filtering enforcement at the MCP router (deferred to M4)
//!    will key on tool name, not note kind. Keeping the red-team
//!    write on its own name lets us narrow the surface later without
//!    breaking normal-mode sessions that legitimately use `write_note`.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use corpus_engine_notes::{NoteScope, NoteStore};

pub struct WriteRedteamFindingTool {
    store: Arc<NoteStore>,
}

impl WriteRedteamFindingTool {
    pub fn new(store: Arc<NoteStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for WriteRedteamFindingTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "write_redteam_finding".to_string(),
            name: "Write Red-Team Finding".to_string(),
            description: "Record a red-team review finding. Structured around §5.3 of the ATOS \
                 design doc — {invariant, status, evidence, confidence}. The call must \
                 be made from a `mode=redteam` driver session against a provisioned \
                 feature_id; otherwise normal-mode `write_note` is the right tool."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "feature_id": {
                        "type": "string",
                        "description": "Feature this finding applies to. Usually \
                                        $SOVEREIGN_FEATURE_ID from the driver env."
                    },
                    "invariant": {
                        "type": "string",
                        "description": "The spec invariant being checked. Quote it from \
                                        the charter verbatim."
                    },
                    "status": {
                        "type": "string",
                        "enum": ["violated", "potentially_violated", "not_found"],
                        "description": "Whether the invariant is violated, possibly \
                                        violated, or no evidence either way was found."
                    },
                    "evidence": {
                        "type": "string",
                        "description": "The specific symbol, file, or line that led to the \
                                        finding. Empty string for `status=not_found`."
                    },
                    "confidence": {
                        "type": "string",
                        "enum": ["high", "medium", "low"],
                        "description": "Reviewer confidence in the status verdict."
                    },
                    "files": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Optional: file paths the evidence points at. \
                                        Drives report grouping."
                    }
                },
                "required": ["feature_id", "invariant", "status", "confidence"]
            }),
            examples: vec![ToolExample {
                situation: "You found that ZoteroLibrary's factory method relies on a \
                            default for `scope=Local` when the charter said the field \
                            must be set explicitly. Medium confidence — the default \
                            currently happens to be correct but a refactor could \
                            silently flip it."
                    .into(),
                call: serde_json::json!({
                    "feature_id": "zotero-acquirer",
                    "invariant": "LocalCorpusConfig::zotero_library MUST set scope=Local explicitly, not via default.",
                    "status": "violated",
                    "evidence": "corpus-engine/src/acquirers/zotero.rs::factory() line 42 omits the field.",
                    "confidence": "medium",
                    "files": ["corpus-engine/src/acquirers/zotero.rs"]
                }),
            }],
            effect: Effect::Write,
            idempotency: Idempotency::NonIdempotent,
            latency: Latency::Instant,
            scope: Scope::Persistent,
            output_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "id":         { "type": "string" },
                    "feature_id": { "type": "string" },
                    "invariant":  { "type": "string" },
                    "status":     { "type": "string" },
                    "confidence": { "type": "string" }
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        for key in ["feature_id", "invariant", "status", "confidence"] {
            params
                .get(key)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    Error::InvalidInput(format!("write_redteam_finding requires non-empty '{key}'"))
                })?;
        }
        let status = params.get("status").and_then(|v| v.as_str()).unwrap_or("");
        if !matches!(status, "violated" | "potentially_violated" | "not_found") {
            return Err(Error::InvalidInput(format!(
                "status must be violated|potentially_violated|not_found, got '{status}'"
            )));
        }
        let confidence = params
            .get("confidence")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !matches!(confidence, "high" | "medium" | "low") {
            return Err(Error::InvalidInput(format!(
                "confidence must be high|medium|low, got '{confidence}'"
            )));
        }
        Ok(())
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let feature_id = params
            .get("feature_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let invariant = params
            .get("invariant")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let status = params.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let evidence = params
            .get("evidence")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let confidence = params
            .get("confidence")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let files: Vec<String> = params
            .get("files")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        // Serialize the structured payload into the note content so
        // the report renderer (M3.6) can parse it back without
        // widening the notes schema. We keep the human-readable
        // invariant first so `read_notes(query=...)` still hits.
        let content = serde_json::json!({
            "invariant": invariant,
            "status": status,
            "evidence": evidence,
            "confidence": confidence,
        })
        .to_string();

        let id = self
            .store
            .write_note_scoped(
                "redteam_finding",
                &content,
                vec![],
                files,
                "redteam",
                NoteScope::Feature,
                Some(feature_id),
            )
            .await
            .map_err(|e| Error::Tool {
                tool_id: "write_redteam_finding".to_string(),
                message: e.to_string(),
            })?;

        Ok(StepOutput::Json(json!({
            "id": id,
            "kind": "redteam_finding",
            "feature_id": feature_id,
            "status": status,
            "confidence": confidence,
        })))
    }
}
