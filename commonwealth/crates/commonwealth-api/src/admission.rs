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
    /// The calling principal already holds its equal share of the
    /// host's concurrency while other principals are active. Distinct
    /// from every reason above: it says nothing about how busy the
    /// host is, only that THIS caller is ahead of its neighbours.
    /// A host with idle capacity still returns this — that is the
    /// point, and it is why it is not a shed
    /// (`MESH_SCALE_100_USERS_1000_CORPORA.md` §7.1 R2).
    PrincipalShareExceeded,
}

/// How many seconds of spread a shed's `Retry-After` hint carries on
/// top of its base value.
///
/// WHY THIS IS NOT ZERO. A constant hint is a synchronized-retry
/// generator: every client shed inside the same busy window is told to
/// come back at the same instant, so the load that produced the shed
/// re-arrives as a single spike instead of a ramp — and the spike sheds
/// the same population again, in lockstep, forever. This is the
/// classic thundering-herd retry loop, and at 100 clients against one
/// concurrent turn (`MESH_SCALE_100_USERS_1000_CORPORA.md` §7.4 item 2)
/// it is the difference between a queue that drains and one that
/// oscillates. Four seconds on a 2s base spreads the herd over 3× the
/// base window while keeping the worst-case hint inside the range a
/// client's own backoff would have chosen anyway.
pub const RETRY_AFTER_JITTER_SPREAD_SECS: u64 = 4;

/// The jitter function itself, pure and therefore testable: `base`
/// plus `entropy mod spread`. Split out from the entropy SOURCE so the
/// spread policy has exactly one implementation and one name (§10.6)
/// no matter which shed path renders the hint.
fn jitter_retry_after(base: u64, entropy: u64) -> u64 {
    base.saturating_add(entropy % RETRY_AFTER_JITTER_SPREAD_SECS)
}

/// Production entry point: `base` seconds, jittered.
///
/// Entropy is a process-local counter mixed with the wall clock's
/// nanosecond field. The counter guarantees that two sheds from the
/// SAME process never land on the same offset back-to-back (which a
/// coarse clock would otherwise allow); the nanoseconds guarantee that
/// two processes shedding in the same instant do not share a phase.
/// Neither alone is sufficient, which is why both are mixed.
pub fn jittered_retry_after_secs(base: u64) -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::from(d.subsec_nanos()))
        .unwrap_or(0);
    // splitmix64 finalizer — cheap avalanche so the low bits of the
    // counter/nanos mix don't hand out a sawtooth.
    let mut z = counter
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(nanos);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    let entropy = z ^ (z >> 31);
    jitter_retry_after(base, entropy)
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

/// Response-body wrapper that holds an RAII guard for the whole streaming
/// lifetime of the body, so a counter opened at admit time closes when the
/// body is consumed, dropped, or the client disconnects — not merely when the
/// handler returned.
///
/// This is the one place the "serving right now" window is defined. Two
/// guards ride it, for the same reason and by the same rule:
///
/// - [`TallyGuard`] — `/status`'s per-peer `active` counter (UC-R1).
/// - [`ClientShareGuard`] — the per-principal fair-share slot. Holding it to
///   headers time would be wrong on a streamed turn: headers leave as soon as
///   the first token is ready, while the decode permit is still held, so a
///   greedy principal would be handed its next share before the current turn
///   had actually finished.
///
/// Generic rather than duplicated: the wrapper is pure plumbing, and two
/// copies of it would be two implementations of one rule (§10.6).
pub struct GuardedBody<G> {
    inner: axum::body::Body,
    _guard: G,
}

impl<G> GuardedBody<G> {
    pub(crate) fn new(inner: axum::body::Body, guard: G) -> Self {
        Self {
            inner,
            _guard: guard,
        }
    }
}

impl<G: Unpin> http_body::Body for GuardedBody<G> {
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

/// The per-peer tally's body wrapper — the original and still the name the
/// peer admission path uses.
pub type TallyBody = GuardedBody<TallyGuard>;

// ── Client fair-share admission (order `serve50-identity`) ─────────────────

/// Concurrency the host can carry before the inference slot queue starts
/// shedding — the numerator
/// [`commonwealth_core::fair_sched::fair_share_cap`] divides among active
/// principals.
///
/// **Derived, not picked.** The slot queue sheds when the predicted wait
/// exceeds `DEFAULT_MAX_QUEUE_WAIT_MS = 30_000`
/// (`sovereign-inference/src/embedded/model_slot.rs:862`) and predicts
/// `position × avg_turn_ms` against one decode permit. Sixteen is that bound
/// at a ~1.9 s turn: the depth at which the host is fully committed but not
/// yet refusing. Sizing it *there* is what keeps this cap from becoming a
/// second shed rule — at or below this concurrency the slot queue serves
/// everyone, so the only thing the cap changes is WHOSE turns fill it.
///
/// Two consequences worth holding:
/// - Too high, and a greedy principal's share is too generous to matter.
/// - Too low, and a lone caller would be throttled below what the host can
///   actually serve — which is why the `active <= 1` branch of
///   `fair_share_cap` bypasses this number entirely.
pub const DEFAULT_CLIENT_FAIR_CONCURRENCY: u32 = 16;

/// Read the fair-share budget from `SOVEREIGN_CLIENT_FAIR_CONCURRENCY`.
/// A malformed or zero value is REPORTED and falls back to the default —
/// never silently accepted, since a zero budget would floor every cap at 1
/// and quietly turn a rationing rule into a serialization rule.
pub fn client_fair_concurrency_from_env() -> u32 {
    match std::env::var("SOVEREIGN_CLIENT_FAIR_CONCURRENCY") {
        Err(_) => DEFAULT_CLIENT_FAIR_CONCURRENCY,
        Ok(v) => match v.trim().parse::<u32>() {
            Ok(n) if n > 0 => n,
            _ => {
                tracing::warn!(
                    value = %v,
                    default = DEFAULT_CLIENT_FAIR_CONCURRENCY,
                    "SOVEREIGN_CLIENT_FAIR_CONCURRENCY is not a positive number — using the default"
                );
                DEFAULT_CLIENT_FAIR_CONCURRENCY
            }
        },
    }
}

/// Read the kill switch from `SOVEREIGN_CLIENT_FAIRNESS`. Default **on**.
/// `0`/`false`/`off`/`no` disable enforcement; the gate still resolves the
/// principal and logs it, so the A/B is one env var on one binary rather than
/// two builds. It restores the unfair BEHAVIOUR, not the old BINARY — the
/// observe-only path still takes and releases the accounting slot and still
/// wraps the response body, which is measurable under load
/// (`MESH_SCALE_100_USERS_1000_CORPORA.md` §9.5).
pub fn client_fairness_enabled_from_env() -> bool {
    match std::env::var("SOVEREIGN_CLIENT_FAIRNESS") {
        Err(_) => true,
        Ok(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
    }
}

/// RAII guard holding one principal's fair-share slot. Released on drop,
/// which — because it rides [`GuardedBody`] — is when the response BODY ends,
/// not when the handler returned.
#[must_use = "drop the guard when the client turn's body ends — the principal's \
              share only frees on drop"]
pub struct ClientShareGuard {
    inner: Arc<AppStateInner>,
    key: crate::principal::PrincipalKey,
}

impl ClientShareGuard {
    fn new(inner: Arc<AppStateInner>, key: crate::principal::PrincipalKey) -> Self {
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

/// Axum middleware: per-principal fair share on the CLIENT surface.
///
/// The §9.3 red in one sentence: ten callers with ten credentials were served
/// strictly by arrival order, so the one keeping 32 requests in flight took
/// 79.5% of the turns against a 10% population share. This layer is the
/// missing consult — it resolves the principal ([`crate::principal`], the one
/// resolver, called here and nowhere else) and asks the shared `SchedCore`
/// whether that principal is already holding its equal share.
///
/// **What this layer is not.** It is not a shed. It never inspects the queue,
/// the host's load, or a predicted wait; those belong to the inference slot
/// queue, which stays THE shed decider (§7.1 R2). It never queues either —
/// `try_grant` leaves no waiter behind, so a refused caller cannot park and
/// there is no second queue to double-count against. And it never ranks: the
/// weight passed to the core is a constant `1.0`, because weight-ordering is
/// condemned (`SCHEDULER_QUALITY.md` F6) and the fix §9.3 asks for is *equal*
/// share, not *ranked* share.
///
/// **Peer traffic passes straight through.** A request carrying `X-Node-Id`
/// is already rationed per node by [`peer_admission_layer`]. Gating it here
/// too would be exactly the double-gate the order forbids, and would make the
/// `distinct` arm of the §9.3 harness worse rather than leaving it untouched.
pub async fn client_fairness_layer(
    State(state): State<AppState>,
    headers: HeaderMap,
    req: Request<Body>,
    next: Next,
) -> Response {
    // Peer requests are the peer gate's business. One decider each.
    if headers.get("x-node-id").is_some() {
        return next.run(req).await;
    }

    let peer_addr = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0);
    // THE call site. Resolving anywhere else would be a second identity.
    let resolved = crate::principal::resolve_principal(&headers, peer_addr);

    let enforcing = state.client_fairness_enabled();
    let budget = state.client_fair_concurrency();

    let (outcome, cap, active, inflight) = {
        let mut sched = state.lock_client_sched();
        let active = sched.active_keys_including(&resolved.key);
        let cap = commonwealth_core::fair_sched::fair_share_cap(budget, active);
        let inflight = sched.inflight_of(&resolved.key);
        // Weight is a constant: see the "never ranks" note above.
        let outcome = if enforcing {
            sched.try_grant(resolved.key.clone(), 1.0, cap)
        } else {
            // Observe-only: still take the slot so the accounting (and the
            // `active` denominator) is identical to the enforcing path —
            // otherwise the A/B would compare two different measurements.
            sched.try_grant(resolved.key.clone(), 1.0, u32::MAX)
        };
        (outcome, cap, active, inflight)
    };

    // Glassbox: EVERY admission decision names the principal, how it was
    // identified, the share it was measured against, and what was decided.
    // `target: "admission"` is a custom target — it is dark unless the
    // tracing filter lists it (see `quality/env-flags.toml`).
    let granted = matches!(outcome, commonwealth_core::fair_sched::TryGrant::Granted);
    tracing::debug!(
        target: "admission",
        principal = %resolved.key,
        identified_by = resolved.source.as_str(),
        active_principals = active,
        fair_share_cap = cap,
        principal_inflight = inflight,
        budget,
        enforcing,
        decision = if granted { "admit" } else { "over-share" },
        "admission.client: fair-share decision"
    );

    if granted {
        let guard = ClientShareGuard::new(Arc::clone(&state.inner), resolved.key);
        let response = next.run(req).await;
        // The share is held for the BODY's lifetime, not headers time — a
        // streamed turn still owns the decode permit after its headers go out.
        return response.map(|body| Body::new(GuardedBody::new(body, guard)));
    }

    // Over its share. This is backpressure with a hint, rendered through the
    // one shed renderer so a client cannot tell it apart from any other
    // `Retry-After` refusal it already handles.
    let retry_after_secs = jittered_retry_after_secs(1);
    tracing::info!(
        target: "admission",
        principal = %resolved.key,
        fair_share_cap = cap,
        active_principals = active,
        retry_after_secs,
        "admission.client: 503 — principal is over its equal share"
    );
    shed_response(AdmissionRejection {
        error: format!(
            "over fair share: this caller holds {inflight} of {cap} concurrent turns \
             while {active} principals are active"
        ),
        reason: AdmissionReason::PrincipalShareExceeded,
        retry_after_secs,
    })
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
    // never silently bypasses the ceiling. The rejected raw value is
    // recorded so /status can NAME it on the zero-bucket row (order
    // commons-fluency fix 7) — an opaque `node-0000000000000000` row would
    // default the absence instead of reporting it (ARCH §18.3).
    let node = match crate::headers::parse_x_node_id(&headers) {
        Some(node) => node,
        None => {
            let raw = headers
                .get("x-node-id")
                .or_else(|| headers.get("X-Node-Id"))
                .and_then(|v| v.to_str().ok())
                .unwrap_or("<unreadable header>");
            state.inner.record_rejected_x_node_id(raw);
            NodeId::from_u128(0)
        }
    };
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

    /// RED-FIRST (order mesh-scale-t0, item 2). Before the fix,
    /// `admit_peer_request` returned a hardcoded `retry_after_secs: 2`
    /// on every ceiling shed, so this collected `{2}` and the
    /// distinct-value assertion failed. A single retry instant for the
    /// whole shed population IS the thundering herd.
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

    // ── Client fair share (order `serve50-identity`) ────────────────
    //
    // These drive the LAYER, not the policy — the policy's own assertions
    // live next to `fair_share_cap` in commonwealth-core. What is tested
    // here is the wiring §9.3 measured as absent: that the principal on the
    // wire reaches the scheduler and changes what the node does.

    fn fair_share_router(state: AppState) -> Router {
        Router::new().route("/chat", post(|| async { "ok" })).layer(
            axum::middleware::from_fn_with_state(state.clone(), client_fairness_layer),
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
        let key = crate::principal::PrincipalKey::Credential({
            // Resolve through the ONE resolver rather than recomputing the
            // fingerprint here — two implementations of a key is the smell.
            let mut h = axum::http::HeaderMap::new();
            h.insert("authorization", "Bearer tok-mine".parse().unwrap());
            match crate::principal::resolve_principal(&h, None).key {
                crate::principal::PrincipalKey::Credential(fp) => fp,
                other => panic!("expected a credential key, got {other:?}"),
            }
        });
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

    #[test]
    fn fair_concurrency_env_reports_a_bad_value_instead_of_accepting_it() {
        // A zero budget would floor every cap at 1 and silently convert a
        // rationing rule into a serialization rule — absence reported, never
        // defaulted (§18.3).
        let restore = std::env::var("SOVEREIGN_CLIENT_FAIR_CONCURRENCY").ok();
        for bad in ["0", "banana", ""] {
            std::env::set_var("SOVEREIGN_CLIENT_FAIR_CONCURRENCY", bad);
            assert_eq!(
                client_fair_concurrency_from_env(),
                DEFAULT_CLIENT_FAIR_CONCURRENCY,
                "{bad:?} must fall back to the default, loudly"
            );
        }
        std::env::set_var("SOVEREIGN_CLIENT_FAIR_CONCURRENCY", "24");
        assert_eq!(client_fair_concurrency_from_env(), 24);
        std::env::remove_var("SOVEREIGN_CLIENT_FAIR_CONCURRENCY");
        assert_eq!(
            client_fair_concurrency_from_env(),
            DEFAULT_CLIENT_FAIR_CONCURRENCY
        );
        if let Some(v) = restore {
            std::env::set_var("SOVEREIGN_CLIENT_FAIR_CONCURRENCY", v);
        }
    }

    #[test]
    fn fairness_defaults_on_and_the_kill_switch_is_explicit() {
        let restore = std::env::var("SOVEREIGN_CLIENT_FAIRNESS").ok();
        std::env::remove_var("SOVEREIGN_CLIENT_FAIRNESS");
        assert!(
            client_fairness_enabled_from_env(),
            "fairness ships on; the flag is a kill switch, not an opt-in"
        );
        for off in ["0", "false", "off", "NO", " off "] {
            std::env::set_var("SOVEREIGN_CLIENT_FAIRNESS", off);
            assert!(!client_fairness_enabled_from_env(), "{off:?} must disable");
        }
        for on in ["1", "true", "on", "anything-else"] {
            std::env::set_var("SOVEREIGN_CLIENT_FAIRNESS", on);
            assert!(client_fairness_enabled_from_env(), "{on:?} must stay on");
        }
        std::env::remove_var("SOVEREIGN_CLIENT_FAIRNESS");
        if let Some(v) = restore {
            std::env::set_var("SOVEREIGN_CLIENT_FAIRNESS", v);
        }
    }

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
