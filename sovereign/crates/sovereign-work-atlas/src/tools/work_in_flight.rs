// SPDX-License-Identifier: AGPL-3.0-or-later
//! `work_in_flight` — list live Claims overlapping a scope.
//!
//! Phase 1 emits only `ConfidenceGrade::Declared` (CodeWatcher-driven
//! Observations land in Phase 2). Read filtering excludes the
//! caller's own session by `(node_id, agent_session_token)`.

use std::sync::Arc;

use serde_json::{json, Value};

use sovereign_core::error::{Error, Result};
use sovereign_core::types::{StepOutput, ToolContext};

use crate::confidence::{observation_grade, ConfidenceGrade};
use crate::store::{ScopeMatch, WorkAtlasStore};
use sovereign_core::tool_manifest::DeclaredTool;

#[derive(Debug)]
pub struct WorkInFlightTool {
    store: Arc<WorkAtlasStore>,
}

impl WorkInFlightTool {
    pub fn new(store: Arc<WorkAtlasStore>) -> Self {
        Self { store }
    }
}

/// Live in-flight signals for one scope, already TTL-filtered,
/// graded, and stripped of the caller's own sessions. JSON shape is
/// exactly what the `work_in_flight` tool returns in `claims` /
/// `observations`.
pub struct InFlight {
    /// Live explicit claims (grade `declared`), TTL-filtered.
    pub claims: Vec<Value>,
    /// CodeWatcher observations graded `active`/`recent` at read time.
    pub observations: Vec<Value>,
}

/// Query + filter the atlas for one scope. Single source of truth for
/// TTL expiry, read-time grading, and self-session exclusion — shared
/// by the `work_in_flight` tool and the session-boot brief's "Work in
/// flight" section so the two surfaces can never disagree.
///
/// `caller_token` identifies the caller's own sessions (paired with
/// this node's id); pass `None` for callers with no registered agent
/// session (e.g. the CLI brief) — same semantics the tool uses when
/// `ToolContext.agent_session_token` is absent.
///
/// `include_self` disables the own-session exclusion entirely. It
/// exists because the exclusion makes "show me MY live claims" (the
/// `claim list` surface, and debugging "why can't I see my claim")
/// inexpressible: proxied CLI callers all share one identity, so
/// without this flag their claims are invisible to themselves.
pub fn collect_in_flight(
    store: &WorkAtlasStore,
    scope: &str,
    match_mode: ScopeMatch,
    caller_token: Option<&str>,
    include_self: bool,
) -> std::result::Result<InFlight, String> {
    let now = now_secs();
    let claims = store
        .list_claims_for_scope(scope, match_mode)
        .map_err(|e| e.to_string())?;
    let observations = store
        .list_observations_for_scope(scope, match_mode)
        .map_err(|e| e.to_string())?;

    let caller_node = store.node_id();
    let self_session_ids: std::collections::HashSet<uuid::Uuid> = if include_self {
        std::collections::HashSet::new()
    } else {
        store
            .scan_sessions()
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|s| {
                s.node_id == caller_node && s.agent_session_token.as_deref() == caller_token
            })
            .map(|s| s.session_id)
            .collect()
    };

    let mut filtered_claims: Vec<Value> = Vec::with_capacity(claims.len());
    for c in claims {
        if c.ttl_expires_at < now {
            continue;
        }
        if self_session_ids.contains(&c.session_id) {
            continue;
        }
        // Fix 1 (commons-fluency): attribution rides the claim. The
        // session-row fallback exists only for claims written by an
        // older binary (node_id absent); it is named, not silent.
        let session_node = c.node_id.or_else(|| {
            store
                .get_session(c.session_id)
                .ok()
                .flatten()
                .map(|s| s.node_id)
        });
        filtered_claims.push(json!({
            "claim_id":       c.claim_id.to_string(),
            "session_id":     c.session_id.to_string(),
            "intent":         c.intent,
            "declared_at":    c.declared_at,
            "ttl_expires_at": c.ttl_expires_at,
            "node_id":        session_node.map(|n| n.to_string()),
            // Is this claim on THIS machine? A bare node_id cannot answer that
            // — it is an opaque hash, and nothing else in the response says
            // which one is the caller's, so a reader has to cross-reference
            // `mesh status` by hand and usually doesn't. Host-local scopes make
            // that fatal: every node's daemon is on :9741, so a scope string
            // like `daemon-runtime:9741-primary-slot` collides across the whole
            // mesh and a peer's claim reads as a lock on YOUR box. That
            // misread stalled real work on 2026-08-07.
            "node_is_self":   session_node == Some(caller_node),
            // Fix 3b (commons-fluency): claims-rail receipt — when
            // THIS node first observed this peer's claim. Always null
            // on the origin's own claim.
            "received_at":    c.received_at,
            "confidence":     ConfidenceGrade::Declared.id(),
            // The claim's own declared scopes (file-path form), so a
            // consumer that matched this claim via a broad prefix
            // query can still show WHAT was claimed.
            "scopes":         c.symbol_refs.iter()
                                  .map(|r| r.file_path.to_string_lossy())
                                  .collect::<Vec<_>>(),
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
        let session_node = store
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
            // See the claims branch: without this, a peer editing the same
            // repo-relative path on a different machine is indistinguishable
            // from a colleague in your own working tree.
            "node_is_self":       session_node == Some(caller_node),
            "confidence":         grade.id(),
        }));
    }

    Ok(InFlight {
        claims: filtered_claims,
        observations: filtered_observations,
    })
}

impl WorkInFlightTool {
    /// Bind this tool's state to its `work_in_flight` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        let run_state = Arc::clone(&state);
        sovereign_core::tool_manifest::declared("work_in_flight", move |params, ctx| {
            let state = Arc::clone(&run_state);
            async move { state.run(&params, &ctx).await }
        })
        .with_signal({
            let state = Arc::clone(&state);
            Arc::new(move || {
                let state = Arc::clone(&state);
                Box::pin(async move { state.signal_now().await })
                    as std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send>>
            })
        })
    }

    /// The executable half of `work_in_flight`.
    async fn run(&self, params: &serde_json::Value, ctx: &ToolContext) -> Result<StepOutput> {
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

        let include_self = params
            .get("include_self")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let in_flight = collect_in_flight(
            &self.store,
            scope,
            match_mode,
            ctx.agent_session_token.as_deref(),
            include_self,
        )
        .map_err(|message| Error::Tool {
            tool_id: "work_in_flight".into(),
            message,
        })?;

        tracing::debug!(
            scope,
            match_mode = match_mode_str,
            claim_hits = in_flight.claims.len(),
            observation_hits = in_flight.observations.len(),
            "work_atlas:query"
        );
        Ok(StepOutput::Json(json!({
            "scope": scope,
            "match_mode": match_mode_str,
            "claims": in_flight.claims,
            "observations": in_flight.observations,
        })))
    }

    async fn signal_now(&self) -> Option<String> {
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
            // Fix 1: node rides the claim; session fallback for old
            // writers (see `collect_in_flight`).
            .filter(|c| {
                c.node_id
                    .or_else(|| session_node(c.session_id))
                    .is_some_and(|n| n != self_node)
            })
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
}

use sovereign_core::time::unix_now_u64 as now_secs;
