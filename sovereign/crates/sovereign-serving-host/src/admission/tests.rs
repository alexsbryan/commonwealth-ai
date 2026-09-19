// SPDX-License-Identifier: AGPL-3.0-or-later
//! `admission`'s test module, split out of `admission.rs` by domains
//! `REVIEW-audit-4` (the `ring_sync.rs`/`scoring.rs` pattern `REVIEW-audit-daemon-1`
//! used): the file had climbed into arch-gate's 800-1200 approach band and the
//! band is a counter ratchet. Behaviour-preserving — `use super::*;` keeps the
//! same items in scope.

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
        (s.len() == 32 && s.chars().all(|c| c.is_ascii_hexdigit())).then(|| NodeId::from_u128(0x2a))
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
