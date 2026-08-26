// SPDX-License-Identifier: AGPL-3.0-or-later
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

use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::types::*;

use corpus_engine_notes::{NoteScope, NoteStore};
use sovereign_core::tool_manifest::DeclaredTool;

pub struct WriteRedteamFindingTool {
    store: Arc<NoteStore>,
}

impl WriteRedteamFindingTool {
    pub fn new(store: Arc<NoteStore>) -> Self {
        Self { store }
    }
}

impl WriteRedteamFindingTool {
    /// Bind this tool's state to its `write_redteam_finding` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        let run_state = Arc::clone(&state);
        sovereign_core::tool_manifest::declared("write_redteam_finding", move |params, ctx| {
            let state = Arc::clone(&run_state);
            async move { state.run(&params, &ctx).await }
        })
        .with_validate({
            let state = Arc::clone(&state);
            Arc::new(move |p: &serde_json::Value| state.validate_extra(p))
        })
    }

    /// The executable half of `write_redteam_finding`.
    async fn run(
        &self,
        params: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<StepOutput> {
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

    fn validate_extra(&self, params: &serde_json::Value) -> Result<()> {

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
}
