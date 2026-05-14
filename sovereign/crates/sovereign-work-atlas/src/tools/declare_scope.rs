//! `declare_scope` — write a Claim and broadcast it.
//!
//! Idempotent on the underlying session, not on the claim itself
//! (each call produces a new `claim_id`). Calling twice with the
//! same intent is intentional: spec §3 says claims have no history,
//! so a re-declare is the way to refresh.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::json;
use uuid::Uuid;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::{
    Effect, Idempotency, Latency, Permission, Scope, StepOutput, ToolContext, ToolDescriptor,
    ToolExample,
};

use crate::config::WorkAtlasConfig;
use crate::model::{AgentKind, ClaimRecord, Privacy, SymbolRef};
use crate::store::{SessionIdentity, WorkAtlasError, WorkAtlasStore};
use crate::tools::broadcast::ClaimBroadcaster;

#[derive(Debug)]
pub struct DeclareScopeTool {
    store: Arc<WorkAtlasStore>,
    config: WorkAtlasConfig,
    broadcaster: Arc<dyn ClaimBroadcaster>,
    repo_root: std::path::PathBuf,
    repo_id: String,
    current_branch: Option<String>,
}

impl DeclareScopeTool {
    /// Construct. The repo identity is resolved once at daemon boot
    /// — the daemon's working directory IS the repo. Per spec §10,
    /// `repo_id` is MUST and the boot path hard-fails if missing.
    pub fn new(
        store: Arc<WorkAtlasStore>,
        config: WorkAtlasConfig,
        broadcaster: Arc<dyn ClaimBroadcaster>,
        repo_root: std::path::PathBuf,
        repo_id: String,
        current_branch: Option<String>,
    ) -> Self {
        Self {
            store,
            config,
            broadcaster,
            repo_root,
            repo_id,
            current_branch,
        }
    }
}

#[async_trait]
impl Tool for DeclareScopeTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "declare_scope".to_string(),
            name: "Declare Scope".to_string(),
            description: "Claim a scope (a symbol id or file path) so other agents on the \
                          same mesh can see you're working on it. Use BEFORE non-trivial \
                          work on a function or file the rest of the team also touches; \
                          drop with `release_scope` when done. \
                          Claims expire on TTL (default 4h, configurable) and are dropped \
                          when your session ends. Empty intent is rejected — the intent is \
                          what tells colliding agents what you're trying to do."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "symbols": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "One or more SCIP symbol ids or file paths. Mixed list OK."
                    },
                    "intent": {
                        "type": "string",
                        "description": "What you're trying to do. Non-empty."
                    },
                    "ttl_seconds": {
                        "type": "integer",
                        "description": "Override the default TTL. Clamped to the configured max.",
                        "minimum": 1
                    }
                },
                "required": ["symbols", "intent"]
            }),
            examples: vec![ToolExample {
                situation: "Before refactoring a function several files reference.".into(),
                call: json!({
                    "symbols": ["CorpusEngine::ingest"],
                    "intent": "split ingest into recipe-driven phases",
                    "ttl_seconds": 7200
                }),
            }],
            effect: Effect::Write,
            idempotency: Idempotency::NonIdempotent,
            latency: Latency::Fast,
            scope: Scope::Persistent,
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "claim_id":        { "type": "string" },
                    "session_id":      { "type": "string" },
                    "ttl_expires_at":  { "type": "integer" },
                    "intent":          { "type": "string" }
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    async fn execute(&self, params: &serde_json::Value, ctx: &ToolContext) -> Result<StepOutput> {
        let intent_raw = params
            .get("intent")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("declare_scope requires 'intent'".into()))?;
        if intent_raw.trim().is_empty() {
            return Err(Error::InvalidInput("intent must not be empty".into()));
        }

        let symbols = params
            .get("symbols")
            .and_then(|v| v.as_array())
            .ok_or_else(|| Error::InvalidInput("declare_scope requires 'symbols' array".into()))?;
        if symbols.is_empty() {
            return Err(Error::InvalidInput("'symbols' must not be empty".into()));
        }

        let ttl_seconds = self
            .config
            .clamp_ttl(params.get("ttl_seconds").and_then(|v| v.as_u64()));

        let identity = SessionIdentity {
            node_id: self.store.node_id(),
            agent_session_token: ctx.agent_session_token.clone(),
            repo_id: self.repo_id.clone(),
        };

        let privacy = self.config.node.default_privacy_enum();
        let session = self
            .store
            .ensure_session(
                identity,
                privacy,
                AgentKind::Agent,
                self.repo_root.clone(),
                self.current_branch.clone(),
            )
            .map_err(map_err)?;

        // Phase 1: SCIP resolution deferred. Store the user's string
        // verbatim in `file_path` so subsequent `work_in_flight`
        // queries with the same string find it (see `matches_scope`
        // fallback in `store.rs`). Phase 2 promotes this to actual
        // SCIP-graph lookup with `scip_was_fresh` reflecting reality.
        let symbol_refs: Vec<SymbolRef> = symbols
            .iter()
            .filter_map(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(|s| SymbolRef {
                scip_symbol: None,
                file_path: std::path::PathBuf::from(s),
                scip_was_fresh: false,
            })
            .collect();
        if symbol_refs.is_empty() {
            return Err(Error::InvalidInput(
                "'symbols' contained no non-empty strings".into(),
            ));
        }

        let now = now_secs();
        let claim = ClaimRecord {
            claim_id: Uuid::new_v4(),
            session_id: session.session_id,
            intent: intent_raw.trim().to_string(),
            symbol_refs,
            declared_at: now,
            ttl_expires_at: now.saturating_add(ttl_seconds),
        };

        self.store.put_claim(privacy, &claim).map_err(map_err)?;

        // Public claims fan out immediately. Private claims never
        // broadcast (their namespace is in `GOSSIP_EXCLUDED_APP_IDS`)
        // — `broadcast_now` itself enforces this as a third privacy
        // layer, but we also guard here so we don't even attempt.
        if privacy == Privacy::Public {
            let key = format!("claim:{}", claim.claim_id);
            self.broadcaster.broadcast(privacy.app_id(), &key).await;
        }

        tracing::info!(
            claim_id = %claim.claim_id,
            session_id = %claim.session_id,
            intent = %claim.intent,
            ttl_seconds,
            privacy = privacy.id(),
            "work_atlas:claim_declared"
        );

        Ok(StepOutput::Json(json!({
            "claim_id":       claim.claim_id.to_string(),
            "session_id":     claim.session_id.to_string(),
            "ttl_expires_at": claim.ttl_expires_at,
            "intent":         claim.intent,
        })))
    }
}

fn map_err(e: WorkAtlasError) -> Error {
    match e {
        WorkAtlasError::EmptyIntent => Error::InvalidInput("intent must not be empty".into()),
        other => Error::Tool {
            tool_id: "declare_scope".into(),
            message: other.to_string(),
        },
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
