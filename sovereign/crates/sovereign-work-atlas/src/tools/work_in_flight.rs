// SPDX-License-Identifier: AGPL-3.0-or-later
//! `work_in_flight` — list live Claims overlapping a scope.
//!
//! Phase 1 emits only `ConfidenceGrade::Declared` (CodeWatcher-driven
//! Observations land in Phase 2). Read filtering excludes the
//! caller's own session by `(node_id, agent_session_token)`.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::{
    Effect, Idempotency, Latency, Permission, Scope, StepOutput, ToolContext, ToolDescriptor,
    ToolExample,
};

use crate::confidence::{observation_grade, ConfidenceGrade};
use crate::store::{ScopeMatch, WorkAtlasStore};

#[derive(Debug)]
pub struct WorkInFlightTool {
    store: Arc<WorkAtlasStore>,
}

impl WorkInFlightTool {
    pub fn new(store: Arc<WorkAtlasStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for WorkInFlightTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "work_in_flight".to_string(),
            name: "Work In Flight".to_string(),
            description: "List live work signals overlapping a scope. Use BEFORE \
                          starting non-trivial work on a function or file to see \
                          whether another agent on the mesh is touching it. \
                          Returns both explicit Claims (Declared grade) and \
                          passive Observations from CodeWatcher edits \
                          (Active ≤5min, Recent ≤30min). Phase 2: Observations \
                          are file-level — `match_mode=file` matches them; \
                          symbol-graph distance arrives in Phase 2b. \
                          Excludes the caller's own session."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "scope": {
                        "type": "string",
                        "description": "A SCIP symbol id, file path, or path prefix."
                    },
                    "match_mode": {
                        "type": "string",
                        "enum": ["symbol", "file"],
                        "default": "symbol",
                        "description": "How to match `scope` against claims. `symbol` = exact symbol id; `file` = path equality or prefix."
                    }
                },
                "required": ["scope"]
            }),
            examples: vec![ToolExample {
                situation:
                    "About to refactor a function — check whether anyone else has claimed it."
                        .into(),
                call: json!({ "scope": "CorpusEngine::ingest" }),
            }],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Persistent,
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "scope":        { "type": "string" },
                    "match_mode":   { "type": "string" },
                    "claims":       { "type": "array", "items": { "type": "object" } },
                    "observations": { "type": "array", "items": { "type": "object" } }
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    /// Surface peer activity into the agent's per-turn preamble so a
    /// Claude session "knows" what's happening on the mesh without
    /// having to remember to query. Called by the context assembler
    /// every turn; must be fast (local SQLite scan only) and must not
    /// mutate state.
    ///
    /// Output is intentionally short — one line, no per-record detail
    /// — and only surfaces *peer* activity (records this node didn't
    /// originate). When nothing salient is happening, returns `None`
    /// so the preamble stays quiet.
    async fn signal(&self) -> Option<String> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let self_node = self.store.node_id();

        let public_claims = self.store.scan_claims(crate::model::Privacy::Public).ok()?;
        let public_observations = self
            .store
            .scan_observations(crate::model::Privacy::Public)
            .ok()?;

        // Resolve owning sessions so we can filter to peers only.
        let sessions = self.store.scan_sessions().ok()?;
        let session_node = |sid: uuid::Uuid| -> Option<commonwealth_core::ids::NodeId> {
            sessions
                .iter()
                .find(|s| s.session_id == sid)
                .map(|s| s.node_id)
        };

        let peer_claims = public_claims
            .iter()
            .filter(|c| c.ttl_expires_at >= now)
            .filter(|c| session_node(c.session_id).is_some_and(|n| n != self_node))
            .count();
        let peer_active = public_observations
            .iter()
            .filter(|o| {
                matches!(
                    crate::confidence::observation_grade(now, o.last_observed_at, o.source),
                    Some(crate::confidence::ConfidenceGrade::Active)
                )
            })
            .filter(|o| session_node(o.session_id).is_some_and(|n| n != self_node))
            .count();
        let peer_recent = public_observations
            .iter()
            .filter(|o| {
                matches!(
                    crate::confidence::observation_grade(now, o.last_observed_at, o.source),
                    Some(crate::confidence::ConfidenceGrade::Recent)
                )
            })
            .filter(|o| session_node(o.session_id).is_some_and(|n| n != self_node))
            .count();

        if peer_claims == 0 && peer_active == 0 && peer_recent == 0 {
            return None;
        }

        let mut parts = Vec::with_capacity(3);
        if peer_active > 0 {
            parts.push(format!("{peer_active} actively edited by peer"));
        }
        if peer_recent > 0 {
            parts.push(format!("{peer_recent} recently edited by peer"));
        }
        if peer_claims > 0 {
            parts.push(format!("{peer_claims} live peer claim(s)"));
        }
        Some(format!(
            "work atlas: {} — query `work_in_flight(scope, match_mode)` before non-trivial edits in this area.",
            parts.join(", ")
        ))
    }

    async fn execute(&self, params: &Value, ctx: &ToolContext) -> Result<StepOutput> {
        let scope = params
            .get("scope")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("work_in_flight requires 'scope'".into()))?;
        let match_mode_str = params
            .get("match_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("symbol");
        let match_mode = match match_mode_str {
            "symbol" => ScopeMatch::Symbol,
            "file" => ScopeMatch::File,
            other => {
                return Err(Error::InvalidInput(format!(
                    "invalid match_mode '{other}' — use 'symbol' or 'file'"
                )))
            }
        };

        let now = now_secs();
        let claims = self
            .store
            .list_claims_for_scope(scope, match_mode)
            .map_err(|e| Error::Tool {
                tool_id: "work_in_flight".into(),
                message: e.to_string(),
            })?;
        let observations = self
            .store
            .list_observations_for_scope(scope, match_mode)
            .map_err(|e| Error::Tool {
                tool_id: "work_in_flight".into(),
                message: e.to_string(),
            })?;

        let caller_token = ctx.agent_session_token.as_deref();
        let caller_node = self.store.node_id();
        let self_session_ids: std::collections::HashSet<uuid::Uuid> = self
            .store
            .scan_sessions()
            .map_err(|e| Error::Tool {
                tool_id: "work_in_flight".into(),
                message: e.to_string(),
            })?
            .into_iter()
            .filter(|s| {
                s.node_id == caller_node && s.agent_session_token.as_deref() == caller_token
            })
            .map(|s| s.session_id)
            .collect();

        let mut filtered_claims: Vec<Value> = Vec::with_capacity(claims.len());
        for c in claims {
            if c.ttl_expires_at < now {
                continue;
            }
            if self_session_ids.contains(&c.session_id) {
                continue;
            }
            let session_node = self
                .store
                .get_session(c.session_id)
                .ok()
                .flatten()
                .map(|s| s.node_id);
            filtered_claims.push(json!({
                "claim_id":       c.claim_id.to_string(),
                "session_id":     c.session_id.to_string(),
                "intent":         c.intent,
                "declared_at":    c.declared_at,
                "ttl_expires_at": c.ttl_expires_at,
                "node_id":        session_node.map(|n| n.to_string()),
                "confidence":     ConfidenceGrade::Declared.id(),
            }));
        }

        let mut filtered_observations: Vec<Value> = Vec::with_capacity(observations.len());
        for o in observations {
            if self_session_ids.contains(&o.session_id) {
                continue;
            }
            // Grade computed at read time so an Observation gracefully
            // degrades Active → Recent → dropped as time passes.
            let Some(grade) = observation_grade(now, o.last_observed_at, o.source) else {
                continue;
            };
            let session_node = self
                .store
                .get_session(o.session_id)
                .ok()
                .flatten()
                .map(|s| s.node_id);
            filtered_observations.push(json!({
                "session_id":         o.session_id.to_string(),
                "file_path":          o.file_path.to_string_lossy(),
                "first_observed_at":  o.first_observed_at,
                "last_observed_at":   o.last_observed_at,
                "event_count":        o.event_count,
                "node_id":            session_node.map(|n| n.to_string()),
                "confidence":         grade.id(),
            }));
        }

        tracing::debug!(
            scope,
            match_mode = match_mode_str,
            claim_hits = filtered_claims.len(),
            observation_hits = filtered_observations.len(),
            "work_atlas:query"
        );
        Ok(StepOutput::Json(json!({
            "scope": scope,
            "match_mode": match_mode_str,
            "claims": filtered_claims,
            "observations": filtered_observations,
        })))
    }
}

use sovereign_core::time::unix_now_u64 as now_secs;
