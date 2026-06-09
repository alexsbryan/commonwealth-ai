// SPDX-License-Identifier: AGPL-3.0-or-later
//! Peer-request admission middleware.
//!
//! The desktop's friend-and-family launch story leans on three
//! invariants this module enforces at the HTTP boundary:
//!
//! - Local requests (no `X-Node-Id` header) are always admitted —
//!   the user's own chat must never 503 because *they* are using
//!   their machine.
//! - Peer requests (`X-Node-Id` present) are subject to three
//!   gates, in order of explicitness:
//!     1. **Pause** — operator hit "Pause for 15 min" in the tray.
//!     2. **Foreground yield** — the local user is actively using
//!        the GPU (a chat completion landed within the yield
//!        window). Prevents the "press send and the GPU is pinned
//!        by a peer's enrich job" failure mode.
//!     3. **Ceiling** — we're already serving as much peer work as
//!        the user has configured.
//! - Every rejection returns a structured 503 body so the
//!   requesting peer's load balancer can pick another peer without
//!   parsing free-form error strings.
//!
//! Wired into routes via per-route `.layer(...)`. Today applied to
//! `POST /v1/chat/completions` (client port; peers reach it via the
//! mesh load balancer) and `POST /internal/knowledge/search`
//! (internal port; peer fan-out).
//!
//! Local requests pay one atomic load (the `X-Node-Id` header check)
//! and skip the rest. The work-stealing model means this hot path
//! must stay cheap.

use std::sync::Arc;

use axum::{
    body::Body,
    extract::State,
    http::{header::RETRY_AFTER, HeaderMap, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

use crate::state::{AppState, AppStateInner};

/// Why a peer request was rejected. Serialised in the 503 body and
/// in tracing spans so contention triage doesn't require log spelunking.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionReason {
    /// Operator-initiated runtime pause is active.
    Paused,
    /// Foreground-yield window: local user has activity in flight,
    /// peer work would contend with their chat.
    YieldedToLocal,
    /// At-or-above the configured ceiling for concurrent peer
    /// requests.
    CeilingExceeded,
}

/// 503 body the admission layer returns to a rejected peer.
/// `retry_after_secs` mirrors the `Retry-After` header value.
#[derive(Debug, Clone, Serialize)]
pub struct AdmissionRejection {
    /// Human-readable explanation; the structured fields below are
    /// what programmatic callers should branch on.
    pub error: String,
    pub reason: AdmissionReason,
    pub retry_after_secs: u64,
}

/// RAII guard returned by `AppState::admit_peer_request`.
/// Decrements `peer_inflight_count` on drop so callers can't forget
/// to release. The drop happens automatically at the end of the
/// middleware's response future — including on unwind, which keeps
/// the counter accurate when a downstream handler panics.
#[must_use = "drop the guard when the peer request completes — \
              the inflight counter only decrements on drop"]
pub struct PeerInflightGuard {
    inner: Arc<AppStateInner>,
}

impl std::fmt::Debug for PeerInflightGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "PeerInflightGuard {{ in_flight: {} }}",
            self.inner
                .peer_inflight_count
                .load(std::sync::atomic::Ordering::Relaxed)
        )
    }
}

impl PeerInflightGuard {
    pub(crate) fn new(inner: Arc<AppStateInner>) -> Self {
        Self { inner }
    }
}

impl Drop for PeerInflightGuard {
    fn drop(&mut self) {
        // Saturating sub so a second drop (shouldn't happen given
        // the move semantics, but defensively) doesn't wrap u32::MAX.
        let _ = self.inner.peer_inflight_count.fetch_update(
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
            |c| Some(c.saturating_sub(1)),
        );
    }
}

/// Axum middleware fn. Apply via
/// `axum::middleware::from_fn_with_state(state, peer_admission_layer)`.
///
/// On admit: forwards to the inner handler with the guard bound to
/// the request's response future, so the inflight counter decrements
/// at response completion.
///
/// On reject: returns 503 + `Retry-After` header + JSON body.
pub async fn peer_admission_layer(
    State(state): State<AppState>,
    headers: HeaderMap,
    req: Request<Body>,
    next: Next,
) -> Response {
    let is_peer = headers.get("x-node-id").is_some();
    if !is_peer {
        return next.run(req).await;
    }
    match state.admit_peer_request() {
        Ok(_guard) => {
            // _guard binds the inflight counter to this future's
            // lifetime; the saturating decrement fires when the
            // response future drops (including panic unwind).
            let response = next.run(req).await;
            drop(_guard);
            response
        }
        Err(rejection) => {
            let retry_after = rejection.retry_after_secs;
            tracing::info!(
                reason = ?rejection.reason,
                retry_after_secs = retry_after,
                "admission: 503 — peer request gated"
            );
            (
                StatusCode::SERVICE_UNAVAILABLE,
                [(RETRY_AFTER, retry_after.to_string())],
                Json(rejection),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;
    use commonwealth_core::ids::{MeshId, NodeId};
    use commonwealth_core::mesh::Mesh;

    fn fresh_state() -> AppState {
        use std::collections::HashMap;
        let mesh = Mesh {
            id: MeshId::from_u128(1),
            name: "Admission Test".into(),
            join_key_hash: [0u8; 32],
            members: HashMap::new(),
            peers: vec![],
        };
        AppState::new(NodeId::from_u128(1), mesh)
    }

    fn unix_now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }

    #[test]
    fn admits_when_unrestricted() {
        let s = fresh_state();
        let g = s.admit_peer_request();
        assert!(g.is_ok());
        assert_eq!(s.peer_inflight_count(), 1);
        drop(g);
        // After drop, count is back to 0.
        assert_eq!(s.peer_inflight_count(), 0);
    }

    #[test]
    fn rejects_when_paused() {
        let s = fresh_state();
        s.set_contribution_paused_until(unix_now() + 60);
        let g = s.admit_peer_request();
        let err = g.expect_err("expected pause rejection");
        assert!(matches!(err.reason, AdmissionReason::Paused));
        assert!(err.retry_after_secs >= 1);
        // No inflight slot was taken.
        assert_eq!(s.peer_inflight_count(), 0);
    }

    #[test]
    fn expired_pause_admits() {
        let s = fresh_state();
        // Pause that expired 1s ago.
        s.set_contribution_paused_until(unix_now() - 1);
        assert!(s.admit_peer_request().is_ok());
    }

    #[test]
    fn rejects_when_ceiling_reached() {
        let s = fresh_state();
        s.set_contribution_max_peer_inflight(2);
        let _g1 = s.admit_peer_request().unwrap();
        let _g2 = s.admit_peer_request().unwrap();
        let err = s
            .admit_peer_request()
            .expect_err("expected ceiling rejection");
        assert!(matches!(err.reason, AdmissionReason::CeilingExceeded));
        assert_eq!(s.peer_inflight_count(), 2);
    }

    #[test]
    fn ceiling_zero_rejects_all() {
        let s = fresh_state();
        s.set_contribution_max_peer_inflight(0);
        let err = s
            .admit_peer_request()
            .expect_err("expected ceiling rejection at 0");
        assert!(matches!(err.reason, AdmissionReason::CeilingExceeded));
    }

    #[test]
    fn rejects_when_yielding_to_foreground() {
        let s = fresh_state();
        s.set_yield_window_secs(60);
        s.bump_foreground_active();
        let err = s
            .admit_peer_request()
            .expect_err("expected foreground-yield rejection");
        assert!(matches!(err.reason, AdmissionReason::YieldedToLocal));
        assert!(err.retry_after_secs >= 1);
    }

    #[test]
    fn yield_disabled_admits_during_foreground() {
        let s = fresh_state();
        s.set_yield_window_secs(60);
        s.bump_foreground_active();
        s.set_yield_peers_to_foreground(false);
        assert!(s.admit_peer_request().is_ok());
    }

    #[test]
    fn pause_takes_priority_over_ceiling() {
        let s = fresh_state();
        s.set_contribution_max_peer_inflight(0); // would reject too
        s.set_contribution_paused_until(unix_now() + 60);
        let err = s.admit_peer_request().expect_err("expected pause");
        assert!(matches!(err.reason, AdmissionReason::Paused));
    }
}
