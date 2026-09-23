// SPDX-License-Identifier: AGPL-3.0-or-later
//! `work_in_flight` — list live Claims overlapping a scope.
//!
//! Phase 1 emits only `ConfidenceGrade::Declared` (CodeWatcher-driven
//! Observations land in Phase 2). Read filtering excludes the
//! caller's own session by `(node_id, agent_session_token)`.
//!
//! **Branching decides whether a signal is a lock.** A peer on YOUR
//! branch working the same scope is a collision; a peer on ANOTHER
//! branch is awareness — their work cannot touch your working tree
//! until one branch merges into the other. So every record carries
//! `branch` and `branch_relation` (`same` / `other` / `unknown`), and
//! the response carries `caller_branch` so `same` is a measured fact,
//! not an assumption. `unknown` is reported, never rounded to `same`:
//! a claim written before branches were recorded is a gap, and a
//! guard that reads absence as a match refuses work nobody locked
//! (2026-09-23: a cross-branch claim was read as a lease and stopped a
//! session that had nothing to collide with).

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

/// The relation between the caller's branch and a record's branch.
///
/// `same` — same branch: a claim is a real lock, an observation a real
/// collision risk. `other` — different branches: awareness, not a lock;
/// the work cannot collide until one branch merges. `unknown` — one side
/// (or both) has no branch on record. Never reported as `same`: absence
/// is not a match (ARCH §6), and a guard that reads it as one refuses
/// work nobody locked.
fn branch_relation(caller: Option<&str>, owner: Option<&str>) -> &'static str {
    match (caller, owner) {
        (Some(a), Some(b)) if a == b => "same",
        (Some(_), Some(_)) => "other",
        _ => "unknown",
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
    /// The branch the caller's own session was on when this ran, when one
    /// was recorded — the thing every record's `branch_relation` is
    /// measured against. `None` means the caller has no session branch on
    /// record, so every relation is `unknown`.
    pub caller_branch: Option<String>,
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
    // The caller's own sessions. Their ids are excluded from results unless
    // `include_self`, but their BRANCH is always consulted: it is what every
    // relation is measured against, and a caller debugging their own claim
    // still needs `same` to be a measured fact rather than an empty set.
    let self_sessions: Vec<crate::model::SessionRecord> = store
        .scan_sessions()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|s| s.node_id == caller_node && s.agent_session_token.as_deref() == caller_token)
        .collect();
    let self_session_ids: std::collections::HashSet<uuid::Uuid> = if include_self {
        std::collections::HashSet::new()
    } else {
        self_sessions.iter().map(|s| s.session_id).collect()
    };
    let caller_branch: Option<String> = self_sessions
        .iter()
        .filter_map(|s| s.current_branch.clone().map(|b| (s.last_activity_at, b)))
        .max_by_key(|(last_activity_at, _)| *last_activity_at)
        .map(|(_, branch)| branch);

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
        // older binary (node_id absent); it is named, not silent. The
        // same row answers the BRANCH — resolved once per claim.
        let owner = store.get_session(c.session_id).ok().flatten();
        let session_node = c.node_id.or_else(|| owner.as_ref().map(|s| s.node_id));
        let owner_branch = owner.as_ref().and_then(|s| s.current_branch.as_deref());
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
            // The claim's branch, and its relation to the caller's:
            // `same` = a lock on your branch; `other` = awareness only —
            // the work is on a branch your tree cannot collide with;
            // `unknown` = at least one side has no branch on record.
            "branch":          owner_branch,
            "branch_relation": branch_relation(caller_branch.as_deref(), owner_branch),
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
        let owner = store.get_session(o.session_id).ok().flatten();
        let session_node = owner.as_ref().map(|s| s.node_id);
        let owner_branch = owner.as_ref().and_then(|s| s.current_branch.as_deref());
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
            // Branch + relation, same contract as claims: `other` is
            // awareness, `unknown` is a gap, `same` is the collision.
            "branch":             owner_branch,
            "branch_relation":    branch_relation(caller_branch.as_deref(), owner_branch),
            "confidence":         grade.id(),
        }));
    }

    Ok(InFlight {
        claims: filtered_claims,
        observations: filtered_observations,
        caller_branch,
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
            // What every record's `branch_relation` was measured against.
            // null = the caller has no session branch on record, so every
            // relation is `unknown` and no record may be read as a lock.
            "caller_branch": in_flight.caller_branch,
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
        let session_node = |sid: uuid::Uuid| -> Option<kernel_types::NodeId> {
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
            "work atlas: {} — a claim is a LOCK only on the caller's own branch \
             (`branch_relation: same`); on another branch it is awareness. Query \
             `work_in_flight(scope, match_mode)` before non-trivial edits in this area.",
            parts.join(", ")
        ))
    }
}

use sovereign_core::time::unix_now_u64 as now_secs;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AgentKind, ClaimRecord, Privacy, SessionRecord, SymbolRef};
    use kernel_types::NodeId;
    use sovereign_contracts::peer::{ReplicatedKv, SoloReplicatedKv};
    use std::path::PathBuf;
    use uuid::Uuid;

    fn mk_store() -> Arc<WorkAtlasStore> {
        let mesh = Arc::new(SoloReplicatedKv::new());
        Arc::new(WorkAtlasStore::new(
            mesh as Arc<dyn ReplicatedKv>,
            NodeId::from_u128(1),
        ))
    }

    fn put_session(store: &WorkAtlasStore, node: u128, token: &str, branch: Option<&str>) -> Uuid {
        let rec = SessionRecord {
            session_id: Uuid::new_v4(),
            node_id: NodeId::from_u128(node),
            agent_kind: AgentKind::Agent,
            agent_session_token: Some(token.into()),
            repo_id: "a".repeat(64),
            repo_root: PathBuf::from("/repo"),
            current_branch: branch.map(str::to_string),
            privacy: Privacy::Public,
            created_at: 1,
            last_activity_at: 2,
        };
        store.put_session(&rec).unwrap();
        rec.session_id
    }

    fn put_claim_on(store: &WorkAtlasStore, session_id: Uuid, path: &str) {
        let claim = ClaimRecord {
            claim_id: Uuid::new_v4(),
            session_id,
            intent: "test claim".into(),
            symbol_refs: vec![SymbolRef {
                scip_symbol: None,
                file_path: PathBuf::from(path),
                scip_was_fresh: false,
            }],
            declared_at: 1,
            ttl_expires_at: now_secs() + 3_600,
            node_id: None,
            received_at: None,
        };
        store.put_claim(Privacy::Public, &claim).unwrap();
    }

    #[test]
    fn branch_relation_classifies_same_other_unknown() {
        assert_eq!(branch_relation(Some("main"), Some("main")), "same");
        assert_eq!(branch_relation(Some("main"), Some("feature/x")), "other");
        // Either side missing a branch is a GAP, never a match — the
        // 2026-09-23 incident was a cross-branch claim read as a lease.
        assert_eq!(branch_relation(None, Some("main")), "unknown");
        assert_eq!(branch_relation(Some("main"), None), "unknown");
        assert_eq!(branch_relation(None, None), "unknown");
    }

    /// A peer's live claim on ANOTHER branch is awareness: it must arrive
    /// with `branch_relation: "other"` so no reader can mistake it for a
    /// lock on the caller's branch.
    #[test]
    fn a_cross_branch_claim_is_reported_as_other_not_a_lock() {
        let store = mk_store();
        put_session(&store, 1, "conn:me", Some("main"));
        let peer = put_session(&store, 2, "conn:peer", Some("feature/rail"));
        put_claim_on(&store, peer, "/repo/src/lib.rs");

        let inflight = collect_in_flight(
            &store,
            "/repo/src/lib.rs",
            ScopeMatch::File,
            Some("conn:me"),
            false,
        )
        .unwrap();

        assert_eq!(inflight.caller_branch.as_deref(), Some("main"));
        assert_eq!(inflight.claims.len(), 1, "the peer claim must be visible");
        let c = &inflight.claims[0];
        assert_eq!(c["branch"], "feature/rail");
        assert_eq!(c["branch_relation"], "other");
        // `other` must never read as a lock: node_is_self is about whose
        // machine, and this is a peer's — both facts travel.
        assert_eq!(c["node_is_self"], false);
    }

    /// Same branch, same scope: that IS the collision signal.
    #[test]
    fn a_same_branch_claim_is_reported_as_same() {
        let store = mk_store();
        put_session(&store, 1, "conn:me", Some("main"));
        let peer = put_session(&store, 2, "conn:peer", Some("main"));
        put_claim_on(&store, peer, "/repo/src/lib.rs");

        let inflight = collect_in_flight(
            &store,
            "/repo/src/lib.rs",
            ScopeMatch::File,
            Some("conn:me"),
            false,
        )
        .unwrap();
        assert_eq!(inflight.claims[0]["branch_relation"], "same");
    }

    /// A claim whose owner recorded no branch is `unknown` — reported,
    /// never rounded to `same`.
    #[test]
    fn a_branchless_claim_is_unknown() {
        let store = mk_store();
        put_session(&store, 1, "conn:me", Some("main"));
        let peer = put_session(&store, 2, "conn:peer", None);
        put_claim_on(&store, peer, "/repo/src/lib.rs");

        let inflight = collect_in_flight(
            &store,
            "/repo/src/lib.rs",
            ScopeMatch::File,
            Some("conn:me"),
            false,
        )
        .unwrap();
        assert_eq!(inflight.claims[0]["branch"], serde_json::Value::Null);
        assert_eq!(inflight.claims[0]["branch_relation"], "unknown");
    }
}
