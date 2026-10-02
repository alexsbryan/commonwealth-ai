// SPDX-License-Identifier: AGPL-3.0-or-later
//! The server shell every program's binary serves HTTP through
//! (FIVE_PROGRAMS §2c; phase-b pb-shell).
//!
//! It owns the listener half of a program and nothing about its routes:
//! the peer address on every request, the loopback guard ([`guard`]), the
//! request-body limits, bind retry, shutdown as a value, and the mount
//! trace. A program hands [`serve`] its listeners and its [`RouteBundle`]s;
//! which routes exist is the program's own. [`serve_under`] is the guard a
//! program serving a page bundle reads its files through.

mod files;
pub mod guard;

pub use files::serve_under;

use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;

use axum::handler::Handler;
use axum::routing::MethodRouter;
use axum::Router;
use tokio::net::TcpListener;

/// A named set of routes. The name and the router are ONE value, and the
/// name is what the mount trace prints beside the routes.
///
/// Routes enter only through [`RouteBundle::route`] and
/// [`RouteBundle::fallback`], so the list the trace prints is the list the
/// bundle serves: there is no door for an unnamed route.
pub struct RouteBundle<S = ()> {
    name: &'static str,
    router: Router<S>,
    routes: Vec<String>,
}

impl<S> RouteBundle<S>
where
    S: Clone + Send + Sync + 'static,
{
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            router: Router::new(),
            routes: Vec::new(),
        }
    }

    /// `Router::route`, recorded.
    pub fn route(mut self, path: &str, method_router: MethodRouter<S>) -> Self {
        self.router = self.router.route(path, method_router);
        self.routes.push(path.to_string());
        self
    }

    /// `Router::fallback`, recorded as `*` — a bundle that serves every
    /// unmatched path says so in its trace.
    pub fn fallback<H, T>(mut self, handler: H) -> Self
    where
        H: Handler<T, S>,
        T: 'static,
    {
        self.router = self.router.fallback(handler);
        self.routes.push("*".to_string());
        self
    }

    pub fn with_state<S2>(self, state: S) -> RouteBundle<S2> {
        RouteBundle {
            name: self.name,
            router: self.router.with_state(state),
            routes: self.routes,
        }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Every path this bundle serves, in registration order.
    pub fn routes(&self) -> &[String] {
        &self.routes
    }
}

/// Merge the bundles into one router and print the mount trace: one `info`
/// event per bundle, naming it and every route it serves.
pub fn mount(bundles: Vec<RouteBundle>) -> Router {
    let mut app = Router::new();
    for bundle in bundles {
        tracing::info!(
            target: "host_kit::shell",
            bundle = bundle.name,
            routes = %bundle.routes.join(" "),
            "shell: mounted"
        );
        app = app.merge(bundle.router);
    }
    app
}

/// Serve `bundles` on every listener until `shutdown` resolves.
///
/// Every connection carries its peer address (`ConnectInfo<SocketAddr>`),
/// which the loopback guard and the per-handler [`guard::LocalOnly`] read:
/// a bare `axum::serve` drops it and the guard fails closed. Returns the
/// first listener error, after every listener has stopped.
pub async fn serve(
    listeners: impl IntoIterator<Item = TcpListener>,
    bundles: Vec<RouteBundle>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let names: Vec<&'static str> = bundles.iter().map(|b| b.name).collect();
    let app = mount(bundles);
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        shutdown.await;
        let _ = stop_tx.send(true);
    });
    let mut serving = tokio::task::JoinSet::new();
    for listener in listeners {
        let addr = listener.local_addr()?;
        tracing::info!(
            target: "host_kit::shell",
            %addr,
            bundles = %names.join(" "),
            "shell: serving"
        );
        let mut stop = stop_rx.clone();
        let service = app
            .clone()
            .into_make_service_with_connect_info::<SocketAddr>();
        serving.spawn(async move {
            axum::serve(listener, service)
                .with_graceful_shutdown(async move {
                    // Err = the sender is gone, which happens only when the
                    // caller's shutdown future panicked: stop, never serve on.
                    if stop.wait_for(|stopped| *stopped).await.is_err() {
                        tracing::warn!(target: "host_kit::shell", "shell: shutdown signal lost; stopping");
                    }
                })
                .await
        });
    }
    let mut first_err = None;
    while let Some(joined) = serving.join_next().await {
        let err = match joined {
            Ok(Ok(())) => continue,
            Ok(Err(e)) => e,
            Err(join) => std::io::Error::other(join),
        };
        tracing::error!(target: "host_kit::shell", error = %err, "shell: listener stopped");
        first_err.get_or_insert(err);
    }
    tracing::info!(target: "host_kit::shell", bundles = %names.join(" "), "shell: stopped");
    first_err.map_or(Ok(()), Err)
}

/// The request-body bound, as a router tail like [`guard::LoopbackRouter`].
pub trait BodyLimits {
    /// Bound the request BODY: at most `max_bytes`, delivered within
    /// `read_timeout` (a slow-loris guard). Responses, streamed or not, are
    /// not touched. The caller names both numbers.
    fn body_limits(self, max_bytes: usize, read_timeout: Duration) -> Self;
}

impl<S> BodyLimits for Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    fn body_limits(self, max_bytes: usize, read_timeout: Duration) -> Self {
        self.layer(tower_http::timeout::RequestBodyTimeoutLayer::new(
            read_timeout,
        ))
        .layer(axum::extract::DefaultBodyLimit::max(max_bytes))
    }
}

/// Bind `addr`, retrying while the address is still held by a listener
/// that is releasing it.
///
/// `SO_REUSEADDR` (which mio sets) only lets a new bind past a socket in
/// `TIME_WAIT`, not one still in `LISTEN`, so a restart that races the old
/// listener's drop gets five tries 100 ms apart. Any other error, or the
/// fifth `AddrInUse`, is returned with `label` and `addr` in its message.
pub async fn bind_with_retry(addr: SocketAddr, label: &str) -> std::io::Result<TcpListener> {
    const ATTEMPTS: usize = 5;
    const BACKOFF: Duration = Duration::from_millis(100);
    let mut last_err: Option<std::io::Error> = None;
    for attempt in 1..=ATTEMPTS {
        match TcpListener::bind(addr).await {
            Ok(listener) => return Ok(listener),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                tracing::warn!(
                    target: "host_kit::shell",
                    %addr, attempt, attempts = ATTEMPTS,
                    "bind {label}: address in use — retrying in {}ms (old listener \
                     may still be releasing)",
                    BACKOFF.as_millis()
                );
                last_err = Some(e);
                tokio::time::sleep(BACKOFF).await;
            }
            Err(e) => {
                return Err(std::io::Error::new(
                    e.kind(),
                    format!("bind {label} on {addr} failed: {e}"),
                ));
            }
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AddrInUse,
        format!(
            "bind {label} on {addr} failed after {ATTEMPTS} attempts: {}",
            last_err
                .map(|e| e.to_string())
                .unwrap_or_else(|| "address in use".to_string())
        ),
    ))
}

#[cfg(test)]
#[path = "shell/tests.rs"]
mod tests;
