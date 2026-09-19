// SPDX-License-Identifier: AGPL-3.0-or-later
//! The guest door — the `Guest` client surface on a bind the room's WiFi
//! can reach, open only while a rail grant is live.
//!
//! Nothing here is a new principal class. The router on `[daemon] guest_bind`
//! is [`client_router_for`](crate::server::client_router_for) with
//! [`ClientSurface::Guest`] — the same route set and the same
//! `UNTRUSTED_LOOPBACK` auth posture as the `GUEST_ALPN` bind — plus ONE
//! route of its own: the ring page, a static directory at [`PAGE_PREFIX`],
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
/// `http://<guest_bind><PAGE_PREFIX>#token=…`.
pub const PAGE_PREFIX: &str = "/ring/";

/// The shim's path, beside the page so the page's relative imports resolve.
const SHIM_PATH: &str = "/ring/__ring.js";

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

/// The router the door serves: the Guest surface, and the page if a
/// directory is configured.
///
/// `turn_host` is the daemon that will run `POST /v1/guest/ask` in-process.
/// It is a parameter rather than something the router reaches for because
/// `AppState` holds no `Runtime` — the turn surface is built from
/// `Arc<EmbeddedDaemon>` everywhere else in this crate too
/// (`turn_http::turn_router`). `None` builds a door whose ask route names its
/// own absence rather than one that silently 404s.
pub fn door_router(
    state: AppState,
    page_dir: Option<PathBuf>,
    turn_host: Option<Arc<crate::daemon::EmbeddedDaemon>>,
) -> Router {
    let mut guest = crate::server::client_router_for(state, ClientSurface::Guest);
    if let Some(host) = turn_host {
        guest = guest.layer(axum::Extension(host));
    }
    match page_dir {
        Some(dir) => guest.merge(
            Router::new()
                .route(PAGE_PREFIX, get(page_index))
                .route("/ring/{*rel}", get(page_file))
                .with_state(Arc::new(dir)),
        ),
        None => guest,
    }
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
    page_dir: Option<PathBuf>,
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
            page = ?page_dir,
            "guest door: open"
        );
        // `ConnectInfo` for the same reason as every other client bind:
        // without it the auth layer cannot identify the caller and fails
        // closed with a 500.
        let service = door_router(state.clone(), page_dir.clone(), turn_host.clone())
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

async fn page_index(State(dir): State<Arc<PathBuf>>) -> Response {
    serve_under(&dir, "index.html", Some(SHIM_PATH))
}

async fn page_file(State(dir): State<Arc<PathBuf>>, AxPath(rel): AxPath<String>) -> Response {
    if format!("{PAGE_PREFIX}{rel}") == SHIM_PATH {
        // Same origin: the page's rail calls go to this listener.
        return (
            [(header::CONTENT_TYPE, "text/javascript")],
            ring_shim("", Some("")),
        )
            .into_response();
    }
    let shim = (rel == "index.html").then_some(SHIM_PATH);
    serve_under(&dir, &rel, shim)
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
    ask: ['POST', '/v1/guest/ask'],
  };
  const send = (op, ctype, body) => {
    if (RAIL === null) {
      return fetch('/__ring/' + op, { method: 'POST', headers: { 'content-type': ctype }, body });
    }
    const [method, path] = ROUTES[op];
    const headers = { authorization: 'Bearer ' + bearer };
    if (method === 'GET') return fetch(RAIL + path, { method, headers });
    headers['content-type'] = ctype;
    return fetch(RAIL + path, { method, headers, body });
  };
  const call = async (op, body) => {
    const r = await send(op, 'application/json', JSON.stringify(body || {}));
    const t = await r.text();
    let v = null;
    try { v = t ? JSON.parse(t) : null; } catch (_) { v = { error: t }; }
    if (!r.ok) throw new Error((v && v.error) || ('ring: ' + op + ' failed'));
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
})();
"#;

#[cfg(test)]
mod tests {
    use super::*;

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
