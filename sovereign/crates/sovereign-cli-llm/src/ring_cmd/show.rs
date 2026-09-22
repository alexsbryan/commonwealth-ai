// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn ring show` — open one ring app on this machine, holding its grant.
//!
//! Split from the verb's other subcommands because it is a different kind
//! of thing: they run and exit, this one binds a port and stays. It is also
//! the only part that ever holds a credential.

use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    extract::{Path as AxPath, State},
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};

use super::{
    daemon_client_port, flag, http_client, mint_rail_grant, rail_log, RAIL_APPEND_PATH,
    RAIL_LIVE_PATH, RAIL_LOG_PATH,
};

struct RingCtx {
    bundle_dir: PathBuf,
    base: String,
    token: String,
    namespace: String,
    http: reqwest::Client,
}

pub(super) async fn run_show(args: &[String]) -> i32 {
    let Some(namespace) = args.first().filter(|a| !a.starts_with("--")).cloned() else {
        eprintln!("ring show: which app? `svrn ring show <namespace>`");
        return 2;
    };
    let port: u16 = flag(args, "--port")
        .and_then(|s| s.parse().ok())
        .unwrap_or(4318);
    // The bundle defaults to `./<namespace>`: `ring new my-doc` writes ./my-doc,
    // so the next command must not need --dir typed a second time.
    let bundle_dir = flag(args, "--dir")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(&namespace));
    if !bundle_dir.join("index.html").is_file() {
        eprintln!(
            "ring show: no index.html in {} — scaffold one with `svrn ring new <dir>`, or pass --dir.",
            bundle_dir.display()
        );
        return 1;
    }

    // Fail before binding if the daemon is not there: a dev server that
    // serves a page which cannot reach its journal looks like it worked.
    if let Err(e) = rail_log(&namespace).await {
        eprintln!("ring show: the daemon is not serving this app: {e}");
        eprintln!("  start it with `svrn daemon start`, then try again.");
        return 1;
    }
    let token = match mint_rail_grant(&namespace).await {
        Ok(t) => t,
        Err(e) => {
            eprintln!("ring show: could not mint a rail grant: {e}");
            return 1;
        }
    };
    let http = match http_client() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ring show: {e}");
            return 1;
        }
    };
    // The RAIL listener, not the operator one. This matters and it is easy to
    // get wrong: `:9741` admits a loopback caller BEFORE it reads a bearer, so
    // an app pointed there would arrive as an operator with its grant ignored —
    // the namespace scoping would be decorative, and a guard nobody can watch
    // fail is not a guard (ARCH §18.1). The rail bind carries
    // `UNTRUSTED_LOOPBACK`: the token is the only way in.
    let rail = commonwealth_core::config::rail_port(daemon_client_port());
    let ctx = Arc::new(RingCtx {
        bundle_dir,
        base: format!("http://127.0.0.1:{rail}"),
        token,
        namespace: namespace.clone(),
        http,
    });

    let app = Router::new()
        .route("/__ring/{op}", post(op_handler))
        .route("/__ring_dev.js", get(shim_handler))
        .fallback(static_handler)
        .with_state(ctx.clone());

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("ring show: bind {addr}: {e} (try --port)");
            return 1;
        }
    };
    println!("ring `{namespace}` is live.");
    println!("  bundle : {}", ctx.bundle_dir.display());
    println!("  rail   : {}", ctx.base);
    println!("  open   : http://{addr}/   (Ctrl-C to stop)");
    println!();
    println!("  The grant this server holds reaches `{namespace}` and nothing else on");
    println!("  the daemon, and it dies with this process. The browser never sees it.");
    if let Err(e) = axum::serve(listener, app.into_make_service()).await {
        eprintln!("ring show: server error: {e}");
        return 1;
    }
    0
}

/// **The whole op table: four ops, because the rail is three routes and the
/// live one is two directions.**
///
/// `meshapp dev`'s equivalent has twelve arms, one per bridge call, because
/// each is a different query over a corpus. A ring app is not querying — it is
/// appending to and reading one log, and telling peers where its cursor is —
/// so a fifth op here would mean the rail had grown another route, and that is
/// where the decision belongs. The app decides what an op MEANS; this proxy
/// only carries it, with the credential attached (which is the one thing the
/// browser must not hold).
///
/// The live lane is TWO ops rather than one carrying a direction, because its
/// push body reaches the daemon verbatim as opaque text
/// (`sovereign-api/src/routes_rail_live.rs` `live_push`) and so cannot carry a
/// field of ours. `None` for a content-type is a request with no body, which
/// is what both GETs are.
///
/// It is a FUNCTION rather than arms inlined in [`op_handler`] so the table
/// can be asserted without binding a socket: a row that pointed the live push
/// at the append route would put presence in the journal, and that is the one
/// failure this lane exists to rule out.
///
/// **Why these rows are not [`rail_log`] and
/// [`rail_append`](super::rail_append).** Those are the OPERATOR clients: they
/// hit `:9741`, which admits a loopback caller before it reads a bearer, and
/// they take a typed act and hand back parsed JSON. This is a reverse proxy on
/// the RAIL listener, whose whole guarantee is that the grant is the only way
/// in — so calling them here would drop the token, move the request to the
/// operator surface, and make the namespace scoping decorative. It would also
/// have to re-parse and re-serialise the browser's body to fit their
/// signatures, when the contract is to pass those bytes through unread and
/// return the daemon's status and text verbatim. What IS shared is the route
/// constants (ARCH §10.6): one spelling of each path, every caller.
fn upstream(op: &str) -> Option<(reqwest::Method, &'static str, Option<&'static str>)> {
    match op {
        "log" => Some((reqwest::Method::GET, RAIL_LOG_PATH, None)),
        "append" => Some((
            reqwest::Method::POST,
            RAIL_APPEND_PATH,
            Some("application/json"),
        )),
        "live" => Some((reqwest::Method::POST, RAIL_LIVE_PATH, Some("text/plain"))),
        "live-drain" => Some((reqwest::Method::GET, RAIL_LIVE_PATH, None)),
        _ => None,
    }
}

async fn op_handler(
    AxPath(op): AxPath<String>,
    State(ctx): State<Arc<RingCtx>>,
    body: axum::body::Bytes,
) -> Response {
    let Some((method, path, ctype)) = upstream(&op) else {
        return (
            StatusCode::NOT_FOUND,
            format!(
                "ring show: no op `{op}` — this rail carries `log`, `append`, `live` and \
                 `live-drain`, and an app's own vocabulary is built out of those four"
            ),
        )
            .into_response();
    };
    let mut req = ctx
        .http
        .request(method, format!("{}{path}", ctx.base))
        .bearer_auth(&ctx.token);
    // A body only where the op has one: the two GETs carry the browser's
    // bytes nowhere, and a content-type on a bodiless request is a lie.
    if let Some(ctype) = ctype {
        req = req.header(header::CONTENT_TYPE, ctype).body(body);
    }
    let result = req.send().await;
    match result {
        Ok(resp) => {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            (
                StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
                [(header::CONTENT_TYPE, "application/json")],
                text,
            )
                .into_response()
        }
        // The app must be able to tell "the daemon said no" from "the daemon
        // is gone" — one is a bug in the app, the other is a bug in the setup.
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            [(header::CONTENT_TYPE, "application/json")],
            serde_json::json!({ "error": format!("ring show: the daemon is unreachable: {e}") })
                .to_string(),
        )
            .into_response(),
    }
}

async fn shim_handler(State(ctx): State<Arc<RingCtx>>) -> Response {
    let js = sovereign_daemon::guest_door::ring_shim(&ctx.namespace, None);
    ([(header::CONTENT_TYPE, "text/javascript")], js).into_response()
}

async fn static_handler(State(ctx): State<Arc<RingCtx>>, uri: Uri) -> Response {
    let rel = uri.path().trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    let shim = (rel == "index.html").then_some("/__ring_dev.js");
    crate::meshapp_cmd::serve_under(&ctx.bundle_dir, rel, shim)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The live lane's two ops must point at the live route, and only at
    /// it.** A `live` row pointing anywhere else is presence written down —
    /// the one thing the lane exists to rule out — and it would look like a
    /// working page until somebody read the journal.
    #[test]
    fn the_op_table_carries_both_directions_of_the_live_lane() {
        assert_eq!(
            upstream("live"),
            Some((reqwest::Method::POST, RAIL_LIVE_PATH, Some("text/plain")))
        );
        assert_eq!(
            upstream("live-drain"),
            Some((reqwest::Method::GET, RAIL_LIVE_PATH, None))
        );
        assert_eq!(
            upstream("log"),
            Some((reqwest::Method::GET, RAIL_LOG_PATH, None))
        );
        assert_eq!(
            upstream("append"),
            Some((
                reqwest::Method::POST,
                RAIL_APPEND_PATH,
                Some("application/json")
            ))
        );
        assert!(upstream("nope").is_none());
    }
}
