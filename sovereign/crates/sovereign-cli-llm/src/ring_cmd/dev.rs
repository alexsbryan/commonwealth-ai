// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn ring dev` — serve one ring app, holding its grant.
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

use super::{daemon_client_port, flag, http_client, mint_rail_grant, rail_log};

struct RingCtx {
    bundle_dir: PathBuf,
    base: String,
    token: String,
    namespace: String,
    http: reqwest::Client,
}

pub(super) async fn run_dev(args: &[String]) -> i32 {
    let Some(namespace) = args.first().filter(|a| !a.starts_with("--")).cloned() else {
        eprintln!("ring dev: which ring? `svrn ring dev <namespace>`");
        return 2;
    };
    let port: u16 = flag(args, "--port")
        .and_then(|s| s.parse().ok())
        .unwrap_or(4318);
    let bundle_dir = flag(args, "--dir")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    if !bundle_dir.join("index.html").is_file() {
        eprintln!(
            "ring dev: no index.html in {} — scaffold one with `svrn ring new <dir>`, or pass --dir.",
            bundle_dir.display()
        );
        return 1;
    }

    // Fail before binding if the daemon is not there: a dev server that
    // serves a page which cannot reach its journal looks like it worked.
    if let Err(e) = rail_log(&namespace).await {
        eprintln!("ring dev: the daemon is not serving this ring: {e}");
        eprintln!("  start it with `svrn daemon start`, then try again.");
        return 1;
    }
    let token = match mint_rail_grant(&namespace).await {
        Ok(t) => t,
        Err(e) => {
            eprintln!("ring dev: could not mint a rail grant: {e}");
            return 1;
        }
    };
    let http = match http_client() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ring dev: {e}");
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
            eprintln!("ring dev: bind {addr}: {e} (try --port)");
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
        eprintln!("ring dev: server error: {e}");
        return 1;
    }
    0
}

/// **The whole op table: two ops, because the rail is two routes.**
///
/// `meshapp dev`'s equivalent has twelve arms, one per bridge call, because
/// each is a different query over a corpus. A ring app is not querying — it is
/// appending to and reading one log — so a third arm here would mean the rail
/// had grown a third route, and that is where the decision belongs. The app
/// decides what an op MEANS; this proxy only carries it, with the credential
/// attached (which is the one thing the browser must not hold).
async fn op_handler(
    AxPath(op): AxPath<String>,
    State(ctx): State<Arc<RingCtx>>,
    body: axum::body::Bytes,
) -> Response {
    let result = match op.as_str() {
        "log" => {
            ctx.http
                .get(format!("{}/v1/rail/log", ctx.base))
                .bearer_auth(&ctx.token)
                .send()
                .await
        }
        "append" => {
            ctx.http
                .post(format!("{}/v1/rail/append", ctx.base))
                .bearer_auth(&ctx.token)
                .header(header::CONTENT_TYPE, "application/json")
                .body(body)
                .send()
                .await
        }
        other => {
            return (
                StatusCode::NOT_FOUND,
                format!(
                    "ring dev: no op `{other}` — this rail carries `log` and `append`, \
                     and an app's own vocabulary is built out of those two"
                ),
            )
                .into_response()
        }
    };
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
            serde_json::json!({ "error": format!("ring dev: the daemon is unreachable: {e}") })
                .to_string(),
        )
            .into_response(),
    }
}

async fn shim_handler(State(ctx): State<Arc<RingCtx>>) -> Response {
    let js = DEV_SHIM.replace("{{NAMESPACE}}", &ctx.namespace);
    ([(header::CONTENT_TYPE, "text/javascript")], js).into_response()
}

async fn static_handler(State(ctx): State<Arc<RingCtx>>, uri: Uri) -> Response {
    let rel = uri.path().trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    let shim = (rel == "index.html").then_some("/__ring_dev.js");
    crate::meshapp_cmd::serve_under(&ctx.bundle_dir, rel, shim)
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
const DEV_SHIM: &str = r#"(function () {
  const call = async (op, body) => {
    const r = await fetch('/__ring/' + op, {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify(body || {}),
    });
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
    // reading it.
    log: () => call('log', {}),
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
        assert!(DEV_SHIM.contains("fold: (log, reducer, initial)"));
        assert!(
            DEV_SHIM.contains("if (op.voided || op.payload == null) continue;"),
            "the fold stopped skipping a non-act — corrections would be counted"
        );
    }

    /// The shim carries the two routes and nothing that pretends to be a
    /// third. An app's vocabulary is built out of `record` and `correct`; a
    /// verb here that the rail does not have is a verb that fails at runtime.
    #[test]
    fn the_shim_offers_exactly_the_rails_two_writes() {
        assert!(DEV_SHIM.contains("record: (payload)"));
        assert!(DEV_SHIM.contains("correct: (correctsId, replacement)"));
        assert!(
            !DEV_SHIM.contains("expense:") && !DEV_SHIM.contains("settle:"),
            "the shim knows about money again — that belongs in the app's own \
             module, where it has tests"
        );
    }
}
