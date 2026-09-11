// SPDX-License-Identifier: AGPL-3.0-or-later
//! `/internal/peer-preference/{list,set,clear}` — the operator's private
//! per-peer affinity multiplier, over the wire.
//!
//! The multiplier scales every claim affinity this node advertises to one
//! peer (`routes_oicp.rs:383 apply_peer_preference`), so it is a policy the
//! DAEMON owns: it lives in the daemon's `MeshStore`, the daemon's OICP
//! manifest path is its only reader, and the daemon's `PeerPreference::new`
//! is the only constructor that can produce a valid one.
//!
//! Before sv-surface svt-3 the desktop reached into an in-process
//! `AppState.inner.peer_preferences` to serve the Mesh Health panel, and
//! REFUSED in Attach mode because there was no route. Refusing was correct
//! (ARCH principle 6 — never quietly do the work against the wrong process),
//! but it left two crates, `commonwealth-core` and `commonwealth-state`,
//! linked into a client for three commands. These three routes are what
//! that refusal was waiting for.
//!
//! TRUST POSTURE. This is the `/internal` listener, whose module header
//! (`routes_internal/mod.rs:7-18`) says plainly that it is reachable by any
//! peer that can route to this host and is NOT transport-authenticated. The
//! two mutating routes here sit beside the operator-policy writes already
//! mounted on it — `/internal/contribution/ceiling`, `/internal/ingest/budget`,
//! `/internal/storage/budget`, `/internal/mesh/quiesce` (`server.rs:496,556,568,547`)
//! — and carry the same exposure, no more and no less. The multiplier is
//! clamped to `(0.0, 1.0]`, so the worst a reachable caller can do is make
//! this node serve some peer LESS; there is no branch that raises affinity.
//! Narrowing this whole namespace to loopback is a separate decision that
//! belongs to every route on it at once, not to the three added here.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use commonwealth_core::ids::NodeId;
use commonwealth_state::PeerPreference;
use serde::{Deserialize, Serialize};

use crate::state::AppState;

/// One peer's affinity preference, in the shape the desktop's Mesh Health
/// panel renders.
///
/// Field names are the desktop `PeerPreferenceDto`'s verbatim and carry no
/// serde renames, so a rename on either side blanks the panel rather than
/// mistyping it — the same contract `NodeContributionsView` keeps with
/// `NodeContributionsDto`, and pinned the same way, by a parse test on the
/// client side over this handler's literal output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerPreferenceView {
    /// 32-char lowercase hex of the peer's node id.
    pub node_id: String,
    /// Affinity multiplier in `(0.0, 1.0]`.
    pub multiplier: f64,
    /// Operator's free-text note, if they left one.
    pub reason: Option<String>,
    /// Unix seconds the preference was last written.
    pub set_at: u64,
}

/// Body of `POST /internal/peer-preference/set`.
#[derive(Debug, Clone, Deserialize)]
pub struct SetPeerPreferenceRequest {
    pub node_id: String,
    pub multiplier: f64,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Body of `POST /internal/peer-preference/clear`.
#[derive(Debug, Clone, Deserialize)]
pub struct ClearPeerPreferenceRequest {
    pub node_id: String,
}

/// Decode the 32-char hex form the desktop keys peers by.
///
/// The length + charset precondition is explicit and the decode itself is
/// `NodeId::from_hex` — the canonical parser — so there is one decoder, not
/// a hand-rolled twin (ARCH principle 8). The precondition is not
/// decoration: `from_hex` alone `.trim()`s its input and would therefore
/// accept `" <32 hex> "`, which the desktop's own parser refused before this
/// route existed. Keeping the refusal is what makes moving the call across
/// the socket behaviour-preserving.
fn parse_node_id_hex(s: &str) -> Result<NodeId, (StatusCode, String)> {
    if s.len() != 32 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("expected 32-hex-char node id, got '{s}'"),
        ));
    }
    NodeId::from_hex(s).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid hex node id '{s}'"),
        )
    })
}

/// `GET /internal/peer-preference/list` — every preference this node holds.
///
/// Order is the `MeshStore` scan order the store itself yields
/// (`commonwealth-state/src/peer_preferences.rs:164`), unchanged: the CLI's
/// `peer-preference list` reads the same call, and re-ordering here would
/// make one of the two surfaces disagree with the store.
pub async fn peer_preference_list(
    State(state): State<AppState>,
) -> Result<Json<Vec<PeerPreferenceView>>, (StatusCode, String)> {
    let entries = state.inner.peer_preferences.list().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("peer_preference_list: {e}"),
        )
    })?;
    tracing::debug!(
        target: "peer_pref",
        count = entries.len(),
        "peer_pref:list served over /internal"
    );
    Ok(Json(
        entries
            .into_iter()
            .map(|(id, p)| PeerPreferenceView {
                node_id: id.to_hex(),
                multiplier: p.multiplier(),
                reason: p.reason().map(|s| s.to_string()),
                set_at: p.set_at(),
            })
            .collect(),
    ))
}

/// `POST /internal/peer-preference/set` — set or replace one peer's
/// multiplier.
///
/// The `(0.0, 1.0]` clamp is `PeerPreference::new`'s and stays there. A
/// rejected multiplier is a 400 carrying that constructor's own message, so
/// the client shows the daemon's reason rather than a second copy of the
/// rule (ARCH principle 8 — one decider).
pub async fn peer_preference_set(
    State(state): State<AppState>,
    Json(req): Json<SetPeerPreferenceRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let target = parse_node_id_hex(&req.node_id)?;
    let pref = PeerPreference::new(req.multiplier, req.reason)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e}")))?;
    state
        .inner
        .peer_preferences
        .set(&target, pref)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("peer_preference_set: {e}"),
            )
        })?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /internal/peer-preference/clear` — drop one peer's preference.
///
/// Returns the store's own idempotency answer: `true` if a preference was
/// there, `false` if there was nothing to clear. Not collapsed into a bare
/// 204 — "cleared" and "there was nothing set" are different facts and the
/// panel distinguishes them (ARCH principle 6).
pub async fn peer_preference_clear(
    State(state): State<AppState>,
    Json(req): Json<ClearPeerPreferenceRequest>,
) -> Result<Json<bool>, (StatusCode, String)> {
    let target = parse_node_id_hex(&req.node_id)?;
    let existed = state.inner.peer_preferences.clear(&target).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("peer_preference_clear: {e}"),
        )
    })?;
    Ok(Json(existed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    use crate::server::internal_router;
    use crate::state::test_app_state;

    /// Build the REAL internal router over a shared state.
    ///
    /// Deliberately not a hand-rolled `Router::new().route(...)` like the
    /// sibling test in `mesh_admin.rs`: a test that re-declares the paths
    /// cannot fail on a path typo in `server.rs`, because the test authored
    /// the path it is checking (ARCH principle 5 — assert on something the
    /// subject cannot author). Driving `internal_router` means the MOUNT is
    /// under test, not just the handler.
    fn router(state: &AppState) -> axum::Router {
        internal_router(state.clone())
    }

    async fn body_string(resp: axum::response::Response) -> String {
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .expect("read body");
        String::from_utf8(bytes.to_vec()).expect("utf8 body")
    }

    const PEER: &str = "0a0b0c0d0e0f101112131415161718aa";

    /// The whole loop the Mesh Health panel drives, through the real mount:
    /// list empty, set, list it back, clear it, clear again.
    ///
    /// Every field carries a DISTINCT value so a field wired to its
    /// neighbour fails instead of matching by coincidence, and the second
    /// clear pins the idempotency answer — `false` is the interesting one,
    /// because collapsing it into the same 204 as a real clear is what
    /// ARCH principle 6 forbids.
    #[tokio::test]
    async fn the_panel_loop_round_trips_through_the_mounted_routes() {
        let state = test_app_state();

        let resp = router(&state)
            .oneshot(
                Request::get("/internal/peer-preference/list")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "list route is mounted");
        let empty: Vec<PeerPreferenceView> =
            serde_json::from_str(&body_string(resp).await).expect("list parses");
        assert!(empty.is_empty(), "a fresh node holds no preferences");

        let resp = router(&state)
            .oneshot(
                Request::post("/internal/peer-preference/set")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "node_id": PEER,
                            "multiplier": 0.25,
                            "reason": "throttled while it backfills",
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NO_CONTENT,
            "set route is mounted"
        );

        let resp = router(&state)
            .oneshot(
                Request::get("/internal/peer-preference/list")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let listed: Vec<PeerPreferenceView> =
            serde_json::from_str(&body_string(resp).await).expect("list parses");
        assert_eq!(listed.len(), 1, "the preference just set comes back");
        assert_eq!(
            listed[0].node_id, PEER,
            "the hex id survives the round trip"
        );
        assert_eq!(listed[0].multiplier, 0.25, "an f64, not rounded or clamped");
        assert_eq!(
            listed[0].reason.as_deref(),
            Some("throttled while it backfills"),
            "the operator's note survives"
        );
        assert!(listed[0].set_at > 0, "the store stamped it");

        let resp = router(&state)
            .oneshot(
                Request::post("/internal/peer-preference/clear")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "node_id": PEER }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "clear route is mounted");
        assert_eq!(
            body_string(resp).await,
            "true",
            "clearing a set preference says it was there"
        );

        let resp = router(&state)
            .oneshot(
                Request::post("/internal/peer-preference/clear")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "node_id": PEER }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            body_string(resp).await,
            "false",
            "clearing nothing is a FACT the host states, not a second 204"
        );
    }

    /// The clamp stays the daemon's. A multiplier outside `(0.0, 1.0]` must
    /// come back 400 carrying `PeerPreference::new`'s own words — not be
    /// silently coerced, and not be re-checked by a second copy of the rule
    /// in the client (ARCH principle 8).
    #[tokio::test]
    async fn an_out_of_range_multiplier_is_the_hosts_refusal() {
        let state = test_app_state();
        for bad in ["0.0", "1.5", "-1.0"] {
            let resp = router(&state)
                .oneshot(
                    Request::post("/internal/peer-preference/set")
                        .header("content-type", "application/json")
                        .body(Body::from(format!(
                            "{{\"node_id\":\"{PEER}\",\"multiplier\":{bad}}}"
                        )))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::BAD_REQUEST,
                "multiplier {bad} must be refused"
            );
            let body = body_string(resp).await;
            assert!(
                body.contains("(0.0, 1.0]"),
                "the refusal carries the constructor's own rule, got: {body}"
            );
        }

        // And nothing landed in the store.
        let resp = router(&state)
            .oneshot(
                Request::get("/internal/peer-preference/list")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let listed: Vec<PeerPreferenceView> =
            serde_json::from_str(&body_string(resp).await).expect("list parses");
        assert!(listed.is_empty(), "a refused set writes nothing");
    }

    /// The precondition is the whole reason this is not a bare
    /// `NodeId::from_hex`. Each input names a distinct way to be wrong, and
    /// the whitespace case is the one `from_hex` accepts on its own — the
    /// desktop's pre-svt-3 parser refused it, so accepting it here would be
    /// a silent widening at exactly the moment the call moved hosts.
    #[test]
    fn the_hex_precondition_refuses_what_from_hex_alone_would_take() {
        let good = "0a0b0c0d0e0f101112131415161718aa";
        assert!(parse_node_id_hex(good).is_ok(), "32 hex chars are accepted");

        // `NodeId::from_hex` trims, so this one parses fine WITHOUT the
        // length+charset gate — it is the positive control for the gate.
        let padded = format!(" {good} ");
        assert!(
            NodeId::from_hex(&padded).is_some(),
            "from_hex alone accepts surrounding whitespace — if this ever \
             fails, the precondition below is testing nothing"
        );
        assert!(
            parse_node_id_hex(&padded).is_err(),
            "the route must refuse what the desktop's parser refused"
        );

        assert!(parse_node_id_hex("").is_err(), "empty");
        assert!(parse_node_id_hex(&good[..31]).is_err(), "31 chars");
        assert!(parse_node_id_hex(&format!("{good}0")).is_err(), "33 chars");
        assert!(
            parse_node_id_hex("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz").is_err(),
            "right length, not hex"
        );
    }

    /// Round-trip through the exact pair the wire uses: the client sends
    /// `to_hex`, the route decodes it, the list route re-encodes it. A
    /// decoder that dropped or reordered bytes would break here.
    #[test]
    fn hex_round_trips_through_the_wire_form() {
        let id = NodeId::from_u128(0x0a0b_0c0d_0e0f_1011_1213_1415_1617_18aa);
        let hex = id.to_hex();
        assert_eq!(hex, "0a0b0c0d0e0f101112131415161718aa");
        assert_eq!(parse_node_id_hex(&hex).unwrap(), id);
    }
}
