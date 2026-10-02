// SPDX-License-Identifier: AGPL-3.0-or-later
//! Admission — the decision's ports and svrn's two axum middlewares.
//!
//! `sovereign/SERVING_BOUNDARY.md` "The five entries" (c). svrn mounts these
//! layers and no other program does, so they are svrn's (pb-svrn-serving-ports,
//! §12 3a rung 1; they lived in `sovereign-serving-host` until then). The wire
//! they answer with — the shed reason, its jitter and the 503 renderer — is the
//! contracts leaf's `admission_wire`, which serve answers through too.
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
    http::{HeaderMap, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use kernel_types::NodeId;
use oicp_types::openai_types::ErrorDetail;
use sovereign_contracts::admission_wire::{self, AdmissionRejection};
use sovereign_contracts::principal::{AttachedPrincipal, Principal};

/// svrn's axum face of the one shed renderer (`admission_wire::shed_response`).
pub fn shed_response(rejection: AdmissionRejection) -> Response {
    admission_wire::shed_response(rejection).into_response()
}

/// svrn's axum face of `admission_wire::local_queue_shed_response`.
pub fn local_queue_shed_response(
    position: u32,
    predicted_wait_ms: u64,
    retry_after_secs: u64,
) -> Response {
    admission_wire::local_queue_shed_response(position, predicted_wait_ms, retry_after_secs)
        .into_response()
}

/// What a peer request spends — two resources, two budgets (seat A23). A corpus
/// read (`/internal/knowledge/search`, ~ms of I/O) is not an inference, so one
/// offloaded judge holding the inference slot must not blind every member's
/// fan-out to this node's corpora.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerWork {
    Inference,
    KnowledgeRead,
}

impl PeerWork {
    /// The `[daemon]` key of the ceiling that decides this work, named on every
    /// admission event.
    pub const fn ceiling(self) -> &'static str {
        match self {
            Self::Inference => "max_peer_inflight",
            Self::KnowledgeRead => "max_peer_knowledge_reads",
        }
    }
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

    /// Decide a member's corpus read ([`PeerWork::KnowledgeRead`]) — under its
    /// own ceiling, never the inference one.
    fn admit_knowledge_read(&self, who: &Principal, now_unix_ms: u64) -> AdmissionVerdict;

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

    /// Open the per-peer tally row for `node`; the lease closes it on drop.
    ///
    /// The tally follows the response BODY's lifetime (so `/status`'s `active`
    /// counter is truthful for a streamed turn), which is why it is a separate
    /// lease from the admission slot.
    fn peer_tally(&self, node: &NodeId) -> Box<dyn AdmissionLease>;
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

/// Does this principal CLAIM a peer identity?
///
/// The one question that splits the two gates, asked in one place so they
/// cannot disagree (ARCH principle 8). [`Principal::Member`] is a claim this
/// daemon verified; [`Principal::Unverified`] is a claim it could not — both
/// are the peer gate's to answer, and neither is a client-fairness caller.
/// Every other arm named nothing about a peer and belongs to client fairness.
fn claims_peer_identity(who: &Principal) -> bool {
    matches!(who, Principal::Member { .. } | Principal::Unverified)
}

/// The refusal a caller gets when it claimed a peer identity this daemon could
/// not verify. A sentence, not a code: the caller has to be able to tell this
/// apart from a ceiling (which is a 503 with `Retry-After`) — retrying will
/// never help, because nothing about the claim is transient.
fn unverified_peer_response(route: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(oicp_types::openai_types::ErrorResponse {
            error: ErrorDetail {
                message: format!(
                    "{route}: this request claims a peer identity that was not \
                     verified by this node's iroh acceptor. A peer route is \
                     served to a verified mesh member; reach it over the mesh \
                     transport, not by dialling this port directly."
                ),
                error_type: "unverified_peer".to_string(),
                code: Some("unverified_peer".to_string()),
            },
        }),
    )
        .into_response()
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
/// **Peer traffic passes straight through.** A request whose principal claims a
/// peer identity is already the peer gate's ([`peer_admission_layer`]) business.
/// Gating it here too would be exactly the double-gate the order forbids.
pub async fn client_fairness_layer<S>(
    State(state): State<S>,
    headers: HeaderMap,
    req: Request<Body>,
    next: Next,
) -> Response
where
    S: AdmissionHost + Clone + Send + Sync + 'static,
{
    // The edge resolved once and attached the value; only a router with no
    // principal layer reaches the resolver fallback.
    let principal = principal_of(&state, &req, &headers);

    // Peer requests are the peer gate's business. One decider each, and the
    // two gates split on the SAME question — `claims_peer_identity` — so a
    // request cannot fall through both or be caught by both.
    if claims_peer_identity(&principal) {
        return next.run(req).await;
    }

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
    peer_gate(state, headers, req, next, PeerWork::Inference).await
}

/// The same peer gate for `/internal/knowledge/search`, decided by
/// [`Admission::admit_knowledge_read`] under its own ceiling (seat A23).
pub async fn peer_knowledge_read_layer<S>(
    State(state): State<S>,
    headers: HeaderMap,
    req: Request<Body>,
    next: Next,
) -> Response
where
    S: AdmissionHost + Clone + Send + Sync + 'static,
{
    peer_gate(state, headers, req, next, PeerWork::KnowledgeRead).await
}

async fn peer_gate<S>(
    state: S,
    headers: HeaderMap,
    req: Request<Body>,
    next: Next,
    work: PeerWork,
) -> Response
where
    S: AdmissionHost + Clone + Send + Sync + 'static,
{
    // Peer request: key the fair scheduler on the origin node. The principal
    // the edge resolved is the whole input — this gate does not re-read the
    // wire, so it cannot disagree with the resolver about who is asking.
    let principal = principal_of(&state, &req, &headers);
    let node = match principal {
        Principal::Member { node_id } => node_id,
        // An identity was claimed and this daemon could not verify it. There
        // is no ceiling to charge it to, so it is REFUSED rather than bucketed
        // under node zero: a ceiling keyed on an id nobody proved is a ceiling
        // any caller can pick (bar `mp-principal-is-the-verified-key` clause
        // (d)). The raw value was recorded by the resolver that read it, so
        // /status still NAMES it on the zero-bucket row.
        Principal::Unverified => {
            tracing::info!(
                target: "transport",
                route = %req.uri().path(),
                "admission: 403 — this request claims a peer identity this \
                 daemon could not verify, and a peer ceiling needs a verified \
                 node id, so it is refused rather than charged to node zero"
            );
            return unverified_peer_response(req.uri().path());
        }
        // Not peer traffic at all: the client fairness gate's business.
        _ => return next.run(req).await,
    };
    let who = Principal::Member { node_id: node };
    let now = sovereign_time::unix_millis();
    let verdict = match work {
        PeerWork::Inference => state.admit(&who, now),
        PeerWork::KnowledgeRead => state.admit_knowledge_read(&who, now),
    };
    match verdict {
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
                ceiling = work.ceiling(),
                retry_after_secs = rejection.retry_after_secs,
                "admission: 503 — peer request gated"
            );
            shed_response(rejection)
        }
    }
}

#[cfg(test)]
mod tests;
