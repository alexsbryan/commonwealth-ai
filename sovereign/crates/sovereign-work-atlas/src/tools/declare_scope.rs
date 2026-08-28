// SPDX-License-Identifier: AGPL-3.0-or-later
//! `declare_scope` — write a Claim and broadcast it.
//!
//! Idempotent on the underlying session, not on the claim itself
//! (each call produces a new `claim_id`). Calling twice with the
//! same intent is intentional: spec §3 says claims have no history,
//! so a re-declare is the way to refresh.

use std::sync::Arc;

use serde_json::json;
use uuid::Uuid;

use sovereign_core::error::{Error, Result};
use sovereign_core::types::{StepOutput, ToolContext};

use crate::config::WorkAtlasConfig;
use crate::model::{AgentKind, ClaimRecord, Privacy, SymbolRef};
use crate::store::{SessionIdentity, WorkAtlasError, WorkAtlasStore};
use crate::tools::broadcast::ClaimBroadcaster;
use sovereign_core::tool_manifest::DeclaredTool;

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

impl DeclareScopeTool {
    /// Bind this tool's state to its `declare_scope` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        sovereign_core::tool_manifest::declared("declare_scope", move |params, ctx| {
            let state = Arc::clone(&state);
            async move { state.run(&params, &ctx).await }
        })
    }

    /// The executable half of `declare_scope`.
    async fn run(&self, params: &serde_json::Value, ctx: &ToolContext) -> Result<StepOutput> {
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
        // in `file_path` so subsequent `work_in_flight` queries find
        // it (see `matches_scope` fallback in `store.rs`). Phase 2
        // promotes this to actual SCIP-graph lookup with
        // `scip_was_fresh` reflecting reality.
        //
        // Canonical path shape: REPO-RELATIVE. An absolute path
        // inside this repo is stripped at write time — the observer
        // normalizes its CodeWatcher paths the same way — so
        // file-mode readers never have to guess which shape the
        // writer used. (Before 2026-07-23 the two writers disagreed:
        // observations were absolute, claims verbatim; every
        // relative file query silently missed all observations.)
        let symbol_refs: Vec<SymbolRef> = symbols
            .iter()
            .filter_map(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                let raw = std::path::Path::new(s);
                let file_path = raw
                    .strip_prefix(&self.repo_root)
                    .map(std::path::Path::to_path_buf)
                    .unwrap_or_else(|_| raw.to_path_buf());
                SymbolRef {
                    scip_symbol: None,
                    file_path,
                    scip_was_fresh: false,
                }
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
            // Fix 1 (commons-fluency): the claim carries its node
            // directly so peers resolve attribution from the claim
            // itself, never from the slower-replicating session row.
            node_id: Some(self.store.node_id()),
            // Fix 3b (commons-fluency): the origin never sets a
            // receipt — `received_at` is a read-side stamp applied by
            // peers on first observation.
            received_at: None,
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

use sovereign_core::time::unix_now_u64 as now_secs;
