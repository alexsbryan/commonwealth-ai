//! The rail listener, where the grant is the only way in.
//!
//! Kept from the retired convergence-ceiling suite: svrn still serves
//! `/v1/rail/*` on its own bind as a grant-checked forward (phase-b-33).

use super::*;

/// Drive the RAIL bind rather than the operator one.
async fn call_rail(state: AppState, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let resp = sovereign_daemon::server::client_router_for(
        state,
        sovereign_daemon::server::ClientSurface::Rail,
    )
    .oneshot(req)
    .await
    .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

/// **Watched failing first, and it is the reason the rail is a separate bind.**
///
/// A ring app is a process on this machine, so it arrives on loopback — and on
/// the operator bind that alone admits it, before any bearer is read. Pointed
/// there, an app would arrive as an OPERATOR and its grant would be ignored:
/// the namespace scoping would be decorative, and a guard nobody can watch
/// fail is not a guard (§18.1). On the rail bind the token is the only way in.
#[tokio::test]
async fn on_the_rail_bind_a_loopback_caller_without_a_grant_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let state = with_guest(
        state_with_rail(dir.path(), &key).await,
        vec![Scope::Rails(NS.into())],
    );

    let (no_token, _) = call_rail(
        state.clone(),
        request("GET", "/v1/rail/log", LOOPBACK, None, None),
    )
    .await;
    assert_eq!(
        no_token,
        StatusCode::UNAUTHORIZED,
        "loopback alone must not admit on the rail bind"
    );

    // The same request WITH the grant is served, and served the right
    // namespace — which the caller never named.
    let (with_token, body) = call_rail(
        state,
        request("GET", "/v1/rail/log", LOOPBACK, Some(GUEST_TOKEN), None),
    )
    .await;
    assert_eq!(with_token, StatusCode::OK, "{body}");
    assert_eq!(body["namespace"], NS);
}

/// The rail bind serves the rail and nothing else, proven from BOTH sides.
///
/// Two independent mechanisms refuse here and a test that only saw one could
/// be satisfied while the other silently broke:
///
/// - **A rail grant** gets 403 on everything outside `/v1/rail/*`, because
///   `Scope::paths()` is the allowlist and the auth layer checks it before
///   routing. That is what stops grants minting grants.
/// - **The daemon token** bypasses scope entirely — so a 404 for the same
///   paths is evidence about the ROUTER: those routes are not mounted on this
///   surface at all (§7.1). If they were, this arm would answer 200.
#[tokio::test]
async fn the_rail_bind_serves_nothing_but_the_rail() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let elsewhere = [
        "/internal/guest/grant",
        "/v1/models",
        "/v1/chat/completions",
        "/v1/mesh/status",
    ];
    for path in elsewhere {
        let state = with_guest(
            state_with_rail(dir.path(), &key).await,
            vec![Scope::Rails(NS.into())],
        );
        let (scoped, _) = call_rail(
            state.clone(),
            request("GET", path, LOOPBACK, Some(GUEST_TOKEN), None),
        )
        .await;
        assert_eq!(scoped, StatusCode::FORBIDDEN, "a rail grant reached {path}");

        // The same path under a credential that has no scope limit at all.
        let (unscoped, _) =
            call_rail(state, request("GET", path, LOOPBACK, Some(TOKEN), None)).await;
        assert_eq!(
            unscoped,
            StatusCode::NOT_FOUND,
            "{path} is MOUNTED on the rail surface — the route set, not the \
             grant, is what must exclude it"
        );
    }

    // `/status` is the documented odd one out and gets its own case.
    // `AUTH_EXEMPT_PATHS` lets it past the auth layer WITHOUT a scope check,
    // so it reaches the router — where the rail surface does not mount it. A
    // probe against a rail listener therefore sees 404, not 401, which reads
    // as "wrong node" rather than "wrong credential". That is a known cost of
    // the split, written down here so the next person to debug it does not
    // spend the afternoon on a credential that was never the problem.
    for bearer in [GUEST_TOKEN, TOKEN] {
        let state = with_guest(
            state_with_rail(dir.path(), &key).await,
            vec![Scope::Rails(NS.into())],
        );
        let (status, _) = call_rail(
            state,
            request("GET", "/status", LOOPBACK, Some(bearer), None),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "/status under {bearer}");
    }

    // And the rail itself is served on this bind, so the assertions above are
    // not passing because the whole router is empty.
    let state = with_guest(
        state_with_rail(dir.path(), &key).await,
        vec![Scope::Rails(NS.into())],
    );
    let (ok, _) = call_rail(
        state,
        request("GET", "/v1/rail/log", LOOPBACK, Some(GUEST_TOKEN), None),
    )
    .await;
    assert_eq!(ok, StatusCode::OK);
}
