// SPDX-License-Identifier: AGPL-3.0-or-later
//! The guest page's browser side, shared by the door and the dev servers —
//! where the page lives on the door, what `window.ring` speaks, and the
//! guard every page bundle is served behind.
//!
//! These were defined in `sovereign_daemon::guest_door`, where the door's
//! router uses them. That was also where `svrn ring dev` and the `svrn
//! meshapp dev` server had to reach them from, which is what tied the CLI's
//! verbs to the daemon's crate: a dev server serving static files has no
//! business linking the whole serving host. The mesh owns the rings and the
//! rails the shim speaks to (`ring_roster`, `ring_sync`, `deep_link`), so
//! the page surface lives here — one implementation for the door, `ring
//! dev` and the meshapp dev server (ARCH §10.6) — and the daemon re-exports
//! every name at its historical `guest_door::` path, so the routes and
//! their tests are unchanged.
//!
//! The door itself — the router, the auth posture, the page registry — is
//! still `sovereign_daemon::guest_door`; only what a BROWSER (or the dev
//! server standing in for one) touches lives here.

use std::path::Path;

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};

/// Where the door serves the ring page. The link a guest scans is
/// `http://<guest_bind><PAGE_PREFIX>#token=…`, or
/// `http://<guest_bind><PAGE_PREFIX><namespace>/#token=…` for a wall holding
/// more than one app.
pub const PAGE_PREFIX: &str = "/ring/";

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
///
/// Public (the source, before the per-namespace placeholders are filled) so
/// the door's pin tests read the exact text a page is served.
pub const RING_SHIM: &str = r#"(function () {
  // null: `svrn ring dev` holds the grant and proxies `/__ring/<op>`.
  // A string: the rail's base, reached with the fragment's bearer.
  const RAIL = {{RAIL_BASE}};
  const bearer = RAIL === null ? null : new URLSearchParams(location.hash.slice(1)).get('token');
  // WHICH app this page is, as the door served it — never anything the page
  // chose. A grant scoped to one namespace answers that from the grant, but a
  // WALL grant names none by design (one code, every app), so the rail routes
  // refuse a request that does not say which app it means. This is where the
  // page says it: the namespace the door mounted this shim under.
  const NS = "{{NAMESPACE}}";
  const railUrl = (path) =>
    RAIL + (NS && path.startsWith('/v1/rail/') ? path + '?namespace=' + encodeURIComponent(NS) : path);
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
    if (method === 'GET') return fetch(railUrl(path), { method, headers });
    headers['content-type'] = ctype;
    return fetch(railUrl(path), { method, headers, body });
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
