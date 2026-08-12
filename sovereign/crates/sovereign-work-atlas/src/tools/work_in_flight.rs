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
                          Excludes the caller's own session. \
                          READ `node_is_self` BEFORE ACTING: it is true only \
                          when the record is on YOUR machine. Host-local scope \
                          names collide across the mesh — every node's daemon \
                          listens on :9741, so a scope like \
                          `daemon-runtime:9741-primary-slot` matches every \
                          node's daemon and a peer's claim looks exactly like a \
                          lock on your own box. `node_id` alone cannot tell \
                          them apart; `node_is_self` can. \
                          File paths are stored REPO-RELATIVE (canonical since \
                          2026-07-23) — query with repo-relative paths. An \
                          empty scope with match_mode=file matches EVERYTHING: \
                          the supported way to fetch all live signals at once."
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
                    },
                    "include_self": {
                        "type": "boolean",
                        "default": false,
                        "description": "Also return the caller's own sessions' records. Default false (coordination view: peers only). Set true to audit ALL live signals, e.g. listing your own claims."
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
}

use sovereign_core::time::unix_now_u64 as now_secs;
