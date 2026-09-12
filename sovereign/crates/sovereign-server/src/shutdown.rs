// SPDX-License-Identifier: AGPL-3.0-or-later
//! `POST /v1/admin/shutdown` — the door that lets this host be told to stop.
//!
//! # Why it exists (sv-surface svt-2)
//!
//! This binary had no stop path of any kind: no route, no signal handler, no
//! `with_graceful_shutdown`, no pidfile, no run lock. The only way it ever
//! stopped was an external SIGKILL from the desktop's Mobile-access toggle,
//! which held the `Child` and set `kill_on_drop(true)` — a client deciding
//! the lifetime of a resident server it does not own, which is the line ARCH
//! principle 12 draws and the shape sv-surface's lifecycle census counts.
//!
//! Moving the toggle onto the sanctioned bring-up (`ServingHost`) meant the
//! app stops holding a handle — and with nothing holding a handle, "stop the
//! mobile host" had nowhere left to go. That gap was never the app's to fill:
//! **a gap in one thing is not a job for another.** It is filled here, in the
//! process that owns its own lifetime.
//!
//! # Authorization
//!
//! The route sits INSIDE the `/v1/*` auth layer, so a caller presents the
//! same bearer key every other `/v1` route takes (`auth.rs:74-86`) — for the
//! mobile host that is the `sk-mobile-…` token already on disk in
//! `~/.svrnmesh/mobile-host.toml`. No second credential is minted.
//!
//! The layer is a no-op when auth is disabled (`auth.rs:62-70`), and this
//! server binds `0.0.0.0` by default, so mounting the route behind a no-op
//! gate would put "stop this process" on the open network. [`Shutdown::new`]
//! therefore takes the server's real auth posture and the handler REFUSES —
//! named, 403, with the reason — rather than serving an unguarded stop.
//!
//! # What "graceful" costs, and the watchdog
//!
//! `axum::serve(..).with_graceful_shutdown(..)` stops accepting and then
//! waits for in-flight requests. A conversation WebSocket
//! (`/v1/conversations/{id}/stream`) is in-flight for as long as the phone
//! holds it open, so the wait alone can be unbounded — a stop the user asked
//! for that never happens. [`GRACE`] bounds it: past that, the process
//! exits anyway and says so.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::Extension;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};

/// How long in-flight work may keep the process alive after a stop is
/// accepted.
///
/// 5 s, sized against what is actually in flight rather than against a
/// round number: the longest non-streaming request this server serves is a
/// turn, and a held-open WebSocket — the one thing that can outlast any
/// budget — is exactly what the watchdog exists to cut. A user who asked a
/// toggle to turn off should not wait longer than the window it takes to
/// notice the toggle moved.
const GRACE: Duration = Duration::from_secs(5);

/// The stop signal, shared between the route that raises it and the serve
/// loop that waits on it.
///
/// A `Notify` rather than a `oneshot::Sender`: the sender would have to be
/// taken out of the `Extension` by the first caller, which turns a second
/// stop request into a 500 for a process that is already stopping.
#[derive(Clone)]
pub struct Shutdown {
    requested: Arc<tokio::sync::Notify>,
    /// Does a credential actually guard the route? Carried rather than
    /// re-derived so the handler and the serve site cannot disagree about
    /// whether this server is safe to expose a stop on (ARCH principle 8).
    authenticated: bool,
}

impl Shutdown {
    /// `authenticated` is the server's REAL auth posture — the same
    /// `auth_enabled` the CORS decision and the exposure check read.
    pub fn new(authenticated: bool) -> Self {
        Self {
            requested: Arc::new(tokio::sync::Notify::new()),
            authenticated,
        }
    }

    /// Resolves when a stop has been accepted. Hand this to
    /// `with_graceful_shutdown`.
    pub async fn requested(&self) {
        self.requested.notified().await;
    }

    /// Exit even if in-flight work outlasts [`GRACE`].
    ///
    /// Spawned beside the serve loop. Without it a single held-open
    /// WebSocket makes an accepted stop indefinite, which reads to a user
    /// as the toggle not working.
    pub fn spawn_watchdog(&self) {
        let me = self.clone();
        tokio::spawn(async move {
            me.requested().await;
            tokio::time::sleep(GRACE).await;
            tracing::warn!(
                grace_secs = GRACE.as_secs(),
                "shutdown: in-flight work outlasted the grace window (a held-open \
                 conversation stream is the usual one) — exiting anyway"
            );
            sovereign_inference::fast_exit_skip_destructors(0);
        });
    }
}

/// The router. Merged into the AUTHED stack, so the auth middleware runs
/// before the handler — see the module docs on why the handler still checks.
pub fn shutdown_router() -> Router {
    Router::new().route("/v1/admin/shutdown", post(shutdown))
}

/// `POST /v1/admin/shutdown` — 202, then the process stops.
///
/// 202 rather than 204: the response is sent while the server is still
/// running, and the stop happens after it. Saying "accepted" is the honest
/// shape for that, and a caller that reads only status codes still learns
/// the difference between "stopping" and "refused".
async fn shutdown(Extension(sd): Extension<Shutdown>) -> Response {
    if !sd.authenticated {
        // Refuse, and name the substitution rather than making it (ARCH
        // principle 6). Serving this unguarded on a `0.0.0.0` bind would
        // hand anyone on the network a stop button.
        tracing::warn!(
            "shutdown: refused — this server runs with auth disabled, so the route has no \
             gate. Configure [auth] keys (the mobile host always does) and retry."
        );
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "auth_disabled",
                "message": "this server runs with [auth] disabled, so /v1/admin/shutdown \
                            has no credential to check and is refused rather than served \
                            unguarded",
            })),
        )
            .into_response();
    }
    tracing::info!("shutdown: accepted — stopping after in-flight requests");
    sd.requested.notify_waiters();
    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "stopping": true })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;

    fn req() -> axum::http::Request<axum::body::Body> {
        axum::http::Request::builder()
            .method("POST")
            .uri("/v1/admin/shutdown")
            .body(axum::body::Body::empty())
            .unwrap()
    }

    /// The stop actually reaches the serve loop's future.
    ///
    /// `notify_waiters` only wakes waiters that are ALREADY waiting, so the
    /// wait is armed before the request is sent — which is also the real
    /// ordering: `with_graceful_shutdown` holds the future from startup.
    #[tokio::test]
    async fn an_authorized_stop_wakes_the_serve_loop() {
        let sd = Shutdown::new(true);
        let waiting = {
            let sd = sd.clone();
            tokio::spawn(async move { sd.requested().await })
        };
        // Let the task reach the await point before notifying.
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        let app = shutdown_router().layer(Extension(sd));
        let resp = app.oneshot(req()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);

        tokio::time::timeout(Duration::from_secs(2), waiting)
            .await
            .expect("the serve loop's shutdown future never resolved")
            .unwrap();
    }

    /// An unguarded server refuses rather than serving a public stop button.
    ///
    /// The failing input is real: `[auth] mode` anything but `api_key`, or
    /// an empty `keys` map, makes `auth_middleware` a pass-through — so
    /// without this branch the route is reachable by anyone who can reach
    /// the bind, which defaults to `0.0.0.0`.
    #[tokio::test]
    async fn an_unauthenticated_server_refuses_to_expose_a_stop() {
        let sd = Shutdown::new(false);
        let waiting = {
            let sd = sd.clone();
            tokio::spawn(async move { sd.requested().await })
        };
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        let app = shutdown_router().layer(Extension(sd));
        let resp = app.oneshot(req()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);

        // And it must not have stopped anything on its way to refusing.
        assert!(
            tokio::time::timeout(Duration::from_millis(200), waiting)
                .await
                .is_err(),
            "a refused stop still woke the serve loop"
        );
    }
}
