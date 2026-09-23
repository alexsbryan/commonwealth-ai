// SPDX-License-Identifier: AGPL-3.0-or-later
//! The guest door — the `Guest` client surface on a bind the room's WiFi
//! can reach, open only while a rail grant is live.
//!
//! Nothing here is a new principal class. The router on `[daemon] guest_bind`
//! is [`client_router_for`](crate::server::client_router_for) with
//! [`ClientSurface::Guest`] — the same route set and the same
//! `UNTRUSTED_LOOPBACK` auth posture as the `GUEST_ALPN` bind — plus ONE
//! route of its own: the ring page, a static bundle under [`PAGE_PREFIX`] —
//! one per rail namespace the wall holds ([`GuestPages`]) —
//! merged OUTSIDE the auth layer because a browser cannot present a bearer
//! when it navigates. The page is code, not data; everything it reads or
//! writes goes through the rail routes, which are scoped to one namespace for
//! a `Scope::Rails` grant and to what [`GuestPages`] declares for a wall one.
//! `/app/{app_id}/*` could not carry it: that route reverse-proxies
//! to a running app's port and serves no directory (`routes_apps::proxy_app`).
//!
//! The listener exists only while [`live_rail_grants`] is non-zero, so a
//! daemon with `guest_bind` set and no wall grant out has nothing listening.
//!
//! The mesh holds what a BROWSER (or the dev server standing in for one)
//! touches — the page prefix, the `window.ring` shim and the
//! served-from-inside-the-bundle guard ([`sovereign_mesh::guest_pages`],
//! re-exported below so the door's paths keep resolving) — because the CLI's
//! dev servers need the same three without linking this crate.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path as AxPath, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use sovereign_core::guest_pages::GuestPage;
use sovereign_grants::GuestGrantStore;
use tracing::{info, warn};

use crate::client_surface::ClientSurface;
use crate::state::AppState;

// The door and `svrn ring dev` / `svrn meshapp dev` share these three, so
// each has one implementation — theirs, in `sovereign_mesh::guest_pages`
// (fp-30's de-embed); the door is a caller like the dev servers now.
pub use sovereign_mesh::guest_pages::{ring_shim, serve_under, PAGE_PREFIX};

/// The shim's file name, beside each page so the page's relative imports
/// resolve. One name, two homes: `/ring/__ring.js` for the un-namespaced page
/// and `/ring/<namespace>/__ring.js` for a registered one.
const SHIM_FILE: &str = "__ring.js";

/// The shim's path beside the un-namespaced page.
const SHIM_PATH: &str = "/ring/__ring.js";

/// Which bundle directory the door serves for which rail namespace.
///
/// A wall holds as many apps as the house put on it and each app is its own
/// rail namespace, so the page surface is a REGISTRY keyed by namespace
/// rather than one directory at one prefix — which apps exist is data that
/// changes without a code change (ARCH §9), the same shape `[iroh.apps]`
/// already uses for the apps a node publishes to members.
/// **It is also the DECLARATION of which namespaces admit guests at all.** An
/// entry here is the owner saying "this app is on the wall" — the shape a
/// recipe's `mesh_sharing` already has for a corpus — and a wall grant
/// ([`Scope::Wall`](sovereign_grants::Scope::Wall)) reaches exactly what is
/// declared here and nothing else on the rail.
#[derive(Clone, Debug, Default)]
pub struct GuestPages {
    /// `[daemon] guest_page_dir` — the page at the bare `/ring/`, which is
    /// what a wall with one app has always had and still has.
    default_dir: Option<PathBuf>,
    /// `[daemon.guest_pages]` — namespace → the entry that says where its
    /// bundle is and what guests may do there, each served at
    /// `/ring/<namespace>/`.
    by_namespace: std::collections::BTreeMap<String, GuestPage>,
}

impl GuestPages {
    /// The registry as configured: the un-namespaced page and the named ones.
    pub fn new(
        default_dir: Option<PathBuf>,
        by_namespace: std::collections::BTreeMap<String, GuestPage>,
    ) -> Self {
        Self {
            default_dir,
            by_namespace,
        }
    }

    /// The registry as `[daemon]` declares it — the ONE place the two config
    /// keys become the door's page surface, so the daemon reads neither.
    ///
    /// **Refuses a namespace this daemon owns.** The work plane, the KV rings,
    /// the atlas and `mesh-measurements` carry the daemon's own writes under
    /// a roster derived from membership; declaring one guest-open would hand
    /// a stranger an append on the machinery, and no operator types that on
    /// purpose. It is refused HERE, at the one config read, so the daemon
    /// declines to start rather than serving a door that is wrong — and
    /// refused AGAIN at the rail route, because a registry that was wrong must
    /// not be the only guard (ARCH 5). The membership question is
    /// [`is_daemon_owned`](sovereign_mesh::ring_roster::is_daemon_owned),
    /// which is the decider that already knows; this only says what to do
    /// about the answer.
    pub fn from_config(d: &sovereign_core::setup_config::DaemonSection) -> Result<Self, String> {
        for ns in d.guest_pages.keys() {
            if sovereign_mesh::ring_roster::is_daemon_owned(ns) {
                return Err(format!(
                    "[daemon.guest_pages] declares '{ns}' open to guests, but that is one of \
                     this daemon's OWN rings — its roster is the mesh's membership and its \
                     writes are the machinery's, not an app's. Remove that entry; a guest \
                     app needs a namespace of its own."
                ));
            }
        }
        Ok(Self::new(d.guest_page_dir.clone(), d.guest_pages.clone()))
    }

    /// Nothing to serve — the door's rail routes stand alone.
    pub fn is_empty(&self) -> bool {
        self.default_dir.is_none() && self.by_namespace.is_empty()
    }

    /// What this wall's owner declared about `namespace`, or `None` if they
    /// declared nothing about it.
    ///
    /// **THE reader of the declaration**, for the door's page routes and for
    /// the rail's `resolve_granted` alike — so "is this app on the wall" has
    /// one answer whether it is asked of a page request or of an append.
    pub fn declared(&self, namespace: &str) -> Option<&GuestPage> {
        self.by_namespace.get(namespace)
    }

    /// Every namespace declared here, in config order (`BTreeMap`, so by
    /// name). What the door's index at `/ring/` lists.
    pub fn declared_namespaces(&self) -> impl Iterator<Item = &str> {
        self.by_namespace.keys().map(String::as_str)
    }
}

/// What a `/ring/…` request resolves to — decided in ONE place so the
/// registry and the live-grant rule cannot disagree between the index route
/// and the file route.
#[derive(Debug, PartialEq)]
enum PageRoute<'a> {
    /// Serve `rel` from inside `dir`, injecting the shim at `shim`.
    File {
        dir: &'a Path,
        rel: String,
        shim: Option<String>,
    },
    /// The shim itself, rendered for this namespace.
    Shim { namespace: String },
    /// The wall's own index at the bare `/ring/`: the declared apps a live
    /// grant reaches, each a link. Only when `guest_page_dir` is unset —
    /// a wall with one app keeps the page at that address.
    Index(Vec<String>),
    /// Nothing is served here, and the sentence says why.
    Missing(String),
}

/// Resolve `rel` (the path after `/ring/`) against the registry.
///
/// `granted` answers "does a live grant name this rail namespace". A
/// registered page whose namespace no live grant names is NOT served: the
/// door is open because some grant is live, and without this one wall grant
/// would hand out every app on the wall. A live namespace with no registered
/// page is a 404 that names it — never a fall-through to another app's page,
/// which would answer a question nobody asked (ARCH §6).
fn route_page<'a>(
    pages: &'a GuestPages,
    granted: &dyn Fn(&str) -> bool,
    rel: &str,
) -> PageRoute<'a> {
    let (head, tail) = rel.split_once('/').unwrap_or((rel, ""));
    if let Some(page) = pages.by_namespace.get(head) {
        let dir = page.dir();
        if !granted(head) {
            return PageRoute::Missing(format!("no live grant names the rail namespace {head}"));
        }
        let rel = if tail.is_empty() { "index.html" } else { tail };
        if rel == SHIM_FILE {
            return PageRoute::Shim {
                namespace: head.to_string(),
            };
        }
        return PageRoute::File {
            dir,
            rel: rel.to_string(),
            shim: (rel == "index.html").then(|| format!("{PAGE_PREFIX}{head}/{SHIM_FILE}")),
        };
    }
    if !head.is_empty() && granted(head) {
        return PageRoute::Missing(format!(
            "rail namespace {head} has a live grant but no page is registered for it"
        ));
    }
    match &pages.default_dir {
        Some(dir) => {
            let rel = if rel.is_empty() { "index.html" } else { rel };
            if rel == SHIM_FILE {
                return PageRoute::Shim {
                    namespace: String::new(),
                };
            }
            PageRoute::File {
                dir,
                rel: rel.to_string(),
                shim: (rel == "index.html").then(|| SHIM_PATH.to_string()),
            }
        }
        // No page at the bare prefix. If apps are REGISTERED, the bare prefix
        // is the wall's index — which is what makes one QR reach the whole
        // wall: the phone lands here and picks, carrying the same fragment.
        // A deeper path is still a 404, because the index is one page and not
        // a directory.
        None if rel.is_empty() => {
            let reachable: Vec<String> = pages
                .by_namespace
                .keys()
                .filter(|ns| granted(ns))
                .cloned()
                .collect();
            if reachable.is_empty() {
                return PageRoute::Missing(
                    "no live grant reaches any app registered at this door".to_string(),
                );
            }
            PageRoute::Index(reachable)
        }
        None => PageRoute::Missing("no ring page is registered here".to_string()),
    }
}

/// Does a live grant reach this rail namespace? The door's one authority for
/// serving a namespaced page, read per request for the same reason expiry is
/// (`GuestGrantStore::live`).
///
/// Two ways to reach one: a grant that NAMES it, or a wall grant and an owner
/// who DECLARED it. The second is why one QR serves a whole wall — and it is
/// still bounded by the registry, so a wall grant never reaches a namespace
/// nobody put on the wall.
fn namespace_is_granted(
    store: &GuestGrantStore,
    pages: &GuestPages,
    ns: &str,
    now_ms: u64,
) -> bool {
    store.all().iter().any(|g| {
        g.is_live(now_ms)
            && (g.rail_namespace() == Some(ns) || (g.is_wall() && pages.declared(ns).is_some()))
    })
}

/// The page routes' state: what is registered, and who may see it.
#[derive(Clone)]
struct PageState {
    pages: Arc<GuestPages>,
    grants: Arc<GuestGrantStore>,
}

/// How often the lifecycle re-reads the grant store. Opening lags a mint by
/// at most this, and closing lags the last expiry by at most this — during
/// which the lapsed bearer is already refused, because expiry is evaluated
/// on every request (`GuestGrantStore::live`).
const POLL: Duration = Duration::from_secs(1);

/// Live grants that reach the rail as of `now_ms` — the door's one input. A
/// wall grant counts: it names no namespace and reaches every declared one.
pub fn live_rail_grants(store: &GuestGrantStore, now_ms: u64) -> usize {
    store
        .all()
        .iter()
        .filter(|g| g.is_live(now_ms) && g.reaches_the_rail())
        .count()
}

/// The router the door serves: the Guest surface, and the registered pages
/// if any are.
///
/// `turn_host` is the daemon that will run `POST /v1/guest/ask` in-process.
/// It is a parameter rather than something the router reaches for because
/// `AppState` holds no `Runtime` — the turn surface is built from
/// `Arc<EmbeddedDaemon>` everywhere else in this crate too
/// (`turn_http::turn_router`). `None` builds a door whose ask route names its
/// own absence rather than one that silently 404s.
pub fn door_router(
    state: AppState,
    pages: Arc<GuestPages>,
    turn_host: Option<Arc<crate::daemon::EmbeddedDaemon>>,
) -> Router {
    let grants = state.inner.node.guest_grants.clone();
    let mut guest = crate::server::client_router_for(state, ClientSurface::Guest);
    if let Some(host) = turn_host {
        guest = guest.layer(axum::Extension(host));
    }
    if pages.is_empty() {
        return guest;
    }
    guest.merge(
        Router::new()
            .route(PAGE_PREFIX, get(page_index))
            .route("/ring/{*rel}", get(page_file))
            .with_state(PageState { pages, grants }),
    )
}

/// Serve the door on `bind` for as long as the daemon runs: listen while a
/// rail grant is live, close at the last expiry, open again at the next mint.
///
/// Never returns — it sits in the daemon's serve `select!`, where returning
/// would end every other listener with it. An unparseable bind is reported
/// once and the door stays shut; an unset one is the default, off.
pub async fn serve(
    state: AppState,
    bind: Option<String>,
    pages: Arc<GuestPages>,
    turn_host: Option<Arc<crate::daemon::EmbeddedDaemon>>,
) {
    let Some(bind) = bind else {
        tracing::debug!("guest door: off ([daemon] guest_bind unset)");
        return std::future::pending().await;
    };
    let addr: SocketAddr = match bind.parse() {
        Ok(a) => a,
        Err(e) => {
            warn!(
                bind,
                "guest door: [daemon] guest_bind is not host:port ({e}) — door stays shut"
            );
            return std::future::pending().await;
        }
    };
    let store = state.inner.node.guest_grants.clone();
    loop {
        let live = wait_for(&store, |n| n > 0).await;
        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                warn!(%addr, "guest door: bind failed ({e}) — retrying");
                tokio::time::sleep(POLL).await;
                continue;
            }
        };
        info!(
            %addr,
            live_rail_grants = live,
            pages = ?pages,
            "guest door: open"
        );
        // `ConnectInfo` for the same reason as every other client bind:
        // without it the auth layer cannot identify the caller and fails
        // closed with a 500.
        let service = door_router(state.clone(), pages.clone(), turn_host.clone())
            .into_make_service_with_connect_info::<SocketAddr>();
        let closing = store.clone();
        if let Err(e) = axum::serve(listener, service)
            .with_graceful_shutdown(async move {
                wait_for(&closing, |n| n == 0).await;
            })
            .await
        {
            warn!(%addr, "guest door: server error: {e}");
        }
        info!(%addr, "guest door: closed — no live rail grant");
    }
}

/// Poll until the live rail-grant count satisfies `done`; return the count.
async fn wait_for(store: &GuestGrantStore, done: impl Fn(usize) -> bool) -> usize {
    loop {
        let n = live_rail_grants(store, commonwealth_core::clock::unix_now_millis());
        if done(n) {
            return n;
        }
        tokio::time::sleep(POLL).await;
    }
}

async fn page_index(State(st): State<PageState>) -> Response {
    serve_route(&st, "")
}

async fn page_file(State(st): State<PageState>, AxPath(rel): AxPath<String>) -> Response {
    serve_route(&st, &rel)
}

/// The one body both page routes have: resolve, then serve or refuse.
fn serve_route(st: &PageState, rel: &str) -> Response {
    let now = commonwealth_core::clock::unix_now_millis();
    let granted = |ns: &str| namespace_is_granted(&st.grants, &st.pages, ns, now);
    match route_page(&st.pages, &granted, rel) {
        PageRoute::File { dir, rel, shim } => {
            tracing::debug!(rel, dir = %dir.display(), "guest door: page");
            serve_under(dir, &rel, shim.as_deref())
        }
        PageRoute::Index(namespaces) => {
            tracing::debug!(
                apps = ?namespaces,
                "guest door: the wall's index at the bare prefix"
            );
            (
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                wall_index(&namespaces),
            )
                .into_response()
        }
        // Same origin: the page's rail calls go to this listener.
        PageRoute::Shim { namespace } => (
            [(header::CONTENT_TYPE, "text/javascript")],
            ring_shim(&namespace, Some("")),
        )
            .into_response(),
        PageRoute::Missing(why) => {
            tracing::info!(rel, why, "guest door: no page served");
            (StatusCode::NOT_FOUND, why).into_response()
        }
    }
}

/// The wall's index: one link per app a live grant reaches.
///
/// **The links are completed in the browser, and they have to be.** The bearer
/// travels in the URL FRAGMENT, which no browser ever sends to a server — that
/// is the whole reason the page reads its grant from `location.hash`. So this
/// page cannot know the token it was opened with, and the script copies the
/// fragment onto each link instead. One scan, one bearer, every app on the
/// wall.
///
/// Namespaces are rendered into an href and into text. `valid_namespace` in
/// the rail bounds them to a safe alphabet, and the registry's keys are the
/// owner's own config rather than anything a caller supplied — but the escape
/// below is here anyway, because "the input is trusted" is the sentence that
/// precedes every injection.
fn wall_index(namespaces: &[String]) -> String {
    let items: String = namespaces
        .iter()
        .map(|ns| {
            let safe = html_escape(ns);
            format!("<li><a class=\"app\" href=\"{safe}/\">{safe}</a></li>")
        })
        .collect();
    format!(
        "<!doctype html><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>The wall</title>\
         <style>body{{font:16px/1.5 system-ui,sans-serif;margin:2rem;max-width:32rem}}\
         li{{margin:.6rem 0}}a{{font-size:1.2rem}}</style>\
         <h1>The wall</h1><ul>{items}</ul>\
         <script>for (const a of document.querySelectorAll('a.app')) \
         a.href += location.hash;</script>"
    )
}

/// Minimal HTML-text escaping for a namespace rendered into an attribute and
/// into element text. Not a general sanitizer — it covers exactly the five
/// characters that can leave either context.
fn html_escape(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '&' => "&amp;".to_string(),
            '<' => "&lt;".to_string(),
            '>' => "&gt;".to_string(),
            '"' => "&quot;".to_string(),
            '\'' => "&#39;".to_string(),
            other => other.to_string(),
        })
        .collect()
}

// Moved to a sibling file: inline, these put this file into the 800-1200
// approach band (ARCH §3.1). `#[path]`, so the names are unchanged.
#[cfg(test)]
#[path = "tests/guest_door.rs"]
mod tests;
