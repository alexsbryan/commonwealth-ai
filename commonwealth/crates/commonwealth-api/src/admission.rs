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
use commonwealth_core::ids::NodeId;
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
    /// This node's own slot refused BEFORE parking the caller:
    /// predicted wait exceeded the queue bound. Distinct from
    /// `CeilingExceeded`, which counts concurrent PEER requests —
    /// this one is about how long the caller would have waited in
    /// THIS node's queue, regardless of who sent the turn.
    LocalQueueFull,
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

/// The ONE place a shed becomes an HTTP response: 503 + `Retry-After`
/// + the structured body. Both the peer-admission middleware below and
/// the local queue-shed path in `routes_inference` render through here.
///
/// Why this is a function rather than two call sites that each build a
/// response: a shed is backpressure, and a client that receives it as
/// an untyped `backend_error` cannot tell "busy, come back in 35s" from
/// "something crashed". That was note `bef03728`'s open gap, and the
/// 2026-08-07 live fleet probe turned it into an observed failure —
/// the caller got `{"type":"backend_error"}` carrying its retry hint
/// only inside a prose message, with no `Retry-After` header.
/// A local queue shed, rendered. Both chat entry points (streaming and
/// non-streaming) call this so the body and header are built in exactly
/// one place rather than once per route.
pub fn local_queue_shed_response(
    position: u32,
    predicted_wait_ms: u64,
    retry_after_secs: u64,
) -> Response {
    shed_response(AdmissionRejection {
        error: format!(
            "host busy: ~{predicted_wait_ms} ms predicted wait at queue position {position}"
        ),
        reason: AdmissionReason::LocalQueueFull,
        retry_after_secs,
    })
}

pub fn shed_response(rejection: AdmissionRejection) -> Response {
    let retry_after = rejection.retry_after_secs;
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(RETRY_AFTER, retry_after.to_string())],
        Json(rejection),
    )
        .into_response()
}

/// RAII guard returned by `AppState::admit_peer_request`. Holds one slot in
/// the peer fair scheduler for `node`; `release`s it on drop so callers can't
/// forget. The drop happens at the end of the middleware's response future —
/// including on unwind, which keeps the scheduler accurate when a downstream
/// handler panics.
#[must_use = "drop the guard when the peer request completes — \
              the scheduler slot only releases on drop"]
pub struct PeerInflightGuard {
    inner: Arc<AppStateInner>,
    node: NodeId,
}

impl std::fmt::Debug for PeerInflightGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let in_flight = self.inner.peer_sched.lock().map_or(0, |s| s.in_flight());
        write!(f, "PeerInflightGuard {{ in_flight: {in_flight} }}")
    }
}

impl PeerInflightGuard {
    pub(crate) fn new(inner: Arc<AppStateInner>, node: NodeId) -> Self {
        Self { inner, node }
    }
}

impl Drop for PeerInflightGuard {
    fn drop(&mut self) {
        // Release this node's slot back to the scheduler (promoting any
        // waiter — none on this shed-only gate). Recover from a poisoned lock
        // rather than cascade the panic.
        self.inner
            .peer_sched
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .release(&self.node);
    }
}

/// RAII open/close of the per-peer tally row (order
/// `seat-resource-commons` UC-R1). Construction opens the row
/// (`tally_peer_request_begin`); drop closes it
/// (`tally_peer_request_end`). Panic-safe like [`PeerInflightGuard`]:
/// if the downstream handler unwinds before a response exists, the
/// guard drops on the middleware's stack frame and `active` is not
/// leaked. When a response IS produced, the guard MOVES into the
/// response body's [`TallyBody`], so the decrement fires when the
/// BODY ends — the truthful in-flight window for streaming responses
/// (the scheduler slot, by contrast, releases at headers time).
#[must_use = "drop the guard when the peer request body ends — the tally active counter only decrements on drop"]
pub struct TallyGuard {
    inner: Arc<AppStateInner>,
    node: NodeId,
}

impl TallyGuard {
    pub(crate) fn new(inner: Arc<AppStateInner>, node: NodeId) -> Self {
        inner.tally_peer_request_begin(node);
        Self { inner, node }
    }
}

impl Drop for TallyGuard {
    fn drop(&mut self) {
        self.inner.tally_peer_request_end(self.node);
    }
}

/// Response-body wrapper that holds the request's [`TallyGuard`] for
/// the whole streaming lifetime of the body, so `/status`'s per-peer
/// `active` counter is non-zero from the moment the response headers
/// leave this daemon until the body is consumed, dropped, or the
/// client disconnects — not merely until the handler returned.
///
/// This is the one place the "serving right now" window is defined;
/// every other counter (scheduler slots, admission guard) is
/// headers-time.
pub struct TallyBody {
    inner: axum::body::Body,
    _guard: TallyGuard,
}

impl TallyBody {
    pub(crate) fn new(inner: axum::body::Body, guard: TallyGuard) -> Self {
        Self {
            inner,
            _guard: guard,
        }
    }
}

impl http_body::Body for TallyBody {
    type Data = <axum::body::Body as http_body::Body>::Data;
    type Error = <axum::body::Body as http_body::Body>::Error;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<std::result::Result<http_body::Frame<Self::Data>, Self::Error>>>
    {
        std::pin::Pin::new(&mut self.inner).poll_frame(cx)
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
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
    // Peer request: key the fair scheduler on the origin node. A present-but-
    // unparseable id buckets under the zero node, so it's still gated and
    // never silently bypasses the ceiling.
    let node = crate::headers::parse_x_node_id(&headers).unwrap_or(NodeId::from_u128(0));
    match state.admit_peer_request(node) {
        Ok(_guard) => {
            // _guard binds the scheduler slot to this future's
            // lifetime; the saturating decrement fires when the
            // response future drops (including panic unwind).
            let tally_guard = TallyGuard::new(Arc::clone(&state.inner), node);
            let response = next.run(req).await;
            drop(_guard);
            // The scheduler slot releases at headers time (above); the
            // TALLY's `active` counter instead follows the response
            // BODY's lifetime via TallyBody, so /status answers "is
            // this daemon serving the peer right now?" truthfully for
            // streaming responses (UC-R1). If the handler panicked,
            // `tally_guard` dropped on unwind and active is already
            // back — it moves into the body only when a response
            // exists. `Body::new` re-boxes the wrapper into the axum
            // `Body` type the rest of the router expects.
            response.map(|body| Body::new(TallyBody::new(body, tally_guard)))
        }
        Err(rejection) => {
            // Rejections are NOT tallied: a 503 means "not serving"
            // and must not read as serving on /status.
            tracing::info!(
                reason = ?rejection.reason,
                retry_after_secs = rejection.retry_after_secs,
                "admission: 503 — peer request gated"
            );
            shed_response(rejection)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;
    use axum::routing::post;
    use axum::Router;
    use commonwealth_core::ids::{MeshId, NodeId};
    use commonwealth_core::mesh::Mesh;
    use tower::ServiceExt;

    fn fresh_state() -> AppState {
        use std::collections::HashMap;
        let mesh = Mesh {
            id: MeshId::from_u128(1),
            name: "Admission Test".into(),
            join_key_hash: [0u8; 32],
            require_encryption: false,
            members: HashMap::new(),
            peers: vec![],
        };
        AppState::new(NodeId::from_u128(1), mesh)
    }

    use sovereign_core::time::unix_now;

    fn nid(n: u128) -> NodeId {
        NodeId::from_u128(n)
    }

    #[test]
    fn admits_when_unrestricted() {
        let s = fresh_state();
        let g = s.admit_peer_request(nid(1));
        assert!(g.is_ok());
        assert_eq!(s.peer_inflight_count(), 1);
        drop(g);
        // After drop, the slot is released.
        assert_eq!(s.peer_inflight_count(), 0);
    }

    #[test]
    fn rejects_when_paused() {
        let s = fresh_state();
        s.set_contribution_paused_until(unix_now() + 60);
        let g = s.admit_peer_request(nid(1));
        let err = g.expect_err("expected pause rejection");
        assert!(matches!(err.reason, AdmissionReason::Paused));
        assert!(err.retry_after_secs >= 1);
        // No slot was taken.
        assert_eq!(s.peer_inflight_count(), 0);
    }

    #[test]
    fn expired_pause_admits() {
        let s = fresh_state();
        // Pause that expired 1s ago.
        s.set_contribution_paused_until(unix_now() - 1);
        assert!(s.admit_peer_request(nid(1)).is_ok());
    }

    #[test]
    fn rejects_when_global_ceiling_reached() {
        let s = fresh_state();
        s.set_contribution_max_peer_inflight(2);
        // Two DISTINCT nodes fill the 2 global slots (each capped at 1 when
        // rationing). A third node is shed — the global ceiling is reached.
        let _g1 = s.admit_peer_request(nid(1)).unwrap();
        let _g2 = s.admit_peer_request(nid(2)).unwrap();
        let err = s
            .admit_peer_request(nid(3))
            .expect_err("expected ceiling rejection");
        assert!(matches!(err.reason, AdmissionReason::CeilingExceeded));
        assert_eq!(s.peer_inflight_count(), 2);
    }

    #[test]
    fn per_node_cap_stops_one_node_from_hogging() {
        let s = fresh_state();
        s.set_contribution_max_peer_inflight(4); // rationing, 4 slots
                                                 // A neutral node's cap is 1 even with 3 slots free — anti-hog.
        let _g1 = s.admit_peer_request(nid(1)).unwrap();
        let err = s
            .admit_peer_request(nid(1))
            .expect_err("same node is capped despite free slots");
        assert!(matches!(err.reason, AdmissionReason::CeilingExceeded));
        // A different node still gets in.
        assert!(s.admit_peer_request(nid(2)).is_ok());
    }

    #[test]
    fn ceiling_zero_rejects_all() {
        let s = fresh_state();
        s.set_contribution_max_peer_inflight(0);
        let err = s
            .admit_peer_request(nid(1))
            .expect_err("expected ceiling rejection at 0");
        assert!(matches!(err.reason, AdmissionReason::CeilingExceeded));
    }

    #[test]
    fn rejects_when_yielding_to_foreground() {
        let s = fresh_state();
        s.set_yield_window_secs(60);
        s.bump_foreground_active();
        let err = s
            .admit_peer_request(nid(1))
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
        assert!(s.admit_peer_request(nid(1)).is_ok());
    }

    #[test]
    fn pause_takes_priority_over_ceiling() {
        let s = fresh_state();
        s.set_contribution_max_peer_inflight(0); // would reject too
        s.set_contribution_paused_until(unix_now() + 60);
        let err = s.admit_peer_request(nid(1)).expect_err("expected pause");
        assert!(matches!(err.reason, AdmissionReason::Paused));
    }

    // ── UC-R1 per-peer tally (order seat-resource-commons) ──────────

    fn tally_of(s: &AppState, node: NodeId) -> crate::state::PeerTally {
        s.inner
            .peer_tally_snapshot()
            .into_iter()
            .find(|(id, _)| *id == node)
            .map(|(_, t)| t)
            .expect("no tally row for node")
    }

    #[test]
    fn tally_guard_opens_and_closes_the_row() {
        let s = fresh_state();
        // No requests yet: snapshot is EMPTY — the "never served"
        // reading, distinct from "served, idle now" (active: 0).
        assert!(
            s.inner.peer_tally_snapshot().is_empty(),
            "fresh daemon must have an empty tally"
        );
        let g = TallyGuard::new(Arc::clone(&s.inner), nid(1));
        let t = tally_of(&s, nid(1));
        assert_eq!(t.active, 1, "admit must open the row");
        assert_eq!(t.served_total, 1);
        assert!(t.last_request_at > 0);
        drop(g);
        let t = tally_of(&s, nid(1));
        assert_eq!(t.active, 0, "body end must close the row");
        assert_eq!(
            t.served_total, 1,
            "served_total is cumulative — the witness must survive the request"
        );
    }

    #[test]
    fn tally_served_total_is_monotonic_across_overlapping_requests() {
        let s = fresh_state();
        let g1 = TallyGuard::new(Arc::clone(&s.inner), nid(1));
        let g2 = TallyGuard::new(Arc::clone(&s.inner), nid(1));
        let t = tally_of(&s, nid(1));
        assert_eq!(t.active, 2, "two concurrent bodies = two active");
        assert_eq!(t.served_total, 2);
        drop(g1);
        let t = tally_of(&s, nid(1));
        assert_eq!(t.active, 1);
        assert_eq!(t.served_total, 2, "served_total never decrements");
        drop(g2);
        assert_eq!(tally_of(&s, nid(1)).active, 0);
    }

    #[test]
    fn tally_guard_drop_after_handler_panic_does_not_leak_active() {
        // The handler panicked before a response existed; the guard
        // drops on the middleware's stack frame. active must return
        // to zero — a leak here would make /status read "serving"
        // forever after one panic.
        let s = fresh_state();
        {
            let _g = TallyGuard::new(Arc::clone(&s.inner), nid(1));
            // simulate unwind: scope exit without a response body
        }
        assert_eq!(tally_of(&s, nid(1)).active, 0);
        assert_eq!(tally_of(&s, nid(1)).served_total, 1);
    }

    #[test]
    fn tally_saturating_end_never_goes_negative() {
        let s = fresh_state();
        // end without a begin (poison recovery / raced drop): no panic,
        // and active cannot underflow.
        s.inner.tally_peer_request_end(nid(1));
        assert!(s.inner.peer_tally_snapshot().is_empty());
    }

    fn tally_test_router(state: AppState) -> Router {
        Router::new().route("/chat", post(|| async { "ok" })).layer(
            axum::middleware::from_fn_with_state(state.clone(), peer_admission_layer),
        )
    }

    fn peer_req(path: &str) -> axum::http::Request<axum::body::Body> {
        axum::http::Request::builder()
            .method("POST")
            .uri(path)
            .body(axum::body::Body::empty())
            .unwrap()
    }

    #[tokio::test]
    async fn middleware_tally_holds_active_until_response_body_drops() {
        let s = fresh_state();
        let router = tally_test_router(s.clone());
        // Peer request: header present, admitted.
        let mut req = peer_req("/chat");
        req.headers_mut()
            .insert("x-node-id", nid(0xBEEF).to_hex().parse().unwrap());
        let resp = router
            .clone()
            .oneshot(req)
            .await
            .expect("admitted peer request must reach the handler");
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        // THE assertion: the handler has RETURNED (headers are out)
        // but the response body is still alive — active must read 1.
        // Headers-time counters (scheduler slots) have already
        // released; the tally must NOT have.
        assert_eq!(
            tally_of(&s, nid(0xBEEF)).active,
            1,
            "active must span the body lifetime, not headers time"
        );
        drop(resp);
        assert_eq!(
            tally_of(&s, nid(0xBEEF)).active,
            0,
            "dropping the response body must close the row"
        );
    }

    #[tokio::test]
    async fn middleware_local_request_is_not_tallied() {
        let s = fresh_state();
        let router = tally_test_router(s.clone());
        // Local request: no X-Node-Id header — the user's own chat is
        // never a peer, so it must never appear in the per-peer tally.
        let resp = router
            .clone()
            .oneshot(peer_req("/chat"))
            .await
            .expect("local request must pass through");
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        drop(resp);
        assert!(
            s.inner.peer_tally_snapshot().is_empty(),
            "a local request must not open a tally row"
        );
    }

    #[tokio::test]
    async fn middleware_rejected_request_is_not_tallied() {
        let s = fresh_state();
        s.set_contribution_max_peer_inflight(0); // reject everything
        let router = tally_test_router(s.clone());
        let mut req = peer_req("/chat");
        req.headers_mut()
            .insert("x-node-id", nid(0xBEEF).to_hex().parse().unwrap());
        let resp = router
            .clone()
            .oneshot(req)
            .await
            .expect("rejection is a response too");
        assert_eq!(resp.status(), axum::http::StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            s.inner.peer_tally_snapshot().is_empty(),
            "a 503 is 'not serving' — it must not read as serving on /status"
        );
    }
}
