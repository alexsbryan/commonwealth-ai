//! Router-level middleware that refuses non-loopback callers.
//!
//! The admin surfaces on `:9741` (`/v1/mesh/*`, `/v1/admin/*`,
//! `/mcp/*`) are privileged: they can mutate mesh membership, reload
//! inference providers, and execute MCP tools. They must only answer
//! localhost.
//!
//! The individual handlers already extract `ConnectInfo<SocketAddr>`
//! and short-circuit on non-loopback. That's good, but it's a
//! per-handler contract — a future route added to `mesh_router` /
//! `admin_router` / `mcp_router` without the extractor would silently
//! skip the guard. This middleware closes that hole by applying the
//! check at the `Router::layer` level, so every current and future
//! route inherits it.
//!
//! Keep the per-handler `enforce_localhost` calls too — belt and
//! suspenders. If the middleware is ever accidentally stripped off a
//! router, the per-handler check still denies the request.
//!
//! Relies on `axum::serve(listener,
//! router.into_make_service_with_connect_info::<SocketAddr>())`
//! being used on the listener (see `daemon::start_daemon`). Without
//! that, `ConnectInfo` is absent and this middleware can't make a
//! decision — in which case it fails closed (403).

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Request};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;

/// Per-handler loopback check. Returns `Ok(())` if `addr` is a
/// loopback peer; otherwise a pre-built 403 `Response` ready to
/// `return` from the handler.
///
/// Why this lives alongside the middleware: every route file
/// previously hand-rolled its own copy of this check with a slightly
/// different return shape — five definitions, ~24 call sites, four
/// distinct response types. ARCH §5 "defence in depth" wants both
/// layers, but it doesn't want five copies of the same body.
///
/// The middleware ([`loopback_only`]) is the primary guard; this
/// helper is the per-handler belt-and-suspenders check defended in
/// `SYSTEM_OVERVIEW.md` §5.4 ("router middleware + per-handler
/// enforce_localhost").
pub(crate) fn enforce_localhost(addr: &SocketAddr) -> Result<(), Response> {
    if addr.ip().is_loopback() {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "local-only" })),
        )
            .into_response())
    }
}

/// `axum::middleware::from_fn`-compatible loopback guard. Apply with
/// `.layer(axum::middleware::from_fn(loopback_only))` when building a
/// router that serves localhost-only routes.
///
/// We take `Request` and inspect its extensions directly rather than
/// letting axum extract `ConnectInfo<SocketAddr>` separately, because
/// we want a single, consistent "fail closed" code path when
/// ConnectInfo is missing. Axum's extractor would 500 on us before
/// the middleware body runs, giving the same outcome but hiding the
/// diagnostic log.
pub async fn loopback_only(request: Request, next: Next) -> Response {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0);

    match peer {
        Some(p) if p.ip().is_loopback() => next.run(request).await,
        Some(p) => {
            // Non-loopback: log and reject. We do echo the peer IP in
            // the log (operator needs it to investigate) but keep it
            // out of the response body — don't help probes.
            tracing::warn!(
                peer = %p,
                path = %request.uri().path(),
                "loopback_only: rejected non-loopback caller"
            );
            (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({ "error": "local-only" })),
            )
                .into_response()
        }
        None => {
            // ConnectInfo wasn't attached — either the listener
            // forgot `into_make_service_with_connect_info`, or the
            // request was injected through a path that bypasses it.
            // Fail closed: refuse rather than letting requests
            // through unauthenticated.
            tracing::error!(
                path = %request.uri().path(),
                "loopback_only: no ConnectInfo on request — check listener wiring"
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "listener misconfigured: missing connect_info"
                })),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use axum::Router;
    use std::net::{IpAddr, Ipv4Addr};
    use tokio::net::TcpListener;

    async fn hello() -> &'static str {
        "ok"
    }

    fn guarded_router() -> Router {
        Router::new()
            .route("/hello", get(hello))
            .layer(axum::middleware::from_fn(loopback_only))
    }

    /// A handler with zero awareness of the guard should still be
    /// unreachable from a non-loopback caller — middleware rejects
    /// before the handler runs. This is the core value of applying
    /// the guard at the layer level.
    #[tokio::test]
    async fn middleware_rejects_non_loopback_even_on_unguarded_handler() {
        // Bind on 0.0.0.0 so we can connect via a non-loopback IP.
        // On most dev machines a routable interface is available;
        // skip the assertion if not.
        let Ok(listener) = TcpListener::bind("0.0.0.0:0").await else {
            eprintln!("no 0.0.0.0 bind available; skipping");
            return;
        };
        let addr = listener.local_addr().unwrap();
        let port = addr.port();
        tokio::spawn(async move {
            axum::serve(
                listener,
                guarded_router().into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .ok();
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Find a routable non-loopback IP on this machine to connect
        // from. If we can't, skip — CI boxes without a usable NIC
        // should not spuriously fail.
        let Some(routable) = if_addrs::get_if_addrs()
            .ok()
            .and_then(|a| {
                a.into_iter().find_map(|i| {
                    let ip = i.ip();
                    if !ip.is_loopback() && matches!(ip, IpAddr::V4(_)) {
                        Some(ip)
                    } else {
                        None
                    }
                })
            })
        else {
            eprintln!("no routable non-loopback IP; skipping");
            return;
        };

        // Connect using that IP as the destination. Loopback on the
        // listener side but the ConnectInfo peer address is the
        // routable IP our kernel chose as source.
        let resp = reqwest::Client::new()
            .get(format!("http://{routable}:{port}/hello"))
            .send()
            .await;
        match resp {
            Ok(r) => {
                assert_eq!(
                    r.status(),
                    StatusCode::FORBIDDEN,
                    "non-loopback caller must be rejected"
                );
            }
            Err(_) => {
                // The routable IP wasn't reachable on the listener —
                // this is a network-environment issue, not a test
                // failure. Skip.
                eprintln!("could not reach server via routable IP; skipping");
            }
        }

        // Loopback request must still succeed.
        let resp = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/hello"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// Per-handler helper: loopback in any form (127.x, ::1) passes;
    /// every routable IP (LAN, Tailscale CGNAT, public v4/v6) rejects
    /// with a 403. The middleware test above pins the layer-level
    /// contract; this test pins the per-handler half of §5.4's
    /// "router middleware + per-handler enforce_localhost" pair.
    #[test]
    fn enforce_localhost_accepts_loopback_rejects_others() {
        use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

        let allowed = [
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 9741),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 1, 2, 3)), 9741),
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 9741),
        ];
        for addr in allowed {
            assert!(
                enforce_localhost(&addr).is_ok(),
                "loopback {addr} must pass"
            );
        }

        // Attack scenarios: LAN, Tailscale CGNAT, public v4/v6.
        let denied = [
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 7)), 9741),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 2)), 9741),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 9741),
            SocketAddr::new(
                IpAddr::V6(Ipv6Addr::new(0x2606, 0, 0, 0, 0, 0, 0, 1)),
                9741,
            ),
        ];
        for addr in denied {
            let Err(resp) = enforce_localhost(&addr) else {
                panic!("non-loopback {addr} must be rejected");
            };
            assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        }
    }

    /// If the listener forgets to wire ConnectInfo, the middleware
    /// fails closed with 500. Better broken than bypassed.
    #[tokio::test]
    async fn middleware_fails_closed_when_connect_info_missing() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            // NOTE: bare `axum::serve` — no connect_info.
            axum::serve(listener, guarded_router()).await.ok();
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/hello"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "missing ConnectInfo must fail closed"
        );
        // We don't assert the exact body text to keep the test
        // robust to message rewording — the status is the contract.
        let _ = ignore(&Ipv4Addr::LOCALHOST);
    }

    fn ignore<T>(_: &T) {}
}
