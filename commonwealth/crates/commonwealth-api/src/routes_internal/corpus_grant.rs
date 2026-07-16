// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ephemeral ingest-grant lifecycle routes.
//!
//! These two operator-facing routes bracket a peer-assisted ingest of an
//! otherwise local-only corpus:
//!
//! - `POST /internal/corpus/grant` issues (or renews) an
//!   [`commonwealth_knowledge::EphemeralIngestGrant`] for a `grantable`
//!   corpus, authorizing a user-selected peer set for a bounded, renewable
//!   window. This is the ONLY place the `grantable` marker is enforced:
//!   structural KnowledgeView corpora (`grantable = false`) are refused, so
//!   they can never be lent to peers even transiently.
//! - `POST /internal/corpus/grant/revoke` tears a grant down: it revokes the
//!   capability (a concurrent `corpus_collaborate` immediately fails closed),
//!   retires the work queue so peers stop leasing, and drops the gossiped
//!   handoff blob so peers exit their pull loops.
//!
//! The grant never mutates on-disk `CorpusMeta`/`IndexInfo` — the corpus's
//! standing `mesh_sharing = false` posture is preserved throughout. See
//! `commonwealth-knowledge::ingest_grant` for the store and the "no standing
//! share" rationale.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use commonwealth_core::ids::NodeId;
use commonwealth_knowledge::ingest_grant::DEFAULT_GRANT_TTL_SECS;

use crate::state::AppState;

use super::ErrorBody;

// ── Issue / renew ─────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct GrantRequest {
    pub corpus_id: String,
    /// Full-hex node ids of the user-selected helper peers. May be empty for
    /// a local-only self-serve run (the grant then authorizes only the
    /// coordinator).
    #[serde(default)]
    pub allowed_peers: Vec<String>,
    /// Grant lifetime in seconds. Omitted → `DEFAULT_GRANT_TTL_SECS`. Clamped
    /// to the store's max. Re-issuing renews (extends) the window.
    #[serde(default)]
    pub ttl_secs: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct GrantResponse {
    pub corpus_id: String,
    pub allowed_peers: Vec<String>,
    pub expires_at_ms: u64,
}

/// POST /internal/corpus/grant — issue or renew an ephemeral ingest grant.
pub async fn corpus_grant_issue(
    State(state): State<AppState>,
    Json(req): Json<GrantRequest>,
) -> Result<Json<GrantResponse>, (StatusCode, Json<ErrorBody>)> {
    let engine = state.inner.corpus_engine.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorBody {
                error: "no corpus engine available on this node".into(),
            }),
        )
    })?;

    // Grantability is enforced here, from the recipe (the source of truth,
    // always present at registration). A non-grantable corpus — a structural
    // KnowledgeView corpus — can NEVER be lent to peers, even under a grant.
    let recipe = engine.load_recipe(&req.corpus_id).await.map_err(|e| {
        (
            StatusCode::NOT_FOUND,
            Json(ErrorBody {
                error: format!("cannot resolve recipe for corpus '{}': {e}", req.corpus_id),
            }),
        )
    })?;
    if !recipe.corpus.grantable {
        return Err((
            StatusCode::FORBIDDEN,
            Json(ErrorBody {
                error: format!(
                    "corpus '{}' is not grantable — it may never be lent to peers, even \
                     under a one-off grant",
                    req.corpus_id
                ),
            }),
        ));
    }

    // Parse the requested peers up front so a bad node id is a clean 400
    // rather than a silently-dropped peer.
    let allowed_peers = parse_node_ids(&req.allowed_peers).map_err(|bad| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorBody {
                error: format!("invalid node id in allowed_peers: '{bad}'"),
            }),
        )
    })?;

    let ttl_secs = req.ttl_secs.unwrap_or(DEFAULT_GRANT_TTL_SECS);
    let now_ms = commonwealth_core::clock::unix_now_millis();
    let grant = state
        .inner
        .grant_store
        .issue(req.corpus_id.clone(), allowed_peers, ttl_secs, now_ms);

    tracing::info!(
        corpus = %grant.corpus_id,
        peers = grant.allowed_peers.len(),
        expires_at_ms = grant.expires_at_ms,
        "corpus_grant: issued/renewed ephemeral ingest grant"
    );

    Ok(Json(GrantResponse {
        corpus_id: grant.corpus_id,
        allowed_peers: grant.allowed_peers.iter().map(|n| n.to_hex()).collect(),
        expires_at_ms: grant.expires_at_ms,
    }))
}

// ── Revoke ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct GrantRevokeRequest {
    pub corpus_id: String,
}

#[derive(Debug, Serialize)]
pub struct GrantRevokeResponse {
    pub corpus_id: String,
    /// True when a live grant was found and revoked; false when there was
    /// nothing to revoke (idempotent — still 200).
    pub revoked: bool,
}

/// POST /internal/corpus/grant/revoke — revoke a grant and tear down its job.
pub async fn corpus_grant_revoke(
    State(state): State<AppState>,
    Json(req): Json<GrantRevokeRequest>,
) -> Result<Json<GrantRevokeResponse>, (StatusCode, Json<ErrorBody>)> {
    let Some(grant) = state.inner.grant_store.revoke(&req.corpus_id) else {
        return Ok(Json(GrantRevokeResponse {
            corpus_id: req.corpus_id,
            revoked: false,
        }));
    };

    // Tear down the job the grant authorized. `revoke` already flipped the
    // grant to fail-closed, so no new collaborate can start; here we stop the
    // in-flight one: retire the queue (peers get 404 on `next_unit` and exit
    // their pull loops) and drop the gossiped handoff blob so it stops being
    // rediscovered. Peer partition-dir eviction is driven by the ephemeral
    // teardown path (see `partition_evict`).
    if let Some(handoff_id) = grant.handoff_id {
        state.inner.work_queue.retire(&handoff_id).await;
        let gossip_key = format!("handoff:{handoff_id}");
        let _ = state.inner.mesh_store.delete("corpus-engine", &gossip_key);
        tracing::info!(
            corpus = %grant.corpus_id,
            handoff = %handoff_id,
            "corpus_grant: revoked grant — retired queue and dropped handoff blob"
        );
    } else {
        tracing::info!(
            corpus = %grant.corpus_id,
            "corpus_grant: revoked grant (no handoff bound yet — nothing to retire)"
        );
    }

    Ok(Json(GrantRevokeResponse {
        corpus_id: grant.corpus_id,
        revoked: true,
    }))
}

/// Parse full-hex node ids, returning the first bad token on failure.
fn parse_node_ids(raw: &[String]) -> Result<Vec<NodeId>, String> {
    raw.iter()
        .map(|s| NodeId::from_hex(s).ok_or_else(|| s.clone()))
        .collect()
}
