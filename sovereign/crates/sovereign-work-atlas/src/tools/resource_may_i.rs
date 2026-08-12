// SPDX-License-Identifier: AGPL-3.0-or-later
//! `resource_may_i` — "is this shared resource taken?" (order
//! `seat-resource-commons` UC-R2/UC-R3).
//!
//! The one-question read surface over the claims rail: a seat about to
//! touch a shared resource (restart a daemon, run a soak, republish a
//! snapshot) asks once and gets a verdict:
//!
//! - `held`    — a live claim names this scope, with node attribution,
//!               intent, and seconds remaining.
//! - `expired` — claims existed but every one has outlived its TTL.
//!               Meaning: someone STARTED this and never released —
//!               the work may have died mid-run (UC-R3's negative
//!               control). Distinct from `free` on purpose.
//! - `free`    — no claim ever, or the last one was explicitly
//!               released. Released means the work FINISHED.
//!
//! Deliberately NOT a lock manager: the verdict never blocks, and a
//! seat may always override with its reason recorded (order's
//! not-worth-continuing-if).
//!
//! The distinction from `work_in_flight` is the point: that tool
//! TTL-filters at read time, so an abandoned claim is invisible and
//! "did the peer die?" is unanswerable. This tool scans including
//! expired rows and reports them as `expired` — see `resource_verdict`.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::{
    Effect, Idempotency, Latency, Permission, Scope, StepOutput, ToolContext, ToolDescriptor,
    ToolExample,
};

use crate::model::ClaimRecord;
use crate::store::{ScopeMatch, WorkAtlasStore};

/// Default TTL for a RESOURCE claim (`claim take`), in seconds —
/// 30 minutes. One implementation per threshold (§10.6): this is the
/// only place the resource-commons default lives; `claim take` in
/// sovereign-cli-llm imports it, and the daemon's `declare_scope`
/// clamps whatever arrives against its configured max.
///
/// Shorter than the general claim default (4h) on purpose: a resource
/// claim answers "is someone mid-operation on this right now?", and a
/// mid-operation window of minutes, not hours, is the honest answer.
/// The abandoned-claim case (UC-R3) clears itself faster this way too.
pub const DEFAULT_RESOURCE_TTL_SECS: u64 = 1800;

#[derive(Debug)]
pub struct ResourceMayITool {
    store: Arc<WorkAtlasStore>,
}

impl ResourceMayITool {
    pub fn new(store: Arc<WorkAtlasStore>) -> Self {
        Self { store }
    }
}

/// The verdict for one resource scope, machine-readable. `claims` is
/// the full evidence list — live ones carry `seconds_remaining`,
/// expired ones `expired_seconds_ago`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceVerdict {
    /// At least one live (unexpired) claim names this scope.
    Held { claims: Vec<Value> },
    /// Claims exist but every one has expired — the taker never
    /// released; the work may have died mid-run.
    Expired { claims: Vec<Value> },
    /// No claim names this scope, live or expired — the resource is
    /// unclaimed and was never taken (or was explicitly released,
    /// which means the work finished).
    Free,
}

impl ResourceVerdict {
    pub fn id(&self) -> &'static str {
        match self {
            ResourceVerdict::Held { .. } => "held",
            ResourceVerdict::Expired { .. } => "expired",
            ResourceVerdict::Free => "free",
        }
    }
}

/// Decide whether a resource scope is taken, EXACT scope match.
///
/// Scans Public claims INCLUDING expired ones — that is the whole
/// difference from `work_in_flight`, which TTL-filters at read time
/// and therefore cannot distinguish "released" (work finished) from
/// "expired" (taker died mid-run; UC-R3).
///
/// Exact match (`ScopeMatch::Symbol`) is deliberate: the resource
/// scope convention is a node-qualified name (`daemon:<node>:<action>`),
/// and prefix matching would make a claim on `daemon:BeefyMac:restart`
/// answer for `daemon:BeefyMac:restart-verify`. "Is THIS resource
/// taken?" is an equality question.
pub fn resource_verdict(
    store: &WorkAtlasStore,
    scope: &str,
) -> std::result::Result<ResourceVerdict, String> {
    let now = now_secs();
    let claims = store
        .list_claims_for_scope(scope, ScopeMatch::Symbol)
        .map_err(|e| e.to_string())?;

    let caller_node = store.node_id();
    let session_node = |c: &ClaimRecord| -> Option<commonwealth_core::ids::NodeId> {
        store
            .get_session(c.session_id)
            .ok()
            .flatten()
            .map(|s| s.node_id)
    };

    let mut live: Vec<Value> = Vec::new();
    let mut expired: Vec<Value> = Vec::new();
    for c in claims {
        let node = session_node(&c);
        let row = json!({
            "claim_id":       c.claim_id.to_string(),
            "session_id":     c.session_id.to_string(),
            "intent":         c.intent,
            "declared_at":    c.declared_at,
            "ttl_expires_at": c.ttl_expires_at,
            "node_id":        node.map(|n| n.to_string()),
            // Same self/peer disambiguation as work_in_flight: a bare
            // node_id is an opaque hash and cannot answer "is this MY
            // claim?"; node_is_self can.
            "node_is_self":   node == Some(caller_node),
        });
        if c.ttl_expires_at >= now {
            let mut obj = row.as_object().expect("json object").clone();
            obj.insert("state".into(), json!("live"));
            obj.insert(
                "seconds_remaining".into(),
                json!(c.ttl_expires_at.saturating_sub(now)),
            );
            live.push(Value::Object(obj));
        } else {
            let mut obj = row.as_object().expect("json object").clone();
            obj.insert("state".into(), json!("expired"));
            obj.insert(
                "expired_seconds_ago".into(),
                json!(now.saturating_sub(c.ttl_expires_at)),
            );
            expired.push(Value::Object(obj));
        }
    }

    if !live.is_empty() {
        Ok(ResourceVerdict::Held { claims: live })
    } else if !expired.is_empty() {
        Ok(ResourceVerdict::Expired { claims: expired })
    } else {
        Ok(ResourceVerdict::Free)
    }
}

#[async_trait]
impl Tool for ResourceMayITool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "resource_may_i".to_string(),
            name: "Resource May I".to_string(),
            description: "One-question check before touching a SHARED resource: is it \
                          taken right now? Verdict `held` = a live claim names this \
                          scope, with node attribution, intent, and time remaining; \
                          `expired` = someone took it and never released (their work may \
                          have died mid-run) — NOT the same as free; `free` = never \
                          claimed or explicitly released (the work finished). \
                          Deliberately NOT a lock: the verdict never blocks, and a seat \
                          may always override with its reason recorded. Scope convention: \
                          node-qualified names like `daemon:<node>:<action>` — exact \
                          match, so `daemon:BeefyMac:restart` does not answer for \
                          `daemon:BeefyMac:restart-verify`. Read `node_is_self` on each \
                          claim to tell your own claim from a peer's."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "scope": {
                        "type": "string",
                        "description": "The resource scope, e.g. `daemon:<node>:restart`. Exact match."
                    }
                },
                "required": ["scope"]
            }),
            examples: vec![ToolExample {
                situation: "About to restart a shared daemon — is anyone using it right now?"
                    .into(),
                call: json!({ "scope": "daemon:BeefyMac:restart" }),
            }],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Persistent,
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "scope":  { "type": "string" },
                    "verdict": { "type": "string", "enum": ["held", "expired", "free"] },
                    "claims": { "type": "array", "items": { "type": "object" } }
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    async fn execute(&self, params: &Value, ctx: &ToolContext) -> Result<StepOutput> {
        let scope = params
            .get("scope")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("resource_may_i requires 'scope'".into()))?;
        if scope.trim().is_empty() {
            return Err(Error::InvalidInput("'scope' must not be empty".into()));
        }

        let verdict = resource_verdict(&self.store, scope).map_err(|message| Error::Tool {
            tool_id: "resource_may_i".into(),
            message,
        })?;

        tracing::debug!(
            scope,
            verdict = verdict.id(),
            self_node = %self.store.node_id(),
            caller_session = ctx.agent_session_token.as_deref().unwrap_or("<none>"),
            "work_atlas:resource_may_i"
        );

        let (verdict_id, claims) = match &verdict {
            ResourceVerdict::Held { claims } => ("held", claims.clone()),
            ResourceVerdict::Expired { claims } => ("expired", claims.clone()),
            ResourceVerdict::Free => ("free", Vec::new()),
        };

        Ok(StepOutput::Json(json!({
            "scope": scope,
            "verdict": verdict_id,
            "claims": claims,
        })))
    }
}

use sovereign_core::time::unix_now_u64 as now_secs;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AgentKind, Privacy, SessionRecord, SymbolRef};
    use commonwealth_core::ids::NodeId;
    use commonwealth_state::MeshStore;
    use sovereign_core::types::{ConversationId, ToolContext};
    use std::path::PathBuf;
    use std::sync::Arc;
    use uuid::Uuid;

    fn mk_store() -> WorkAtlasStore {
        let mesh = Arc::new(MeshStore::in_memory().unwrap());
        WorkAtlasStore::new(mesh, NodeId::from_u128(1))
    }

    fn ctx() -> ToolContext {
        ToolContext {
            conversation_id: ConversationId::from("test-conv"),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
        }
    }

    fn sample_session(node: u128) -> SessionRecord {
        SessionRecord {
            session_id: Uuid::new_v4(),
            node_id: NodeId::from_u128(node),
            agent_kind: AgentKind::Agent,
            agent_session_token: Some("conn:abc".into()),
            repo_id: "a".repeat(64),
            repo_root: PathBuf::from("/tmp/x"),
            current_branch: Some("main".into()),
            privacy: Privacy::Public,
            created_at: 0,
            last_activity_at: 0,
        }
    }

    /// Write a Public claim naming `scope`, owned by `node`, with
    /// `ttl_expires_at` exactly `expiry` (relative to the fixed clock
    /// `now` captured by the caller).
    fn put_claim(
        store: &WorkAtlasStore,
        session: &SessionRecord,
        scope: &str,
        ttl_expires_at: u64,
    ) {
        let claim = ClaimRecord {
            claim_id: Uuid::new_v4(),
            session_id: session.session_id,
            intent: format!("test claim on {scope}"),
            symbol_refs: vec![SymbolRef {
                scip_symbol: None,
                file_path: PathBuf::from(scope),
                scip_was_fresh: false,
            }],
            declared_at: 0,
            ttl_expires_at,
        };
        store.put_claim(Privacy::Public, &claim).unwrap();
    }

    /// NodeId Display is `node-` + hex of the FIRST 8 bytes only
    /// (`ids.rs define_id!`): the HIGH 64 bits, low 64 dropped. Test
    /// ids must live in the high 64 bits to survive the round-trip;
    /// shift the parsed value back up.
    fn node_id_of(v: &Value) -> u128 {
        let s = v["node_id"].as_str().unwrap();
        u128::from_str_radix(s.strip_prefix("node-").unwrap(), 16).unwrap() << 64
    }

    /// A node id whose first 8 bytes are non-zero (see `node_id_of`).
    fn node(n: u8) -> u128 {
        (n as u128) << 120
    }

    #[test]
    fn free_when_nothing_ever_claimed() {
        let store = mk_store();
        let v = resource_verdict(&store, "daemon:BeefyMac:restart").unwrap();
        assert_eq!(v.id(), "free");
        assert_eq!(v, ResourceVerdict::Free);
    }

    #[test]
    fn held_when_live_claim_matches_exactly() {
        let store = mk_store();
        let sess = sample_session(node(2)); // peer node
        store.put_session(&sess).unwrap();
        let now = now_secs();
        put_claim(&store, &sess, "daemon:BeefyMac:restart", now + 600);

        let v = resource_verdict(&store, "daemon:BeefyMac:restart").unwrap();
        let ResourceVerdict::Held { claims } = v else {
            panic!("expected held, got {:?}", v.id());
        };
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0]["seconds_remaining"].as_u64(), Some(600));
        assert_eq!(claims[0]["node_is_self"].as_bool(), Some(false));
        assert_eq!(node_id_of(&claims[0]), node(2));
    }

    #[test]
    fn exact_match_only_prefix_does_not_answer() {
        let store = mk_store();
        let sess = sample_session(2);
        store.put_session(&sess).unwrap();
        let now = now_secs();
        put_claim(&store, &sess, "daemon:BeefyMac:restart-verify", now + 600);

        // The narrower query must NOT be answered by the longer scope:
        // prefix semantics would make "restart" match "restart-verify".
        let v = resource_verdict(&store, "daemon:BeefyMac:restart").unwrap();
        assert_eq!(v.id(), "free");
        // And the exact one still resolves.
        let v2 = resource_verdict(&store, "daemon:BeefyMac:restart-verify").unwrap();
        assert_eq!(v2.id(), "held");
    }

    #[test]
    fn expired_is_distinct_from_free_and_carries_ago() {
        let store = mk_store();
        let sess = sample_session(2);
        store.put_session(&sess).unwrap();
        let now = now_secs();
        // Claim expired 300s ago — the taker never released.
        put_claim(
            &store,
            &sess,
            "daemon:BeefyMac:restart",
            now.saturating_sub(300),
        );

        let v = resource_verdict(&store, "daemon:BeefyMac:restart").unwrap();
        let ResourceVerdict::Expired { claims } = v else {
            panic!("expected expired, got {:?}", v.id());
        };
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0]["expired_seconds_ago"].as_u64(), Some(300));
        assert_eq!(claims[0]["state"].as_str(), Some("expired"));

        // A DIFFERENT scope, never touched, is free — "released or
        // never taken" must not read as "expired" (UC-R3: those mean
        // different things about whether the work finished).
        let other = resource_verdict(&store, "daemon:RuggedFox:restart").unwrap();
        assert_eq!(other.id(), "free");
    }

    #[test]
    fn expired_plus_new_live_claim_is_held() {
        let store = mk_store();
        let old = sample_session(node(2));
        store.put_session(&old).unwrap();
        let now = now_secs();
        put_claim(
            &store,
            &old,
            "daemon:BeefyMac:restart",
            now.saturating_sub(60),
        );

        // The resource was re-taken: live claim wins the verdict, and
        // the expired row stays visible as evidence.
        let new = sample_session(node(3));
        store.put_session(&new).unwrap();
        put_claim(&store, &new, "daemon:BeefyMac:restart", now + 120);

        let v = resource_verdict(&store, "daemon:BeefyMac:restart").unwrap();
        let ResourceVerdict::Held { claims } = v else {
            panic!("expected held, got {:?}", v.id());
        };
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0]["seconds_remaining"].as_u64(), Some(120));
        assert_eq!(claims[0]["state"].as_str(), Some("live"));
    }

    #[test]
    fn self_claim_is_marked_node_is_self() {
        let store = mk_store();
        let sess = sample_session(1); // this store's own node
        store.put_session(&sess).unwrap();
        let now = now_secs();
        put_claim(&store, &sess, "daemon:RuggedFox:restart", now + 300);

        let v = resource_verdict(&store, "daemon:RuggedFox:restart").unwrap();
        let ResourceVerdict::Held { claims } = v else {
            panic!("expected held, got {:?}", v.id());
        };
        assert_eq!(claims[0]["node_is_self"].as_bool(), Some(true));
    }

    #[tokio::test]
    async fn empty_or_missing_scope_is_rejected() {
        let store = mk_store();
        let tool = ResourceMayITool::new(Arc::new(store));
        let ctx = ctx();
        let err = tool.execute(&json!({}), &ctx).await.unwrap_err();
        assert!(err.to_string().contains("requires 'scope'"));
        let err = tool
            .execute(&json!({ "scope": "  " }), &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
    }
}
