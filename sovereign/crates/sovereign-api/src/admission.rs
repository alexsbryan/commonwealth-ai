// SPDX-License-Identifier: AGPL-3.0-or-later
//! The daemon's side of admission — the state the decision reads, and the
//! guards it hands back.
//!
//! The decision itself, the two axum middlewares and the 503 renderer live in
//! `sovereign-serving-host::admission` (`sovereign/SERVING_BOUNDARY.md` "The
//! five entries" (c)); this module re-exports them at their historical paths
//! and implements the two ports over [`AppState`]:
//!
//! - [`Admission`] — the peer ceiling (pause, foreground yield, the
//!   reciprocity-scaled `SchedCore` cap) and the client fair share, dispatched
//!   on the [`Principal`] arm. One decider, one key (ARCH principle 8).
//! - [`AdmissionHost`] — the edge resolver (`crate::principal`, which stays
//!   here until `REVIEW-mint-principal`), the peer tally row, the canonical
//!   `X-Node-Id` parser and the malformed-header record.
//!
//! The RAII guards stay here because they hold `Arc<AppStateInner>`: the peer
//! slot releases at headers time, the tally and the client share at the
//! response BODY's end. Each is an [`AdmissionLease`] so the host's
//! middlewares carry it as an opaque box.

use std::sync::Arc;

use crate::state::{AppState, AppStateInner};
use axum::http::HeaderMap;
use commonwealth_core::ids::NodeId;

pub use sovereign_serving_host::admission::{
    client_fair_concurrency_from_env, client_fairness_enabled_from_env, client_fairness_layer,
    jitter_retry_after, jittered_retry_after_secs, local_queue_shed_response, peer_admission_layer,
    shed_response, Admission, AdmissionHost, AdmissionLease, AdmissionPosture, AdmissionReason,
    AdmissionRejection, AdmissionVerdict, GuardedBody, Principal, DEFAULT_CLIENT_FAIR_CONCURRENCY,
    RETRY_AFTER_JITTER_SPREAD_SECS,
};

/// RAII guard returned by the peer admission decision. Holds one slot in the
/// peer fair scheduler for `node`; `release`s it on drop so callers can't
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

impl AdmissionLease for PeerInflightGuard {}

/// RAII open/close of the per-peer tally row (order `seat-resource-commons`
/// UC-R1). Construction opens the row (`tally_peer_request_begin`); drop closes
/// it (`tally_peer_request_end`). Panic-safe like [`PeerInflightGuard`]: if the
/// downstream handler unwinds before a response exists, the guard drops on the
/// middleware's stack frame and `active` is not leaked. When a response IS
/// produced, the guard moves into the response body's wrapper, so the decrement
/// fires when the BODY ends — the truthful in-flight window for streaming
/// responses (the scheduler slot, by contrast, releases at headers time).
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

impl AdmissionLease for TallyGuard {}

/// RAII guard holding one principal's fair-share slot. Released on drop, which
/// — because it rides the host's [`GuardedBody`] — is when the response BODY
/// ends, not when the handler returned.
#[must_use = "drop the guard when the client turn's body ends — the principal's \
              share only frees on drop"]
pub struct ClientShareGuard {
    inner: Arc<AppStateInner>,
    key: Principal,
}

impl ClientShareGuard {
    fn new(inner: Arc<AppStateInner>, key: Principal) -> Self {
        Self { inner, key }
    }
}

impl Drop for ClientShareGuard {
    fn drop(&mut self) {
        let mut sched = self
            .inner
            .client_sched
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        sched.release(&self.key);
        tracing::debug!(
            target: "admission",
            principal = %self.key,
            principal_inflight = sched.inflight_of(&self.key),
            active_principals = sched.active_keys(),
            "admission.client: share released"
        );
    }
}

impl AdmissionLease for ClientShareGuard {}

/// The client fair-share decision, over the daemon's `SchedCore<Principal>`.
///
/// The §9.3 red in one sentence: ten callers with ten credentials were served
/// strictly by arrival order, so the one keeping 32 requests in flight took
/// 79.5% of the turns against a 10% population share. This is the missing
/// consult — the fair share of a principal already ahead of its neighbours.
///
/// **What this is not.** It is not a shed: it never inspects the queue, the
/// host's load, or a predicted wait; those belong to the inference slot queue,
/// which stays THE shed decider (§7.1 R2). It never queues either — `try_grant`
/// leaves no waiter behind. And it never ranks: the weight is a constant `1.0`,
/// because weight-ordering is condemned (`SCHEDULER_QUALITY.md` F6) and the fix
/// §9.3 asks for is *equal* share, not *ranked* share.
fn admit_client(state: &AppState, who: &Principal) -> AdmissionVerdict {
    let enforcing = state.client_fairness_enabled();
    let budget = state.client_fair_concurrency();

    let (outcome, cap, active, inflight) = {
        let mut sched = state.lock_client_sched();
        let active = sched.active_keys_including(who);
        let cap = serving_policy::fair_sched::fair_share_cap(budget, active);
        let inflight = sched.inflight_of(who);
        // Weight is a constant: see the "never ranks" note above.
        let outcome = if enforcing {
            sched.try_grant(who.clone(), 1.0, cap)
        } else {
            // Observe-only: still take the slot so the accounting (and the
            // `active` denominator) is identical to the enforcing path —
            // otherwise the A/B would compare two different measurements.
            sched.try_grant(who.clone(), 1.0, u32::MAX)
        };
        (outcome, cap, active, inflight)
    };

    // Glassbox: EVERY admission decision names the principal, the share it was
    // measured against, and what was decided. `target: "admission"` is a custom
    // target — it is dark unless the tracing filter lists it (see
    // `quality/env-flags.toml`).
    let granted = matches!(outcome, serving_policy::fair_sched::TryGrant::Granted);
    tracing::debug!(
        target: "admission",
        principal = %who,
        active_principals = active,
        fair_share_cap = cap,
        principal_inflight = inflight,
        budget,
        enforcing,
        decision = if granted { "admit" } else { "over-share" },
        "admission.client: fair-share decision"
    );

    if granted {
        return AdmissionVerdict::Admitted(Box::new(ClientShareGuard::new(
            Arc::clone(&state.inner),
            who.clone(),
        )));
    }

    // Over its share. This is backpressure with a hint, rendered through the
    // one shed renderer so a client cannot tell it apart from any other
    // `Retry-After` refusal it already handles.
    let retry_after_secs = jittered_retry_after_secs(1);
    tracing::info!(
        target: "admission",
        principal = %who,
        fair_share_cap = cap,
        active_principals = active,
        retry_after_secs,
        "admission.client: 503 — principal is over its equal share"
    );
    AdmissionVerdict::Rejected(AdmissionRejection::new(
        format!(
            "over fair share: this caller holds {inflight} of {cap} concurrent turns \
             while {active} principals are active"
        ),
        AdmissionReason::PrincipalShareExceeded,
        retry_after_secs,
    ))
}

impl Admission for AppState {
    fn admit(&self, who: &Principal, now_unix_ms: u64) -> AdmissionVerdict {
        match who {
            // A verified member is peer traffic: the ceiling, scaled by its
            // reciprocity weight, under the pause and the foreground yield.
            Principal::Member { node_id } => {
                match self.admit_peer_request_at(*node_id, (now_unix_ms / 1000) as i64) {
                    Ok(guard) => AdmissionVerdict::Admitted(Box::new(guard)),
                    Err(rejection) => AdmissionVerdict::Rejected(rejection),
                }
            }
            // Every other arm is a client: the fair share.
            Principal::LocalOwner { .. }
            | Principal::RemoteClient { .. }
            | Principal::Guest { .. }
            | Principal::Anonymous => admit_client(self, who),
        }
    }

    fn posture(&self) -> AdmissionPosture {
        let now = sovereign_time::unix_now();
        if self.seconds_until_unpaused_at(now).is_some() {
            AdmissionPosture::Paused
        } else if self.yield_peers_to_foreground()
            && self.seconds_until_foreground_idle_at(now).is_some()
        {
            AdmissionPosture::ForegroundYield
        } else if self.peer_inflight_count() >= self.contribution_max_peer_inflight() {
            AdmissionPosture::Ceiling
        } else {
            AdmissionPosture::Open
        }
    }
}

impl AdmissionHost for AppState {
    fn resolve(&self, headers: &HeaderMap, peer: Option<std::net::SocketAddr>) -> Principal {
        // THE resolver, called here and nowhere else. Its `PrincipalKey` and
        // this `Principal` partition the same three buckets, so the key change
        // is behaviour-preserving; `DAEMON_CORE.md` §3.3 moves the resolver to
        // the daemon's edge in `REVIEW-mint-principal`.
        match crate::principal::resolve_principal(headers, peer).key {
            crate::principal::PrincipalKey::Credential(fp) => {
                Principal::RemoteClient { credential: fp }
            }
            crate::principal::PrincipalKey::Declared(name) => Principal::LocalOwner {
                sub_identity: Some(name),
            },
            crate::principal::PrincipalKey::Anonymous => Principal::Anonymous,
        }
    }

    fn parse_node_id(&self, headers: &HeaderMap) -> Option<NodeId> {
        crate::headers::parse_x_node_id(headers)
    }

    fn peer_tally(&self, node: &NodeId) -> Box<dyn AdmissionLease> {
        Box::new(TallyGuard::new(Arc::clone(&self.inner), *node))
    }

    fn record_rejected_node_id(&self, raw: &str) {
        self.inner.record_rejected_x_node_id(raw);
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

    use axum::http::header::RETRY_AFTER;
    use axum::response::Response;

    fn fresh_state() -> AppState {
        use std::collections::HashMap;
        let mesh = Mesh {
            mesh_secret: [0u8; 32],
            invite_expires_at: None,
            id: MeshId::from_u128(1),
            name: "Admission Test".into(),
            invite_key_hash: [0u8; 32],
            invite_version: 0,
            require_encryption: false,
            members: HashMap::new(),
            peers: vec![],
        };
        AppState::new(NodeId::from_u128(1), mesh)
    }

    use sovereign_time::unix_now;

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

    /// RED-FIRST (order mesh-scale-t0, item 2). Before the fix, the ceiling
    /// shed returned a hardcoded `retry_after_secs: 2`, so this collected
    /// `{2}` and the distinct-value assertion failed. A single retry instant
    /// for the whole shed population IS the thundering herd.
    #[test]
    fn ceiling_shed_retry_after_is_jittered() {
        let s = fresh_state();
        s.set_contribution_max_peer_inflight(0);
        let hints: Vec<u64> = (0..32)
            .map(|i| {
                s.admit_peer_request(nid(i))
                    .expect_err("ceiling 0 sheds everything")
                    .retry_after_secs
            })
            .collect();
        let distinct: std::collections::BTreeSet<u64> = hints.iter().copied().collect();
        assert!(
            distinct.len() >= 3,
            "a shed hint with no spread is a synchronized-retry generator; got {distinct:?}"
        );
        // Bounded: the hint must stay inside [base, base + spread) so a
        // client is never told to sleep for an unbounded time.
        for h in &hints {
            assert!(
                (2..2 + RETRY_AFTER_JITTER_SPREAD_SECS).contains(h),
                "hint {h} escaped [2, {}) ",
                2 + RETRY_AFTER_JITTER_SPREAD_SECS
            );
        }
    }

    /// The spread policy itself, independent of the entropy source.
    #[test]
    fn jitter_is_bounded_and_covers_the_window() {
        let seen: std::collections::BTreeSet<u64> =
            (0..64).map(|e| jitter_retry_after(2, e)).collect();
        assert_eq!(
            seen,
            (2..2 + RETRY_AFTER_JITTER_SPREAD_SECS).collect(),
            "every offset in the window must be reachable, and none outside it"
        );
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

    /// The published `posture()` names the gate that is refusing — one arm per
    /// refusal `admit` can return, in the order it checks them. A posture with
    /// no arm exercised is a query nobody can trust.
    #[test]
    fn posture_names_the_active_gate() {
        let s = fresh_state();
        assert_eq!(s.posture(), AdmissionPosture::Open);

        // Ceiling: a zero budget sheds every peer, and nothing else is set.
        let s = fresh_state();
        s.set_contribution_max_peer_inflight(0);
        assert_eq!(s.posture(), AdmissionPosture::Ceiling);

        // Foreground yield outranks the ceiling.
        let s = fresh_state();
        s.set_yield_window_secs(60);
        s.bump_foreground_active();
        assert_eq!(s.posture(), AdmissionPosture::ForegroundYield);

        // Pause outranks both.
        let s = fresh_state();
        s.set_yield_window_secs(60);
        s.bump_foreground_active();
        s.set_contribution_paused_until(unix_now() + 60);
        assert_eq!(s.posture(), AdmissionPosture::Paused);
    }

    // ── The advertised number matches the enforced decision ──────
    //
    // These four pin the availability composite. The defect they
    // guard (note 3234d770): a node refusing 100% of peer requests
    // with `yielded_to_local` gossiped `availability: 1.0` for as
    // long as it kept refusing, because nothing but sovereign-
    // server's ActivityReporter ever wrote the field. Every one of
    // them fails against the pre-2026-08-14 plain setter.

    #[tokio::test]
    async fn yielding_node_advertises_zero_availability() {
        let s = fresh_state();
        s.set_yield_window_secs(60);
        s.bump_foreground_active();
        // Same state that makes the peer decision refuse...
        assert!(matches!(
            s.admit_peer_request(nid(1)).unwrap_err().reason,
            AdmissionReason::YieldedToLocal
        ));
        // ...must be the state we advertise.
        assert_eq!(s.recompute_local_availability().await, 0.0);
        assert_eq!(s.local_availability_published().await, 0.0);
    }

    #[tokio::test]
    async fn idle_node_advertises_full_availability() {
        let s = fresh_state();
        s.set_yield_window_secs(60);
        // No foreground request has ever landed: not yielding.
        assert!(s.admit_peer_request(nid(1)).is_ok());
        assert_eq!(s.recompute_local_availability().await, 1.0);
    }

    /// The clobber guard. An "idle" activity report arriving mid-yield
    /// must not be able to advertise 1.0 while this node is refusing
    /// every peer request — that is the original bug, re-entering
    /// through the other input.
    #[tokio::test]
    async fn activity_report_cannot_erase_a_live_yield() {
        let s = fresh_state();
        s.set_yield_window_secs(60);
        s.bump_foreground_active();
        // ActivityReporter says "idle" — the coding watcher sees no
        // edits. The yield window is still open regardless.
        s.update_local_availability(1.00).await;
        assert_eq!(
            s.local_availability_published().await,
            0.0,
            "an idle activity report erased a live yield window"
        );
        // And the activity input survives underneath: when the yield
        // window closes, availability returns to the reported level
        // rather than to a remembered 0.0.
        s.set_yield_window_secs(0);
        assert_eq!(s.recompute_local_availability().await, 1.00);
    }

    /// The composite is a MINIMUM, not a last-writer. A busy coding
    /// node that is also yielding advertises the tighter of the two.
    #[tokio::test]
    async fn composite_takes_the_tighter_ceiling() {
        let s = fresh_state();
        // "hot" — the ActivityReporter's busiest level.
        s.update_local_availability(0.20).await;
        assert_eq!(s.local_availability_published().await, 0.20);
        // Now the local user is at the keyboard too.
        s.set_yield_window_secs(60);
        s.bump_foreground_active();
        assert_eq!(s.recompute_local_availability().await, 0.20_f32.min(0.0));
        // Yield lifts; the activity ceiling is still in force.
        s.set_yield_window_secs(0);
        assert_eq!(s.recompute_local_availability().await, 0.20);
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
            axum::middleware::from_fn_with_state(state.clone(), peer_admission_layer::<AppState>),
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

    // ── The two gates are DISJOINT (FE-100) ─────────────────────────

    /// Both layers on one route, stacked the way `client_router_for` stacks
    /// them (`.layer(admission()).layer(fair_share())` — server.rs:134-135).
    fn both_gates_router(state: AppState) -> Router {
        Router::new()
            .route("/chat", post(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                peer_admission_layer::<AppState>,
            ))
            .layer(axum::middleware::from_fn_with_state(
                state,
                client_fairness_layer::<AppState>,
            ))
    }

    /// covers: FE-100
    ///
    /// One request meets exactly ONE gate. Each layer returns early when the
    /// other applies — the peer layer on a request with no `X-Node-Id`, the
    /// client layer on a request that has one — and the two early-returns are
    /// exact negations of each other. Nothing held them together: they live
    /// ~90 lines apart and every existing test drives one layer alone, so a
    /// change to either condition is invisible until traffic is charged twice
    /// or not at all.
    ///
    /// Asserted through the ceilings rather than through counters, because a
    /// double-gate is only harmful where it costs a slot. Each half sets ONE
    /// gate's budget to its floor, saturates it with the OTHER kind of
    /// traffic, and requires the gate's own kind to still get through.
    #[tokio::test]
    async fn peer_traffic_and_client_traffic_never_consume_each_others_budget() {
        // ── Peer traffic must not eat the client fair share ──────────
        let s = fresh_state();
        s.set_client_fair_concurrency(1);
        s.set_client_fairness_enabled(true);
        s.set_contribution_max_peer_inflight(8);
        let router = both_gates_router(s.clone());

        // A peer holds a turn open (the response body is alive, so both
        // gates' guards — if it took one from each — are still held).
        let mut req = peer_req("/chat");
        req.headers_mut()
            .insert("x-node-id", nid(0xBEEF).to_hex().parse().unwrap());
        let peer_held = router.clone().oneshot(req).await.expect("peer admitted");
        assert_eq!(peer_held.status(), axum::http::StatusCode::OK);

        // Instrument check (§18.4): the peer gate really did charge for it.
        assert_eq!(
            tally_of(&s, nid(0xBEEF)).active,
            1,
            "the peer gate must have counted the peer turn"
        );
        // And the client scheduler is untouched — nothing is in flight there.
        assert_eq!(
            s.lock_client_sched()
                .active_keys_including(&Principal::Anonymous),
            1,
            "a peer turn must not appear as an active client principal — the only \
             active key here is the probe key itself"
        );

        // The client budget is ONE. If the peer turn had also consumed a
        // client share slot, this would be shed.
        let client = turn_as(&router, "ailsa").await;
        assert_eq!(
            client.status(),
            axum::http::StatusCode::OK,
            "a peer turn must not spend the client fair-share budget"
        );
        drop(client);
        drop(peer_held);

        // ── And client traffic must not eat the peer ceiling ─────────
        //
        // The peer scheduler slot releases at HEADERS time (only the /status
        // tally follows the body), so a completed `oneshot` cannot hold the
        // ceiling. The slot is taken directly instead, which is both
        // deterministic and closer to the real shape: a peer turn that is
        // genuinely still decoding.
        let s = fresh_state();
        s.set_client_fair_concurrency(16);
        s.set_client_fairness_enabled(true);
        s.set_contribution_max_peer_inflight(1);
        let router = both_gates_router(s.clone());
        let _peer_slot = s
            .admit_peer_request(nid(0xCAFE))
            .expect("the one peer slot");

        // The control: with that slot held, a peer request IS shed. Without
        // this the assertion below could pass against a ceiling that never
        // bites.
        let mut req = peer_req("/chat");
        req.headers_mut()
            .insert("x-node-id", nid(0xD00D).to_hex().parse().unwrap());
        let second_peer = router
            .clone()
            .oneshot(req)
            .await
            .expect("gate must respond");
        assert_eq!(
            second_peer.status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "the peer ceiling of 1 must actually refuse a second peer"
        );

        // And a CLIENT turn, at the same moment, is unaffected — it never
        // consults the peer ceiling.
        let client = turn_as(&router, "rhona").await;
        assert_eq!(
            client.status(),
            axum::http::StatusCode::OK,
            "the peer ceiling must not gate a local caller"
        );
        assert!(
            s.inner.peer_tally_snapshot().is_empty(),
            "a local client turn must never open a peer tally row"
        );
        drop(client);
    }

    // ── Client fair share (order `serve50-identity`) ────────────────
    //
    // These drive the LAYER, not the policy — the policy's own assertions
    // live next to `fair_share_cap` in serving-policy. What is tested here is
    // the wiring §9.3 measured as absent: that the principal on the wire
    // reaches the scheduler and changes what the node does.

    fn fair_share_router(state: AppState) -> Router {
        Router::new().route("/chat", post(|| async { "ok" })).layer(
            axum::middleware::from_fn_with_state(state.clone(), client_fairness_layer::<AppState>),
        )
    }

    /// One request as principal `who` (a distinct bearer per caller — the
    /// exact wire shape `probe_a_greedy_vs_polite.py --identity-mode
    /// principal` sends). The response is RETURNED, not dropped, so the
    /// caller can hold turns in flight.
    async fn turn_as(router: &Router, who: &str) -> Response {
        let mut req = peer_req("/chat");
        req.headers_mut().insert(
            "authorization",
            format!("Bearer tok-{who}").parse().unwrap(),
        );
        router
            .clone()
            .oneshot(req)
            .await
            .expect("gate must respond")
    }

    #[tokio::test]
    async fn client_gate_holds_a_greedy_principal_to_its_equal_share() {
        // THE red, as a test. Nine polite principals each hold one turn; the
        // tenth keeps firing. Before this gate every one of the greedy
        // caller's requests was admitted and it took 79.5% of the turns.
        let s = fresh_state();
        s.set_client_fair_concurrency(16);
        s.set_client_fairness_enabled(true);
        let router = fair_share_router(s.clone());

        let mut held = Vec::new();
        for i in 0..9 {
            let r = turn_as(&router, &format!("polite-{i}")).await;
            assert_eq!(r.status(), axum::http::StatusCode::OK);
            held.push(r);
        }
        // Ten principals over a budget of 16 → an equal share of one turn.
        let first = turn_as(&router, "greedy").await;
        assert_eq!(
            first.status(),
            axum::http::StatusCode::OK,
            "the greedy caller is entitled to its share, and must get it"
        );
        held.push(first);

        for attempt in 0..32 {
            let r = turn_as(&router, "greedy").await;
            assert_eq!(
                r.status(),
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "attempt {attempt} exceeded the greedy caller's equal share"
            );
            // Backpressure, not a fault: the refusal must carry a hint the
            // client can act on, exactly like every other shed.
            assert!(
                r.headers().contains_key(RETRY_AFTER),
                "a refusal without Retry-After reads as a crash, not as busy"
            );
        }
        assert_eq!(
            s.client_inflight_count(),
            10,
            "ten principals, ten turns — not 10 + 32"
        );
    }

    #[tokio::test]
    async fn client_gate_leaves_a_lone_principal_alone() {
        // The no-regression arm, structural: with nobody else active the cap
        // is the `u32::MAX` "not rationing" sentinel, so a single caller's
        // concurrency is untouched by this layer existing.
        let s = fresh_state();
        s.set_client_fair_concurrency(16);
        s.set_client_fairness_enabled(true);
        let router = fair_share_router(s.clone());
        let mut held = Vec::new();
        for _ in 0..32 {
            let r = turn_as(&router, "solo").await;
            assert_eq!(
                r.status(),
                axum::http::StatusCode::OK,
                "a lone caller must never be throttled by a FAIRNESS rule"
            );
            held.push(r);
        }
        assert_eq!(s.client_inflight_count(), 32);
    }

    #[tokio::test]
    async fn client_gate_releases_the_share_when_the_response_body_drops() {
        // The share must span the BODY, not headers time: a streamed turn
        // still owns the decode permit after its headers have gone out.
        let s = fresh_state();
        s.set_client_fair_concurrency(16);
        s.set_client_fairness_enabled(true);
        let router = fair_share_router(s.clone());
        let other = turn_as(&router, "other").await; // a second active principal
        let mine = turn_as(&router, "mine").await;
        assert_eq!(mine.status(), axum::http::StatusCode::OK);
        let key = Principal::RemoteClient {
            credential: {
                // Resolve through the ONE resolver rather than recomputing the
                // fingerprint here — two implementations of a key is the smell.
                let mut h = axum::http::HeaderMap::new();
                h.insert("authorization", "Bearer tok-mine".parse().unwrap());
                match <AppState as AdmissionHost>::resolve(&s, &h, None) {
                    Principal::RemoteClient { credential } => credential,
                    other => panic!("expected a credential principal, got {other:?}"),
                }
            },
        };
        assert_eq!(
            s.client_inflight_of(&key),
            1,
            "the handler returned but the body is alive — the share is held"
        );
        drop(mine);
        assert_eq!(
            s.client_inflight_of(&key),
            0,
            "dropping the body must return the share"
        );
        drop(other);
        assert_eq!(s.client_inflight_count(), 0);
    }

    #[tokio::test]
    async fn client_gate_never_touches_peer_requests() {
        // A request naming a node is the PEER gate's business. Double-gating
        // it would be the double-shed the order forbids, and would make the
        // §9.3 `distinct` arm worse rather than leaving it untouched.
        let s = fresh_state();
        s.set_client_fair_concurrency(1);
        s.set_client_fairness_enabled(true);
        let router = fair_share_router(s.clone());
        let mut held = Vec::new();
        for _ in 0..8 {
            let mut req = peer_req("/chat");
            req.headers_mut()
                .insert("x-node-id", nid(0xBEEF).to_hex().parse().unwrap());
            req.headers_mut()
                .insert("authorization", "Bearer whatever".parse().unwrap());
            let r = router.clone().oneshot(req).await.expect("must respond");
            assert_eq!(r.status(), axum::http::StatusCode::OK);
            held.push(r);
        }
        assert_eq!(
            s.client_inflight_count(),
            0,
            "peer traffic must not even be accounted for on the client gate"
        );
    }

    #[tokio::test]
    async fn client_gate_kill_switch_reproduces_the_unfair_behaviour() {
        // A gate you have not watched fail is not a gate (§18.1). Flipping
        // the switch off must restore the red on the SAME binary — which is
        // also what makes the probe's A/B one env var instead of two builds.
        let s = fresh_state();
        s.set_client_fair_concurrency(16);
        s.set_client_fairness_enabled(false);
        let router = fair_share_router(s.clone());
        let mut held = Vec::new();
        for i in 0..9 {
            held.push(turn_as(&router, &format!("polite-{i}")).await);
        }
        for _ in 0..32 {
            let r = turn_as(&router, "greedy").await;
            assert_eq!(
                r.status(),
                axum::http::StatusCode::OK,
                "with the gate off, the greedy caller takes everything — the red"
            );
            held.push(r);
        }
        assert_eq!(s.client_inflight_count(), 41, "9 polite + 32 greedy");
    }

    #[tokio::test]
    async fn client_gate_buckets_unidentified_callers_together() {
        // Callers presenting nothing share one bucket — which is what they
        // are today, so this is the no-change branch. It must still be a
        // bucket, though: otherwise "present no header" would be a bypass,
        // the exact footgun `client_auth` killed at its own layer.
        let s = fresh_state();
        // Budget 2 over the 2 principals below → an equal share of one turn
        // each. (At the default 16 the share would be 8, and this test would
        // be asserting the cap's SIZE rather than that the anonymous bucket
        // is subject to it at all.)
        s.set_client_fair_concurrency(2);
        s.set_client_fairness_enabled(true);
        let router = fair_share_router(s.clone());
        let named = turn_as(&router, "named").await;
        assert_eq!(named.status(), axum::http::StatusCode::OK);
        let first = router
            .clone()
            .oneshot(peer_req("/chat"))
            .await
            .expect("must respond");
        assert_eq!(first.status(), axum::http::StatusCode::OK);
        let second = router
            .clone()
            .oneshot(peer_req("/chat"))
            .await
            .expect("must respond");
        assert_eq!(
            second.status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "omitting identity must not buy a second share"
        );
        drop((named, first, second));
    }

    /// covers: FE-99
    ///
    /// "A present-but-malformed value MUST still be gated and tallied, and the
    /// status surface MUST name the rejected raw value." Both halves: the
    /// request buckets under the ZERO node rather than bypassing the ceiling,
    /// and the raw value is recorded with its timestamp so /status can name it
    /// instead of showing an opaque `node-0000000000000000` row (ARCH §18.3 —
    /// absence is reported, never defaulted).
    #[tokio::test]
    async fn middleware_malformed_header_buckets_zero_and_is_named() {
        // Fix 7: a present-but-malformed X-Node-Id must (a) still be gated
        // and tallied — under the ZERO node, never bypassing the ceiling —
        // and (b) record the rejected raw value so /status can name it.
        let s = fresh_state();
        let router = tally_test_router(s.clone());
        let mut req = peer_req("/chat");
        req.headers_mut()
            .insert("x-node-id", "not-a-node-id!!".parse().unwrap());
        let resp = router
            .clone()
            .oneshot(req)
            .await
            .expect("malformed header must still be admitted (zero bucket)");
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        drop(resp);
        assert_eq!(
            tally_of(&s, NodeId::from_u128(0)).served_total,
            1,
            "the malformed request must tally under the zero node"
        );
        let rejected = s
            .inner
            .last_rejected_x_node_id()
            .expect("the rejected value must be recorded");
        assert_eq!(rejected.raw, "not-a-node-id!!");
        assert!(rejected.at_unix > 0);
    }
}
