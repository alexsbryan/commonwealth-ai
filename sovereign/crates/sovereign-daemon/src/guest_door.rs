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
//! writes goes through the rail routes, which the grant scopes to one
//! namespace. `/app/{app_id}/*` could not carry it: that route reverse-proxies
//! to a running app's port and serves no directory (`routes_apps::proxy_app`).
//!
//! The listener exists only while [`live_rail_grants`] is non-zero, so a
//! daemon with `guest_bind` set and no wall grant out has nothing listening.
//!
//! This module also owns the two things the door and `svrn ring dev` share,
//! so each has one implementation: `window.ring` ([`ring_shim`]) and the
//! served-from-inside-the-bundle guard ([`serve_under`]).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path as AxPath, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use sovereign_grants::GuestGrantStore;
use tracing::{info, warn};

use crate::client_surface::ClientSurface;
use crate::state::AppState;

/// Where the door serves the ring page. The link a guest scans is
/// `http://<guest_bind><PAGE_PREFIX>#token=…`, or
/// `http://<guest_bind><PAGE_PREFIX><namespace>/#token=…` for a wall holding
/// more than one app.
pub const PAGE_PREFIX: &str = "/ring/";

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
#[derive(Clone, Debug, Default)]
pub struct GuestPages {
    /// `[daemon] guest_page_dir` — the page at the bare `/ring/`, which is
    /// what a wall with one app has always had and still has.
    default_dir: Option<PathBuf>,
    /// `[daemon.guest_pages]` — namespace → bundle directory, each served at
    /// `/ring/<namespace>/`.
    by_namespace: std::collections::BTreeMap<String, PathBuf>,
}

impl GuestPages {
    /// The registry as configured: the un-namespaced page and the named ones.
    pub fn new(
        default_dir: Option<PathBuf>,
        by_namespace: std::collections::BTreeMap<String, PathBuf>,
    ) -> Self {
        Self {
            default_dir,
            by_namespace,
        }
    }

    /// The registry as `[daemon]` declares it — the ONE place the two config
    /// keys become the door's page surface, so the daemon reads neither.
    pub fn from_config(d: &sovereign_core::setup_config::DaemonSection) -> Self {
        Self::new(d.guest_page_dir.clone(), d.guest_pages.clone())
    }

    /// Nothing to serve — the door's rail routes stand alone.
    pub fn is_empty(&self) -> bool {
        self.default_dir.is_none() && self.by_namespace.is_empty()
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
    if let Some(dir) = pages.by_namespace.get(head) {
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
        None => PageRoute::Missing("no ring page is registered here".to_string()),
    }
}

/// Does a live grant name this rail namespace? The door's one authority for
/// serving a namespaced page, read per request for the same reason expiry is
/// (`GuestGrantStore::live`).
fn namespace_is_granted(store: &GuestGrantStore, ns: &str, now_ms: u64) -> bool {
    store
        .all()
        .iter()
        .any(|g| g.is_live(now_ms) && g.rail_namespace() == Some(ns))
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

/// Live grants carrying a rail scope as of `now_ms` — the door's one input.
pub fn live_rail_grants(store: &GuestGrantStore, now_ms: u64) -> usize {
    store
        .all()
        .iter()
        .filter(|g| g.is_live(now_ms) && g.rail_namespace().is_some())
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
    pages: GuestPages,
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
            .with_state(PageState {
                pages: Arc::new(pages),
                grants,
            }),
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
    pages: GuestPages,
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
    let granted = |ns: &str| namespace_is_granted(&st.grants, ns, now);
    match route_page(&st.pages, &granted, rel) {
        PageRoute::File { dir, rel, shim } => {
            tracing::debug!(rel, dir = %dir.display(), "guest door: page");
            serve_under(dir, &rel, shim.as_deref())
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

/// Serve `rel` from inside `root`, or 404 — **never from outside it**.
///
/// The guard is not "reject `..`": a request path can spell an escape many
/// ways, and a check on the spelling is a check on what the caller authored
/// (ARCH §18.1). Both sides are canonicalized and the result must still be
/// under the root, so what is asserted is where the file actually IS.
///
/// Until this landed, both dev servers joined the request path onto the
/// bundle directory and read whatever came out.
pub fn serve_under(root: &Path, rel: &str, shim_src: Option<&str>) -> Response {
    let joined = root.join(rel);
    let Ok(real_root) = std::fs::canonicalize(root) else {
        return (StatusCode::NOT_FOUND, "bundle directory is gone").into_response();
    };
    let Ok(real) = std::fs::canonicalize(&joined) else {
        return (StatusCode::NOT_FOUND, format!("not found: {rel}")).into_response();
    };
    if !real.starts_with(&real_root) {
        // Say nothing about what is out there. A 404 and a refusal look the
        // same to a caller who should not have asked.
        tracing::warn!(rel, root = %real_root.display(), "dev server: refused a path outside the bundle");
        return (StatusCode::NOT_FOUND, format!("not found: {rel}")).into_response();
    }
    serve_file(&real, shim_src)
}

fn serve_file(file: &Path, shim_src: Option<&str>) -> Response {
    let bytes = match std::fs::read(file) {
        Ok(b) => b,
        Err(_) => {
            return (
                StatusCode::NOT_FOUND,
                format!("not found: {}", file.display()),
            )
                .into_response()
        }
    };
    let ct = content_type(file);
    if let Some(src) = shim_src {
        let html = String::from_utf8_lossy(&bytes);
        let tag = format!("<script src=\"{src}\"></script>");
        let injected = if let Some(idx) = html.find("</head>") {
            format!("{}{}{}", &html[..idx], tag, &html[idx..])
        } else {
            format!("{tag}{html}")
        };
        return ([(header::CONTENT_TYPE, ct)], injected).into_response();
    }
    ([(header::CONTENT_TYPE, ct)], bytes).into_response()
}

fn content_type(file: &Path) -> &'static str {
    match file.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// `window.ring` for `namespace`, over one of two transports.
///
/// `rail_base: None` is `svrn ring dev`: every op is POSTed to the dev
/// server's `/__ring/<op>`, which holds the grant, so the browser never sees
/// one. `Some(base)` is the guest door: the page calls the rail routes at
/// `base` itself, presenting the bearer it reads from the URL FRAGMENT —
/// which a browser never sends to a server. One source, both callers.
pub fn ring_shim(namespace: &str, rail_base: Option<&str>) -> String {
    let base = rail_base.map_or_else(
        || "null".to_string(),
        |b| serde_json::Value::from(b).to_string(),
    );
    RING_SHIM
        .replace("{{NAMESPACE}}", namespace)
        .replace("{{RAIL_BASE}}", &base)
}

/// `window.ring` — the whole client surface, and it is small on purpose.
///
/// **It ships the fold, not just the transport.** `log()` and `record()` are
/// the two routes; `fold()` is the third thing, and it is the reason this is
/// an SDK rather than a fetch wrapper. The rail computes the order and the
/// void set server-side, and `fold` is what makes an app author consume that
/// rather than re-derive it: they write a reducer over one act at a time and
/// never touch `log.ops` directly. Hand somebody a raw log and hope, and the
/// first thing they write is `ops.filter(...).sort(...)` — and their house
/// disagrees with itself about who owes what.
///
/// `live` is the fourth thing and it is a different kind: it writes nothing
/// down, so it is namespaced apart rather than sitting beside `record`.
const RING_SHIM: &str = r#"(function () {
  // null: `svrn ring dev` holds the grant and proxies `/__ring/<op>`.
  // A string: the rail's base, reached with the fragment's bearer.
  const RAIL = {{RAIL_BASE}};
  const bearer = RAIL === null ? null : new URLSearchParams(location.hash.slice(1)).get('token');
  const ROUTES = {
    log: ['GET', '/v1/rail/log'], append: ['POST', '/v1/rail/append'],
    live: ['POST', '/v1/rail/live'], 'live-drain': ['GET', '/v1/rail/live'],
    ask: ['POST', '/v1/guest/ask'], session: ['POST', '/v1/guest/session'],
  };
  // The NAME, asked by the door's shim and never by the app.
  //
  // One QR serves a room, so every phone holds the same bearer and the grant
  // cannot say who is writing. The door binds the name to a session handle;
  // this asks for it once per device and presents the handle from then on.
  //
  // Remembered per ORIGIN — which is this door — and NOT against the bearer.
  // A wall's second app is a second grant with its own QR (a grant names one
  // rail namespace), and the person does not change because the scope did.
  // The door decides whether the handle is still good: a handle it does not
  // recognise comes back 409 `stale_session` and is forgotten below.
  const SESSION_KEY = 'ring.session';
  let SESSION = null;
  const remembered = () => {
    try {
      const v = JSON.parse(localStorage.getItem(SESSION_KEY) || 'null');
      return v && v.handle ? v : null;
    } catch (_) { return null; }
  };
  const forget = () => { SESSION = null; try { localStorage.removeItem(SESSION_KEY); } catch (_) {} };
  const claim = async () => {
    for (;;) {
      const typed = (window.prompt('Your name for the wall') || '').trim();
      if (!typed) throw new Error('ring: the wall shows who wrote each line, so it needs a name');
      const r = await fetch(RAIL + ROUTES.session[1], {
        method: 'POST',
        headers: { authorization: 'Bearer ' + bearer, 'content-type': 'application/json' },
        body: JSON.stringify({ name: typed }),
      });
      let v = null;
      try { v = await r.json(); } catch (_) { v = null; }
      if (r.ok && v && v.session) {
        SESSION = { handle: v.session, name: v.name };
        try { localStorage.setItem(SESSION_KEY, JSON.stringify(SESSION)); } catch (_) {}
        return SESSION;
      }
      // 409 is a name the door refused — a member's, or one somebody in this
      // room already has. Both are re-askable, and the door's own sentence is
      // what the person needs to read.
      if (r.status === 409) { window.alert((v && v.error) || 'ring: that name is taken'); continue; }
      throw new Error((v && v.error) || 'ring: could not claim a name');
    }
  };
  const session = async () => {
    // `ring dev` holds the grant itself: no room, nobody to tell apart.
    if (RAIL === null) return null;
    if (!SESSION) SESSION = remembered();
    if (!SESSION) await claim();
    return SESSION;
  };
  const send = async (op, ctype, body) => {
    if (RAIL === null) {
      return fetch('/__ring/' + op, { method: 'POST', headers: { 'content-type': ctype }, body });
    }
    await session();
    const [method, path] = ROUTES[op];
    const headers = { authorization: 'Bearer ' + bearer };
    if (SESSION) headers['x-ring-session'] = SESSION.handle;
    if (method === 'GET') return fetch(RAIL + path, { method, headers });
    headers['content-type'] = ctype;
    return fetch(RAIL + path, { method, headers, body });
  };
  const call = async (op, body) => {
    const r = await send(op, 'application/json', JSON.stringify(body || {}));
    const t = await r.text();
    let v = null;
    try { v = t ? JSON.parse(t) : null; } catch (_) { v = { error: t }; }
    if (!r.ok) {
      // The handle outlived the grant it was claimed under (a re-issued link,
      // a restarted daemon). Drop it so the next call asks again rather than
      // presenting a dead one forever.
      if (r.status === 409 && v && v.code === 'stale_session') forget();
      throw new Error((v && v.error) || ('ring: ' + op + ' failed'));
    }
    return v;
  };
  window.ring = {
    namespace: "{{NAMESPACE}}",
    // The whole journal in one call: the admitted acts in the order every
    // node applies them, the gaps, and the roster. `complete === false` means
    // those acts are a subset; an app that hides that is lying to the person
    // reading it. The namespace is taken from the rail's answer: the guest
    // door serves one page to every grant and does not know which will ask.
    log: async () => {
      const v = await call('log', {});
      if (v && v.namespace) window.ring.namespace = v.namespace;
      return v;
    },
    // Write one act. The payload is yours and the rail never reads inside it
    // — but it must be a JSON object of whole numbers and strings, because
    // two nodes have to derive identical bytes from it and JSON does not
    // promise that for fractions. Use cents, grams, milliseconds.
    record: (payload) => call('append', { op: 'record', payload }),
    // Void an earlier act, optionally re-stating it. The void is PERMANENT:
    // correcting a correction cancels its replacement and leaves the original
    // gone. To bring something back, write it again.
    correct: (correctsId, replacement) =>
      call('append', { op: 'correct', corrects: correctsId, replacement: replacement || null }),
    // The live lane: delivery, not record. Nothing here reaches a journal.
    //
    // `send` hands the payload through and NOT the `call` helper on purpose:
    // the daemon reads the push body as opaque text, so `call`'s
    // JSON.stringify would wrap an already-stringified envelope in quotes and
    // every peer would skip it without saying anything.
    live: {
      send: async (payload) => {
        const r = await send('live', 'text/plain', payload);
        if (!r.ok) throw new Error('ring: live send failed (' + r.status + ')');
        return r.json();
      },
      // A drain, not a read: the daemon hands each payload out once.
      drain: () => call('live-drain', {}),
    },
    // Ask the room. The door runs the turn as its OWN principal and hands
    // back `{answer, epistemic_state}` — never a conversation id, so there is
    // no handle here to point at anyone else's.
    //
    // Only over the bearer transport. Under `svrn ring dev` the grant is held
    // by the dev server and the browser has none, so this REFUSES by name
    // rather than posting a fifth op at a proxy whose table is the rail's
    // three routes — the ask is the guest DOOR's route, not the rail's.
    ask: (question) => {
      if (RAIL === null) throw new Error('ring: ask is the guest door\'s route; `ring dev` holds no grant to present');
      return call('ask', { question });
    },
    // Fold the journal with your reducer.
    //
    // Skips the acts a correction voided and the corrections that state no
    // replacement, and walks the rest in the rail's order — which is the same
    // order on every node in the ring. Use this instead of iterating
    // `log.ops`: the guarantee is in the traversal, not in the array.
    fold: (log, reducer, initial) => {
      let acc = initial;
      for (const op of (log && log.ops) || []) {
        if (op.voided || op.payload == null) continue;
        acc = reducer(acc, op.payload, op);
      }
      return acc;
    },
  };
  // WHO this phone is, for a page that wants to greet them. READ-ONLY on
  // purpose: a page may say hello to a guest, it may not decide which guest it
  // has — that is claimed at the door and carried by the handle, and an app
  // that could set it would be back to authoring the value the wall trusts.
  Object.defineProperty(window.ring, 'guest', {
    enumerable: true,
    get: () => (SESSION ? SESSION.name : null),
  });
})();
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn pages(default_dir: Option<&str>, named: &[(&str, &str)]) -> GuestPages {
        GuestPages::new(
            default_dir.map(PathBuf::from),
            named
                .iter()
                .map(|(ns, d)| ((*ns).to_string(), PathBuf::from(*d)))
                .collect(),
        )
    }

    /// The wall serves the page whose namespace a live grant names, and
    /// **only** that one. The door is open because SOME grant is live; without
    /// this, one wall grant would hand out every app the wall holds.
    #[test]
    fn a_registered_page_is_served_only_while_a_grant_names_its_namespace() {
        let reg = pages(None, &[("wall", "/srv/wall"), ("doc", "/srv/doc")]);
        let granted = |ns: &str| ns == "wall";
        assert_eq!(
            route_page(&reg, &granted, "wall/"),
            PageRoute::File {
                dir: Path::new("/srv/wall"),
                rel: "index.html".to_string(),
                shim: Some("/ring/wall/__ring.js".to_string()),
            }
        );
        let PageRoute::Missing(why) = route_page(&reg, &granted, "doc/") else {
            panic!("the ungranted app's page was served");
        };
        assert!(why.contains("doc"), "the refusal must name the namespace");
    }

    /// A namespace a grant names with no page registered is a 404 that says
    /// so — never the other app's page, which would answer a question nobody
    /// asked (ARCH §6).
    #[test]
    fn a_granted_namespace_with_no_page_is_named_not_substituted() {
        let reg = pages(Some("/srv/one"), &[]);
        let PageRoute::Missing(why) = route_page(&reg, &|ns| ns == "doc", "doc/") else {
            panic!("an unregistered namespace fell through to another page");
        };
        assert!(why.contains("doc") && why.contains("no page is registered"));
    }

    /// The old key keeps its old address: a wall with one app is still served
    /// at the bare `/ring/`, shim and all.
    #[test]
    fn the_un_namespaced_page_is_still_served_at_the_bare_prefix() {
        let reg = pages(Some("/srv/one"), &[]);
        let never = |_: &str| false;
        assert_eq!(
            route_page(&reg, &never, ""),
            PageRoute::File {
                dir: Path::new("/srv/one"),
                rel: "index.html".to_string(),
                shim: Some(SHIM_PATH.to_string()),
            }
        );
        assert_eq!(
            route_page(&reg, &never, "app.js"),
            PageRoute::File {
                dir: Path::new("/srv/one"),
                rel: "app.js".to_string(),
                shim: None,
            }
        );
        assert_eq!(
            route_page(&reg, &never, SHIM_FILE),
            PageRoute::Shim {
                namespace: String::new()
            }
        );
    }

    /// Each registered page gets the shim beside it, rendered for its own
    /// namespace — a page that imported another app's shim would be told it
    /// is on the wrong rail.
    #[test]
    fn each_registered_page_has_its_own_shim_beside_it() {
        let reg = pages(None, &[("wall", "/srv/wall")]);
        assert_eq!(
            route_page(&reg, &|_| true, "wall/__ring.js"),
            PageRoute::Shim {
                namespace: "wall".to_string()
            }
        );
    }

    /// **The SDK must ship the fold, and the fold must skip both kinds of
    /// non-act.** A `fold` that forgot `voided` would double-count every
    /// corrected entry in every ring app on this rail, and it would look
    /// right until somebody made a correction.
    ///
    /// A string assertion because the shim is JS inside a Rust const; the
    /// behaviour itself is exercised for real by `expenses.test.mjs` and by
    /// the rail's own `a_voided_op_is_still_visible_but_is_never_applied`.
    #[test]
    fn the_shim_ships_a_fold_that_skips_voided_and_empty_acts() {
        assert!(RING_SHIM.contains("fold: (log, reducer, initial)"));
        assert!(
            RING_SHIM.contains("if (op.voided || op.payload == null) continue;"),
            "the fold stopped skipping a non-act — corrections would be counted"
        );
    }

    /// The shim carries the two routes and nothing that pretends to be a
    /// third. An app's vocabulary is built out of `record` and `correct`; a
    /// verb here that the rail does not have is a verb that fails at runtime.
    #[test]
    fn the_shim_offers_exactly_the_rails_two_writes() {
        assert!(RING_SHIM.contains("record: (payload)"));
        assert!(RING_SHIM.contains("correct: (correctsId, replacement)"));
        assert!(
            !RING_SHIM.contains("expense:") && !RING_SHIM.contains("settle:"),
            "the shim knows about money again — that belongs in the app's own \
             module, where it has tests"
        );
    }

    /// **The push body must leave the browser exactly as the app wrote it.**
    /// `call` would `JSON.stringify` an envelope the app already stringified,
    /// and `decodePresence` on the other side would skip every payload
    /// silently — no error, no cursor, nothing to read.
    #[test]
    fn the_shim_reaches_the_live_lane_without_re_encoding() {
        assert!(RING_SHIM.contains("live: {"));
        assert!(RING_SHIM.contains("send: async (payload)"));
        assert!(RING_SHIM.contains("drain: () => call('live-drain', {})"));
        assert!(
            RING_SHIM.contains("await send('live', 'text/plain', payload);"),
            "the live send stopped handing the payload through verbatim — a \
             `call(` here would double-encode it and every peer would skip it"
        );
        assert!(
            RING_SHIM.contains("return r.json();"),
            "the live send stopped returning the daemon's answer — without \
             `peers` the page cannot name a peer that did not get its presence"
        );
    }

    /// **The ask verb reaches the door's route, and only over the bearer
    /// transport.** Under `ring dev` the browser holds no grant, so a shim
    /// that posted `/__ring/ask` anyway would hit a proxy op table that is
    /// the rail's three routes and fail with the proxy's words instead of
    /// its own.
    #[test]
    fn the_ask_verb_is_the_doors_route_and_refuses_without_a_bearer() {
        assert!(RING_SHIM.contains("ask: ['POST', '/v1/guest/ask']"));
        assert!(RING_SHIM.contains("ask: (question) =>"));
        assert!(
            RING_SHIM.contains("if (RAIL === null) throw new Error('ring: ask"),
            "the ask verb stopped refusing the grantless transport — under \
             `ring dev` it would post at the rail proxy and fail obscurely"
        );
        assert!(sovereign_grants::Scope::Rails("x".into())
            .paths()
            .contains(&crate::routes_guest_ask::GUEST_ASK_PATH));
    }

    /// **Every rail route the bearer transport names is one a rail grant
    /// unlocks.** A route the shim calls that `Scope::Rails` does not list is
    /// a page that 403s on a guest's phone and works under `ring dev`.
    #[test]
    fn the_bearer_transport_calls_only_what_a_rail_grant_unlocks() {
        let unlocked = sovereign_grants::Scope::Rails("x".into()).paths();
        for route in ["'/v1/rail/log'", "'/v1/rail/append'", "'/v1/rail/live'"] {
            assert!(RING_SHIM.contains(route), "shim lost {route}");
            assert!(unlocked.contains(&route.trim_matches('\'')));
        }
    }

    /// **The shim asks the name, and the shim carries the handle.** If either
    /// half moved into an app, every app author would own a safety property
    /// (`a guest is never mistakable for a member`) that the door can enforce
    /// once — and the scaffolded app, which has no name field at all, would
    /// have its guests shown under the member's name.
    #[test]
    fn the_shim_claims_the_guests_name_itself_and_presents_the_handle() {
        assert!(RING_SHIM.contains("session: ['POST', '/v1/guest/session']"));
        assert!(
            RING_SHIM.contains("window.prompt('Your name for the wall')"),
            "the shim stopped asking for the name — an app would have to"
        );
        assert!(
            RING_SHIM.contains("headers['x-ring-session'] = SESSION.handle;"),
            "the shim stopped presenting the session handle — the door would \
             have nothing to name the guest from"
        );
        // Remembered per ORIGIN — this door — and never against the bearer,
        // which is what makes the second app on the same wall not ask again:
        // it is a second grant with a second bearer, and the same person.
        assert!(
            RING_SHIM.contains("return v && v.handle ? v : null;"),
            "the shim stopped remembering the handle for this origin"
        );
        assert!(
            !RING_SHIM.contains("v.token === bearer"),
            "the handle is bound to one bearer again — the second app on this \
             wall will ask the name a second time"
        );
    }

    /// **A page may greet a guest; it may not choose one.** A settable name
    /// would be the payload convention this replaced, wearing a new spelling.
    #[test]
    fn the_pages_view_of_the_guests_name_is_read_only() {
        assert!(
            RING_SHIM.contains("Object.defineProperty(window.ring, 'guest', {")
                && RING_SHIM.contains("get: () => (SESSION ? SESSION.name : null),"),
            "the name stopped being an accessor"
        );
        assert!(
            !RING_SHIM.contains("set: "),
            "the shim grew a setter for the guest's name"
        );
        assert!(sovereign_grants::Scope::Rails("x".into())
            .paths()
            .contains(&crate::routes_guest_session::GUEST_SESSION_PATH));
    }

    /// The one parameter that picks the transport renders as JS: `null` for
    /// the dev proxy, a quoted string for the door. An unquoted base would
    /// be a syntax error that blanks the page.
    #[test]
    fn the_transport_parameter_renders_as_js() {
        assert!(ring_shim("n", None).contains("const RAIL = null;"));
        assert!(ring_shim("n", Some("")).contains("const RAIL = \"\";"));
        assert!(!ring_shim("n", Some("")).contains("{{"));
    }
}
