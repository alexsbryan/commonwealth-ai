// SPDX-License-Identifier: AGPL-3.0-or-later
//! Admission — the decision, separated from the middleware.
//!
//! `sovereign/SERVING_BOUNDARY.md` "The five entries" (c): the decider is the
//! package's published surface, over `serving-policy`'s `SchedCore`; the two
//! axum middlewares are the host's thin adapters. This module holds the first
//! and, because the daemon still owns `AppState`, the second as generic
//! adapters over [`AdmissionHost`].
//!
//! # What is published, and what is a port
//!
//! - [`Admission`] is the published entry. `admit(&Principal, now_unix_ms)` is
//!   the decision: it reads no clock (the caller passes `now`), names no axum
//!   type and holds no `AppState`. It reserves on `Admitted` and the returned
//!   [`AdmissionLease`] releases on drop, which is the one shape both the peer
//!   slot and the client fair-share slot fit.
//! - [`AdmissionHost`] is the daemon-state port the two middlewares need and
//!   the decision does not: the edge resolver, the peer tally row, and the
//!   recording of a malformed `X-Node-Id`. The daemon implements it over
//!   `AppState`. The edge authenticates a request once and attaches the
//!   resolved [`Principal`] as an [`AttachedPrincipal`] extension
//!   (`DAEMON_CORE.md` §3.3, "one resolution at the edge, one value"); both
//!   middlewares read it, and [`AdmissionHost::resolve`] is the fallback for
//!   the internal router, which carries no edge layer.
//!
//! # The two identities are one key
//!
//! A peer request resolves to [`Principal::Member`] by its verified node id; a
//! client request to [`Principal::LocalOwner`], [`Principal::RemoteClient`],
//! [`Principal::Guest`] or [`Principal::Anonymous`]. `admit` dispatches on the
//! arm, so the peer ceiling and the client fair share are two branches of one
//! decision over one key — never two parallel identity schemes (ARCH
//! principle 8).

use std::net::SocketAddr;

use axum::{
    body::Body,
    extract::{ConnectInfo, State},
    http::{header::RETRY_AFTER, HeaderMap, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use kernel_types::NodeId;
use oicp_types::openai_types::ErrorDetail;
use serde::Serialize;
// Re-exported: `Principal` is part of this surface — `AdmissionHost::resolve`
// returns it and `Admission::admit` keys on it — and the daemon reaching the
// type through the module that publishes it is what keeps the direct
// `sovereign-contracts` fan-in from growing (layer-gate's ratchet).
pub use sovereign_contracts::principal::Principal;

/// The edge-resolved [`Principal`], attached to the request by the daemon's
/// `client_auth_layer` and read by the two admission middlewares.
///
/// One resolution per request (`DAEMON_CORE.md` §3.3, "authenticates a request
/// once and attaches a `Principal`"): the edge resolves the caller once and
/// puts the value here, so neither middleware answers "who is asking" a second
/// time. The internal router (`sovereign-api/src/server.rs`) carries no
/// `client_auth_layer`, so [`principal_of`] falls back to
/// [`AdmissionHost::resolve`] when the extension is absent.
#[derive(Clone)]
pub struct AttachedPrincipal(pub Principal);

/// Why a request was rejected. Serialised in the 503 body and in tracing
/// spans so contention triage doesn't require log spelunking.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionReason {
    /// Operator-initiated runtime pause is active.
    Paused,
    /// Foreground-yield window: local user has activity in flight, peer work
    /// would contend with their chat.
    YieldedToLocal,
    /// At-or-above the configured ceiling for concurrent peer requests.
    CeilingExceeded,
    /// This node's own slot refused BEFORE parking the caller: predicted wait
    /// exceeded the queue bound. Distinct from `CeilingExceeded`, which counts
    /// concurrent PEER requests — this one is about how long the caller would
    /// have waited in THIS node's queue, regardless of who sent the turn.
    LocalQueueFull,
    /// The calling principal already holds its equal share of the host's
    /// concurrency while other principals are active. Distinct from every
    /// reason above: it says nothing about how busy the host is, only that
    /// THIS caller is ahead of its neighbours. A host with idle capacity still
    /// returns this — that is the point, and it is why it is not a shed
    /// (`MESH_SCALE_100_USERS_1000_CORPORA.md` §7.1 R2).
    PrincipalShareExceeded,
}

impl AdmissionReason {
    /// The stable machine-readable string for this reason, used as the OpenAI
    /// `error.code` and as the top-level `reason`. Kept in sync with the
    /// `rename_all = "snake_case"` serde attribute above by the
    /// `admission_reason_code_matches_serde` test — two spellings of one name
    /// is the §10.6 smell, and this is the pair most likely to drift.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Paused => "paused",
            Self::YieldedToLocal => "yielded_to_local",
            Self::CeilingExceeded => "ceiling_exceeded",
            Self::LocalQueueFull => "local_queue_full",
            Self::PrincipalShareExceeded => "principal_share_exceeded",
        }
    }
}

/// How many seconds of spread a shed's `Retry-After` hint carries on top of its
/// base value.
///
/// WHY THIS IS NOT ZERO. A constant hint is a synchronized-retry generator:
/// every client shed inside the same busy window is told to come back at the
/// same instant, so the load that produced the shed re-arrives as a single
/// spike instead of a ramp — and the spike sheds the same population again, in
/// lockstep, forever. This is the classic thundering-herd retry loop, and at
/// 100 clients against one concurrent turn (`MESH_SCALE_100_USERS_1000_CORPORA.md`
/// §7.4 item 2) it is the difference between a queue that drains and one that
/// oscillates. Four seconds on a 2s base spreads the herd over 3× the base
/// window while keeping the worst-case hint inside the range a client's own
/// backoff would have chosen anyway.
pub const RETRY_AFTER_JITTER_SPREAD_SECS: u64 = 4;

/// The jitter function itself, pure and therefore testable: `base` plus
/// `entropy mod spread`. Split out from the entropy SOURCE so the spread policy
/// has exactly one implementation and one name (§10.6) no matter which shed
/// path renders the hint.
pub fn jitter_retry_after(base: u64, entropy: u64) -> u64 {
    base.saturating_add(entropy % RETRY_AFTER_JITTER_SPREAD_SECS)
}

/// Production entry point: `base` seconds, jittered.
///
/// Entropy is a process-local counter mixed with the wall clock's nanosecond
/// field. The counter guarantees that two sheds from the SAME process never
/// land on the same offset back-to-back (which a coarse clock would otherwise
/// allow); the nanoseconds guarantee that two processes shedding in the same
/// instant do not share a phase. Neither alone is sufficient, which is why both
/// are mixed.
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

/// 503 body the admission layer returns to a rejected caller.
/// `retry_after_secs` mirrors the `Retry-After` header value.
///
/// `error` is the OpenAI error OBJECT, not a bare string: this route is
/// advertised as OpenAI-compatible, so a shed serialised as a plain string put
/// the one message that has to survive ("busy, come back in 30s") out of reach
/// of every SDK on the route. Reuses [`ErrorDetail`] rather than minting a
/// second error shape (§10.6).
///
/// `reason` and `retry_after_secs` stay TOP-LEVEL and unchanged — the peer load
/// balancer and `deep_research`'s shed classifier key off them, the latter by
/// substring — so widening `error` is additive.
#[derive(Debug, Clone, Serialize)]
pub struct AdmissionRejection {
    /// OpenAI-shaped error object. `code` carries the same value as `reason` so
    /// a client that only understands the OpenAI envelope still gets the
    /// precise cause.
    pub error: ErrorDetail,
    pub reason: AdmissionReason,
    pub retry_after_secs: u64,
}

impl AdmissionRejection {
    /// Build a rejection from the human-readable cause. The OpenAI `type` is
    /// the coarse bucket (`server_error`) and `code` is the precise reason,
    /// which is the split the OpenAI error contract asks for.
    pub fn new(message: impl Into<String>, reason: AdmissionReason, retry_after_secs: u64) -> Self {
        Self {
            error: ErrorDetail {
                message: message.into(),
                error_type: "server_error".to_string(),
                code: Some(reason.as_str().to_string()),
            },
            reason,
            retry_after_secs,
        }
    }
}

/// The ONE place a shed becomes an HTTP response: 503 + `Retry-After` + the
/// structured body. Both the peer-admission middleware and the local queue-shed
/// path in `routes_inference` render through here.
///
/// Why this is a function rather than two call sites that each build a
/// response: a shed is backpressure, and a client that receives it as an
/// untyped `backend_error` cannot tell "busy, come back in 35s" from
/// "something crashed". That was note `bef03728`'s open gap, and the 2026-08-07
/// live fleet probe turned it into an observed failure — the caller got
/// `{"type":"backend_error"}` carrying its retry hint only inside a prose
/// message, with no `Retry-After` header.
///
/// A local queue shed, rendered. Both chat entry points (streaming and
/// non-streaming) call this so the body and header are built in exactly one
/// place rather than once per route.
pub fn local_queue_shed_response(
    position: u32,
    predicted_wait_ms: u64,
    retry_after_secs: u64,
) -> Response {
    shed_response(AdmissionRejection::new(
        format!("host busy: ~{predicted_wait_ms} ms predicted wait at queue position {position}"),
        AdmissionReason::LocalQueueFull,
        retry_after_secs,
    ))
}

/// Render a rejection as 503 + `Retry-After` + the structured body.
pub fn shed_response(rejection: AdmissionRejection) -> Response {
    let retry_after = rejection.retry_after_secs;
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(RETRY_AFTER, retry_after.to_string())],
        Json(rejection),
    )
        .into_response()
}

/// The host's posture, for `/status` and operator triage.
///
/// `Open` is the only arm that admits every caller; each other arm names the
/// gate that is currently refusing. The arms are the refusals `admit` can
/// return, in the order it checks them (ARCH principle 8: one spelling of the
/// order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionPosture {
    /// Nothing is refusing: peers are admitted up to their ceiling and every
    /// client is within its fair share.
    Open,
    /// Operator-initiated contribution pause is active.
    Paused,
    /// The local user is active; peer work is yielding to their chat.
    ForegroundYield,
    /// The peer concurrency ceiling is reached.
    Ceiling,
}

/// A held admission slot. The concrete lease releases its reservation on drop;
/// the trait is the box the middleware carries, not a place for methods.
pub trait AdmissionLease: Send + Sync {}

/// The decision `admit` returns.
#[must_use = "an Admitted verdict holds a lease — dropping it releases the slot"]
pub enum AdmissionVerdict {
    /// Admitted. The lease releases the reservation when dropped: at headers
    /// time for a peer slot, at body end for a client fair-share slot.
    Admitted(Box<dyn AdmissionLease>),
    /// Refused, with the rendered reason.
    Rejected(AdmissionRejection),
}

/// The published admission entry (`SERVING_BOUNDARY.md` (c)).
///
/// Pure over the snapshot: no axum, no `AppState`, no clock read — `now` is an
/// argument so a simulation can run on virtual time. The decision reserves on
/// [`AdmissionVerdict::Admitted`]; the lease it returns releases on drop.
pub trait Admission: Send + Sync {
    /// Decide whether `who` may be served at `now_unix_ms`.
    fn admit(&self, who: &Principal, now_unix_ms: u64) -> AdmissionVerdict;

    /// The current posture, for `/status`.
    fn posture(&self) -> AdmissionPosture;
}

/// What the middlewares need from the daemon that owns the state.
///
/// A supertrait of [`Admission`] so the adapters take one `State<S>`. Each
/// method is a fact the daemon owns and the package may not name: the edge
/// resolver (`DAEMON_CORE.md` §3.3), the peer tally row, and the
/// malformed-header record.
pub trait AdmissionHost: Admission + Send + Sync {
    /// Resolve a request to its principal — the daemon's one edge resolver.
    ///
    /// The daemon implements this over `AppState::resolve`, which covers all
    /// five arms: a live guest grant becomes [`Principal::Guest`], another
    /// bearer [`Principal::RemoteClient`], a readable `X-Node-Id`
    /// [`Principal::Member`], a loopback caller's self-declared name
    /// [`Principal::LocalOwner`], and nothing at all
    /// [`Principal::Anonymous`].
    fn resolve(&self, headers: &HeaderMap, peer: Option<SocketAddr>) -> Principal;

    /// Parse the optional `X-Node-Id` header, the canonical wire form.
    ///
    /// The parser and its conformance claims stay with the daemon until the
    /// host owns the HTTP edge, so this is a port method rather than a local
    /// function: two implementations of the one canonical wire form is exactly
    /// the `FE-99` clause this parser is tagged for (ARCH principle 8).
    fn parse_node_id(&self, headers: &HeaderMap) -> Option<NodeId>;

    /// Open the per-peer tally row for `node`; the lease closes it on drop.
    ///
    /// The tally follows the response BODY's lifetime (so `/status`'s `active`
    /// counter is truthful for a streamed turn), which is why it is a separate
    /// lease from the admission slot.
    fn peer_tally(&self, node: &NodeId) -> Box<dyn AdmissionLease>;

    /// Record a present-but-unparseable `X-Node-Id` so `/status` can name it on
    /// the zero-bucket row (ARCH principle 6: absence is reported).
    fn record_rejected_node_id(&self, raw: &str);
}

/// Response-body wrapper that holds an RAII guard for the whole streaming
/// lifetime of the body, so a counter opened at admit time closes when the body
/// is consumed, dropped, or the client disconnects — not merely when the
/// handler returned.
///
/// This is the one place the "serving right now" window is defined. Two guards
/// ride it, for the same reason and by the same rule: the peer tally's `active`
/// counter and the per-principal fair-share slot. Holding the latter to headers
/// time would be wrong on a streamed turn: headers leave as soon as the first
/// token is ready, while the decode permit is still held.
pub struct GuardedBody<G> {
    inner: axum::body::Body,
    _guard: G,
}

impl<G> GuardedBody<G> {
    pub fn new(inner: axum::body::Body, guard: G) -> Self {
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

// ── Client fair-share admission (order `serve50-identity`) ─────────────────

/// Concurrency the host can carry before the inference slot queue starts
/// shedding — the numerator `serving_policy::fair_sched::fair_share_cap`
/// divides among active principals.
///
/// **Derived, not picked.** The slot queue sheds when the predicted wait
/// exceeds `DEFAULT_MAX_QUEUE_WAIT_MS = 30_000`
/// (`sovereign-inference/src/embedded/model_slot.rs:862`) and predicts
/// `position × avg_turn_ms` against one decode permit. Sixteen is that bound at
/// a ~1.9 s turn: the depth at which the host is fully committed but not yet
/// refusing. Sizing it *there* is what keeps this cap from becoming a second
/// shed rule — at or below this concurrency the slot queue serves everyone, so
/// the only thing the cap changes is WHOSE turns fill it.
pub const DEFAULT_CLIENT_FAIR_CONCURRENCY: u32 = 16;

/// Read the fair-share budget from `SOVEREIGN_CLIENT_FAIR_CONCURRENCY`. A
/// malformed or zero value is REPORTED and falls back to the default — never
/// silently accepted, since a zero budget would floor every cap at 1 and
/// quietly turn a rationing rule into a serialization rule.
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

/// The request's principal: the value the edge attached, or — where no edge
/// layer ran (the internal router) — a resolution here.
///
/// Both middlewares call this, so the "attached or resolved" fallback has one
/// implementation and cannot drift between them (ARCH principle 8). The
/// resolver reads the same headers and `ConnectInfo` peer address either way,
/// so the attached value equals what the fallback would have produced.
fn principal_of<S: AdmissionHost>(
    state: &S,
    req: &Request<Body>,
    headers: &HeaderMap,
) -> Principal {
    if let Some(attached) = req.extensions().get::<AttachedPrincipal>() {
        return attached.0.clone();
    }
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0);
    state.resolve(headers, peer)
}

/// Axum middleware: per-principal fair share on the CLIENT surface.
///
/// The §9.3 red in one sentence: ten callers with ten credentials were served
/// strictly by arrival order, so the one keeping 32 requests in flight took
/// 79.5% of the turns against a 10% population share. This adapter reads the
/// [`Principal`] the edge attached as an [`AttachedPrincipal`] extension (the
/// one resolver, run once by `client_auth_layer`), falling back to
/// [`AdmissionHost::resolve`] on the internal router, and asks
/// [`Admission::admit`] whether that principal is already holding its equal
/// share.
///
/// **Peer traffic passes straight through.** A request carrying `X-Node-Id` is
/// already rationed per node by [`peer_admission_layer`]. Gating it here too
/// would be exactly the double-gate the order forbids.
pub async fn client_fairness_layer<S>(
    State(state): State<S>,
    headers: HeaderMap,
    req: Request<Body>,
    next: Next,
) -> Response
where
    S: AdmissionHost + Clone + Send + Sync + 'static,
{
    // Peer requests are the peer gate's business. One decider each.
    if headers.get("x-node-id").is_some() {
        return next.run(req).await;
    }

    // The edge resolved once and attached the value; only the internal router,
    // which has no `client_auth_layer`, reaches the resolver fallback.
    let principal = principal_of(&state, &req, &headers);

    match state.admit(&principal, sovereign_time::unix_millis()) {
        AdmissionVerdict::Admitted(lease) => {
            let response = next.run(req).await;
            // The share is held for the BODY's lifetime, not headers time — a
            // streamed turn still owns the decode permit after its headers go
            // out.
            response.map(|body| Body::new(GuardedBody::new(body, lease)))
        }
        // Over its share. This is backpressure with a hint, rendered through
        // the one shed renderer so a client cannot tell it apart from any other
        // `Retry-After` refusal it already handles.
        AdmissionVerdict::Rejected(rejection) => shed_response(rejection),
    }
}

/// Axum middleware fn. Apply via
/// `axum::middleware::from_fn_with_state(state, peer_admission_layer)`.
///
/// On admit: forwards to the inner handler with the tally lease bound to the
/// response BODY and the scheduler slot released at headers time.
///
/// On reject: returns 503 + `Retry-After` header + JSON body.
pub async fn peer_admission_layer<S>(
    State(state): State<S>,
    headers: HeaderMap,
    req: Request<Body>,
    next: Next,
) -> Response
where
    S: AdmissionHost + Clone + Send + Sync + 'static,
{
    if headers.get("x-node-id").is_none() {
        return next.run(req).await;
    }
    // Peer request: key the fair scheduler on the origin node. The one edge
    // resolver assigns the `Member` arm — it reads `X-Node-Id` before the
    // loopback branch, so a peer on the trusting listener is a member and not
    // a local owner. A present-but-unparseable id does not resolve to a
    // member: its raw value is recorded and it buckets under the zero node, so
    // it is still gated and never silently bypasses the ceiling. Recording the
    // raw value is what lets /status NAME it on the zero-bucket row (order
    // commons-fluency fix 7) — an opaque `node-0000000000000000` row would
    // default the absence instead of reporting it (ARCH §18.3).
    let node = match principal_of(&state, &req, &headers) {
        Principal::Member { node_id } => node_id,
        _ => {
            let raw = headers
                .get("x-node-id")
                .or_else(|| headers.get("X-Node-Id"))
                .and_then(|v| v.to_str().ok())
                .unwrap_or("<unreadable header>");
            state.record_rejected_node_id(raw);
            NodeId::from_u128(0)
        }
    };
    let who = Principal::Member { node_id: node };
    match state.admit(&who, sovereign_time::unix_millis()) {
        AdmissionVerdict::Admitted(slot) => {
            // The tally row opens for the whole serving window, before the
            // handler runs; the slot releases at headers time (below), the
            // tally when the body ends.
            let tally = state.peer_tally(&node);
            let response = next.run(req).await;
            drop(slot);
            response.map(|body| Body::new(GuardedBody::new(body, tally)))
        }
        AdmissionVerdict::Rejected(rejection) => {
            // Rejections are NOT tallied: a 503 means "not serving" and must
            // not read as serving on /status.
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
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};

    use axum::routing::post;
    use axum::Router;
    use tower::ServiceExt;

    const VALID: &str = "0000000000000000000000000000002a";

    /// A lease that records nothing — the stub's stand-in for a daemon guard.
    struct Lease;
    impl AdmissionLease for Lease {}

    /// A host whose decision is fixed and whose ports count what the adapters
    /// asked of them. Fields are shared so a test can probe after the router
    /// has consumed its clone.
    #[derive(Clone)]
    struct StubHost {
        admit: bool,
        admits: Arc<AtomicU32>,
        tallies: Arc<AtomicU32>,
        rejected: Arc<Mutex<Option<String>>>,
        principal: Principal,
        last_principal: Arc<Mutex<Option<Principal>>>,
    }

    impl StubHost {
        fn new(admit: bool) -> Self {
            Self {
                admit,
                admits: Arc::new(AtomicU32::new(0)),
                tallies: Arc::new(AtomicU32::new(0)),
                rejected: Arc::new(Mutex::new(None)),
                principal: Principal::Anonymous,
                last_principal: Arc::new(Mutex::new(None)),
            }
        }

        fn admits(&self) -> u32 {
            self.admits.load(Ordering::Relaxed)
        }

        fn tallies(&self) -> u32 {
            self.tallies.load(Ordering::Relaxed)
        }

        fn rejected(&self) -> Option<String> {
            self.rejected.lock().unwrap().clone()
        }

        fn last_principal(&self) -> Option<Principal> {
            self.last_principal.lock().unwrap().clone()
        }
    }

    impl Admission for StubHost {
        fn admit(&self, who: &Principal, _now_unix_ms: u64) -> AdmissionVerdict {
            self.admits.fetch_add(1, Ordering::Relaxed);
            *self.last_principal.lock().unwrap() = Some(who.clone());
            if self.admit {
                AdmissionVerdict::Admitted(Box::new(Lease))
            } else {
                AdmissionVerdict::Rejected(AdmissionRejection::new(
                    "nope",
                    AdmissionReason::CeilingExceeded,
                    3,
                ))
            }
        }

        fn posture(&self) -> AdmissionPosture {
            AdmissionPosture::Open
        }
    }

    impl AdmissionHost for StubHost {
        fn resolve(&self, _headers: &HeaderMap, _peer: Option<SocketAddr>) -> Principal {
            self.principal.clone()
        }

        fn parse_node_id(&self, headers: &HeaderMap) -> Option<NodeId> {
            // The real parser lives with the daemon (`headers::parse_x_node_id`,
            // tagged FE-99); this stub only has to distinguish the canonical
            // 32-hex form from a malformed one so the adapter's refusal branch
            // is reachable.
            let raw = headers
                .get("x-node-id")
                .or_else(|| headers.get("X-Node-Id"))?;
            let s = raw.to_str().ok()?;
            (s.len() == 32 && s.chars().all(|c| c.is_ascii_hexdigit()))
                .then(|| NodeId::from_u128(0x2a))
        }

        fn peer_tally(&self, _node: &NodeId) -> Box<dyn AdmissionLease> {
            self.tallies.fetch_add(1, Ordering::Relaxed);
            Box::new(Lease)
        }

        fn record_rejected_node_id(&self, raw: &str) {
            *self.rejected.lock().unwrap() = Some(raw.to_string());
        }
    }

    fn peer_router(state: StubHost) -> Router {
        Router::new()
            .route("/v1/chat/completions", post(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(
                state,
                peer_admission_layer::<StubHost>,
            ))
    }

    fn client_router(state: StubHost) -> Router {
        Router::new()
            .route("/v1/chat/completions", post(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(
                state,
                client_fairness_layer::<StubHost>,
            ))
    }

    fn request(header: Option<&str>) -> Request<Body> {
        let mut builder = Request::post("/v1/chat/completions");
        if let Some(h) = header {
            builder = builder.header("x-node-id", h);
        }
        builder.body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn a_peer_request_is_admitted_and_tallied_once() {
        let state = StubHost::new(true);
        let resp = peer_router(state.clone())
            .oneshot(request(Some(VALID)))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(state.admits(), 1, "the peer path decides once");
        assert_eq!(state.tallies(), 1, "an admitted peer opens one tally row");
    }

    #[tokio::test]
    async fn a_refused_peer_is_503_with_retry_after_and_is_not_tallied() {
        let state = StubHost::new(false);
        let resp = peer_router(state.clone())
            .oneshot(request(Some(VALID)))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(resp.headers().get(RETRY_AFTER).unwrap(), "3");
        assert_eq!(
            state.tallies(),
            0,
            "a 503 means not serving — it must not read as serving on /status"
        );
    }

    #[tokio::test]
    async fn a_malformed_header_is_recorded_and_still_gated() {
        let state = StubHost::new(true);
        let resp = peer_router(state.clone())
            .oneshot(request(Some("not-a-node-id")))
            .await
            .unwrap();
        // Still gated (zero bucket), so it is admitted by this host rather than
        // bypassing the ceiling.
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(state.admits(), 1, "a malformed id must not bypass the gate");
        assert_eq!(state.rejected().as_deref(), Some("not-a-node-id"));
    }

    #[tokio::test]
    async fn a_non_peer_request_passes_the_peer_layer_untouched() {
        let state = StubHost::new(false);
        let resp = peer_router(state.clone())
            .oneshot(request(None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(state.admits(), 0, "no X-Node-Id means not peer traffic");
    }

    #[tokio::test]
    async fn a_client_request_is_decided_and_held_to_the_body() {
        let state = StubHost::new(true);
        let resp = client_router(state.clone())
            .oneshot(request(None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(state.admits(), 1, "the client path decides once");
    }

    #[tokio::test]
    async fn a_refused_client_is_503_with_retry_after() {
        let state = StubHost::new(false);
        let resp = client_router(state.clone())
            .oneshot(request(None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(resp.headers().get(RETRY_AFTER).unwrap(), "3");
    }

    #[tokio::test]
    async fn a_peer_request_passes_the_client_layer_untouched() {
        let state = StubHost::new(false);
        let resp = client_router(state.clone())
            .oneshot(request(Some(VALID)))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            state.admits(),
            0,
            "peer traffic is the peer gate's business — never double-gated"
        );
    }

    /// Stand in for `client_auth_layer`'s insertion: attach `attached` to the
    /// request before the admission layer runs.
    fn with_attached(router: Router, attached: Principal) -> Router {
        router.layer(axum::middleware::from_fn(
            move |mut req: Request<Body>, next: Next| {
                let attached = attached.clone();
                async move {
                    req.extensions_mut().insert(AttachedPrincipal(attached));
                    next.run(req).await
                }
            },
        ))
    }

    /// The edge resolves once and attaches the value; both middlewares read it
    /// rather than resolving a second time (`DAEMON_CORE.md` §3.3, "one
    /// resolution at the edge, one value").
    ///
    /// Positive: with the extension present, the decision sees the attached
    /// principal — here a `Member` the stub's own `resolve` would never return
    /// (it returns `Anonymous`). Negative control: with no extension, the
    /// fallback resolution is what reaches the decision.
    #[tokio::test]
    async fn the_attached_principal_is_used_instead_of_a_second_resolution() {
        let attached = Principal::Member {
            node_id: NodeId::from_u128(0xBEEF),
        };

        // Positive, client layer: the attached value reaches `admit`.
        let state = StubHost::new(true);
        assert_eq!(state.principal, Principal::Anonymous);
        let resp = with_attached(client_router(state.clone()), attached.clone())
            .oneshot(request(None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            state.last_principal(),
            Some(attached.clone()),
            "the edge's value must reach the decision, not a fresh resolution"
        );

        // Positive, peer layer: the attached `Member` keys the tally, and the
        // header parse is not consulted.
        let state = StubHost::new(true);
        let resp = with_attached(peer_router(state.clone()), attached.clone())
            .oneshot(request(Some(VALID)))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            state.tallies(),
            1,
            "the attached member opens the tally row"
        );
        assert_eq!(
            state.last_principal(),
            Some(attached),
            "the peer gate must decide on the attached member"
        );

        // Negative control: no extension → the fallback resolver decides.
        let state = StubHost::new(true);
        let resp = client_router(state.clone())
            .oneshot(request(None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            state.last_principal(),
            Some(Principal::Anonymous),
            "the internal router's fallback must still resolve"
        );
    }

    #[test]
    fn jitter_stays_inside_the_base_plus_spread_window() {
        for base in [1u64, 2, 30] {
            for _ in 0..64 {
                let got = jittered_retry_after_secs(base);
                assert!(
                    (base..base + RETRY_AFTER_JITTER_SPREAD_SECS).contains(&got),
                    "base {base} produced {got}, outside [{base}, {})",
                    base + RETRY_AFTER_JITTER_SPREAD_SECS
                );
            }
        }
    }

    #[test]
    fn jitter_varies_across_calls() {
        let seen: std::collections::HashSet<u64> =
            (0..64).map(|_| jittered_retry_after_secs(2)).collect();
        assert!(seen.len() > 1, "a constant hint is the thundering herd");
    }

    #[test]
    fn fair_concurrency_env_reports_a_bad_value_instead_of_accepting_it() {
        let prev = std::env::var("SOVEREIGN_CLIENT_FAIR_CONCURRENCY").ok();
        std::env::set_var("SOVEREIGN_CLIENT_FAIR_CONCURRENCY", "not-a-number");
        assert_eq!(
            client_fair_concurrency_from_env(),
            DEFAULT_CLIENT_FAIR_CONCURRENCY
        );
        std::env::set_var("SOVEREIGN_CLIENT_FAIR_CONCURRENCY", "0");
        assert_eq!(
            client_fair_concurrency_from_env(),
            DEFAULT_CLIENT_FAIR_CONCURRENCY
        );
        std::env::set_var("SOVEREIGN_CLIENT_FAIR_CONCURRENCY", "24");
        assert_eq!(client_fair_concurrency_from_env(), 24);
        match prev {
            Some(v) => std::env::set_var("SOVEREIGN_CLIENT_FAIR_CONCURRENCY", v),
            None => std::env::remove_var("SOVEREIGN_CLIENT_FAIR_CONCURRENCY"),
        }
    }

    #[test]
    fn fairness_defaults_on_and_the_kill_switch_is_explicit() {
        let prev = std::env::var("SOVEREIGN_CLIENT_FAIRNESS").ok();
        std::env::remove_var("SOVEREIGN_CLIENT_FAIRNESS");
        assert!(client_fairness_enabled_from_env(), "default is on");
        for off in ["0", "false", "OFF", " no "] {
            std::env::set_var("SOVEREIGN_CLIENT_FAIRNESS", off);
            assert!(!client_fairness_enabled_from_env(), "{off:?} must disable");
        }
        for on in ["1", "true", "yes", "anything-else"] {
            std::env::set_var("SOVEREIGN_CLIENT_FAIRNESS", on);
            assert!(client_fairness_enabled_from_env(), "{on:?} must stay on");
        }
        match prev {
            Some(v) => std::env::set_var("SOVEREIGN_CLIENT_FAIRNESS", v),
            None => std::env::remove_var("SOVEREIGN_CLIENT_FAIRNESS"),
        }
    }

    #[test]
    fn shed_response_is_503_with_retry_after_and_the_openai_error_object() {
        let response = shed_response(AdmissionRejection::new(
            "busy",
            AdmissionReason::CeilingExceeded,
            7,
        ));
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers().get(RETRY_AFTER).unwrap(), "7");
    }

    #[test]
    fn local_queue_shed_names_the_queue_position_and_predicted_wait() {
        let response = local_queue_shed_response(4, 30_000, 5);
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers().get(RETRY_AFTER).unwrap(), "5");
    }
}
