// SPDX-License-Identifier: AGPL-3.0-or-later
//! v0.5's auth checks (§5): who is asking. `auth.named_client` holds a
//! presented credential to its verdict from any address; `auth.local_peer`
//! holds that a web page is never a local process, and runs against every
//! host because every host that trusts loopback owes it.

use oicp_types::{features, ProviderManifest};
use reqwest::header::{HOST, ORIGIN};
use reqwest::Method;
use serde_json::{json, Value};

use crate::args::Args;
use crate::checks::Host;
use crate::fixture::sha256_hex;
use crate::report::{Check, Level};
use crate::wire::{send, Reply};

/// The foreign origin every browser probe claims.
pub const EVIL: &str = "https://evil.example";

/// A credential in the reference host's form (`svrn_` + 64 lowercase hex,
/// §5.1) that no host issued. `--bogus-token` replaces it for a host whose
/// credentials take another form.
pub fn default_bogus_token() -> String {
    format!(
        "svrn_{}",
        sha256_hex(b"oicp-conformance: a credential no host issued")
    )
}

/// One route a probe is sent to.
struct Probe {
    label: &'static str,
    method: Method,
    path: String,
    body: Option<Value>,
}

fn mcp_list() -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})
}

/// The protected routes this host has: the OpenAI baseline, every OICP
/// route the manifest advertises that is safe to call, and `/mcp`.
fn probes(m: Option<&ProviderManifest>) -> Vec<Probe> {
    let mut out = vec![Probe {
        label: "GET /v1/models",
        method: Method::GET,
        path: "/v1/models".into(),
        body: None,
    }];
    if let Some(k) = m.and_then(|m| m.knowledge.as_ref()) {
        out.push(Probe {
            label: "POST search_endpoint",
            method: Method::POST,
            path: k.search_endpoint.clone(),
            body: Some(json!({"query": "conformance probe", "limit": 1})),
        });
        if let Some(i) = &k.ingest {
            out.push(Probe {
                label: "GET progress_endpoint",
                method: Method::GET,
                path: i.progress_endpoint.clone(),
                body: None,
            });
        }
        if let Some(a) = k.evidence.as_ref().and_then(|e| e.align_endpoint.clone()) {
            out.push(Probe {
                label: "POST align_endpoint",
                method: Method::POST,
                path: a,
                body: Some(json!({"quote": "conformance probe"})),
            });
        }
    }
    out.push(Probe {
        label: "POST /mcp",
        method: Method::POST,
        path: "/mcp".into(),
        body: Some(mcp_list()),
    });
    out
}

/// Send probe `p` with no credential and `headers`, optionally a bearer.
async fn fire(
    host: &Host,
    p: &Probe,
    headers: &[(&str, String)],
    bearer: Option<&str>,
) -> Result<Reply, String> {
    let mut rb = match bearer {
        Some(b) => host.with_bearer(p.method.clone(), &p.path, b),
        None => host.bare(p.method.clone(), &p.path),
    };
    for (k, v) in headers {
        rb = rb.header(*k, v);
    }
    if let Some(body) = &p.body {
        rb = rb
            .header("accept", "application/json, text/event-stream")
            .json(body);
    }
    send(rb).await
}

/// `auth.named_client` (§5.1, rules 1-3): a bearer in the host's credential
/// form that no host issued is a 401 from any address, loopback included,
/// on every protected route and `/mcp`; a named token is admitted; a revoked
/// one is a 401. Rule 2's name in the log is the host's own witness: no wire
/// shows it.
pub async fn check_auth_named_client(host: &Host, m: &ProviderManifest, args: &Args) -> Check {
    let id = "auth.named_client";
    if !m.has_feature(features::AUTH_NAMED_CLIENT) {
        return Check::skip(id, Level::Feature, "auth:named_client not advertised");
    }
    let bogus = args.bogus_token.clone().unwrap_or_else(default_bogus_token);
    let mut judged = Vec::new();
    for p in probes(Some(m)) {
        let control = match fire(host, &p, &[], None).await {
            Ok(r) => r,
            Err(e) => return Check::fail(id, Level::Feature, e),
        };
        if matches!(control.status, 404 | 405) {
            continue;
        }
        match fire(host, &p, &[], Some(&bogus)).await {
            Ok(r) if r.status == 401 => judged.push(p.label),
            Ok(r) => {
                return Check::fail(
                    id,
                    Level::Feature,
                    format!(
                        "{}: an unissued credential must be 401, got {}",
                        p.label,
                        r.brief()
                    ),
                )
            }
            Err(e) => return Check::fail(id, Level::Feature, e),
        }
    }
    if judged.is_empty() {
        return Check::fail(id, Level::Feature, "no protected route answered at all");
    }
    let models = Probe {
        label: "GET /v1/models",
        method: Method::GET,
        path: "/v1/models".into(),
        body: None,
    };
    let mut notes = vec![format!("unissued credential 401 on {judged:?}")];
    for (flag, token, want_admitted) in [
        ("--named-token", &args.named_token, true),
        ("--revoked-token", &args.revoked_token, false),
    ] {
        let Some(token) = token else {
            notes.push(format!("{flag} not given: not judged"));
            continue;
        };
        match fire(host, &models, &[], Some(token)).await {
            Ok(r) if want_admitted && r.admitted() => notes.push(format!("{flag} admitted")),
            Ok(r) if !want_admitted && r.status == 401 => notes.push(format!("{flag} 401")),
            Ok(r) => {
                return Check::fail(
                    id,
                    Level::Feature,
                    format!(
                        "{flag}: want {}, got {}",
                        if want_admitted { "admitted" } else { "401" },
                        r.brief()
                    ),
                )
            }
            Err(e) => return Check::fail(id, Level::Feature, e),
        }
    }
    Check::pass(id, Level::Feature, notes.join("; "))
}

/// Whether a reply lets `origin` read it.
fn grants(r: &Reply, origin: &str) -> bool {
    r.headers
        .get("access-control-allow-origin")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == "*" || v == origin)
}

/// `auth.local_peer` (§5.2, rule 4): a request carrying a foreign `Origin`,
/// `Sec-Fetch-Site: cross-site` or a non-loopback `Host` is not admitted as
/// local on any protected route or `/mcp`; where the host trusts this
/// client as local, the first two are refused by name (`403 cross-origin`);
/// and no foreign origin is granted `Access-Control-Allow-Origin`.
pub async fn check_auth_local_peer(host: &Host, m: Option<&ProviderManifest>) -> Check {
    let id = "auth.local_peer";
    let own = host.authority();
    let evil_host = format!(
        "evil.example:{}",
        own.rsplit_once(':').map(|(_, p)| p).unwrap_or("80")
    );
    let mut judged = Vec::new();
    let mut trusted = Vec::new();
    for p in probes(m) {
        let fail = |why: String| Check::fail(id, Level::Must, format!("{}: {why}", p.label));
        let control = match fire(host, &p, &[], None).await {
            Ok(r) => r,
            Err(e) => return fail(e),
        };
        if matches!(control.status, 404 | 405) {
            continue;
        }
        let local = control.admitted();
        if local {
            trusted.push(p.label);
        }
        let cases: [(&str, Vec<(&str, String)>, bool); 3] = [
            (
                "a foreign Origin",
                vec![(HOST.as_str(), own.clone()), (ORIGIN.as_str(), EVIL.into())],
                true,
            ),
            (
                "Sec-Fetch-Site: cross-site",
                vec![
                    (HOST.as_str(), own.clone()),
                    ("sec-fetch-site", "cross-site".into()),
                ],
                true,
            ),
            (
                "a foreign Host",
                vec![(HOST.as_str(), evil_host.clone())],
                false,
            ),
        ];
        for (what, headers, named) in cases {
            let r = match fire(host, &p, &headers, None).await {
                Ok(r) => r,
                Err(e) => return fail(e),
            };
            if r.admitted() {
                return fail(format!(
                    "{what} from this client was admitted ({})",
                    r.brief()
                ));
            }
            if local && named && !(r.status == 403 && r.error() == Some("cross-origin")) {
                return fail(format!(
                    "{what} must be refused as `403 cross-origin`, got {}",
                    r.brief()
                ));
            }
            if grants(&r, EVIL) {
                return fail(format!("{what}: the refusal still grants {EVIL} the reply"));
            }
        }
        let preflight = host
            .bare(Method::OPTIONS, &p.path)
            .header(ORIGIN, EVIL)
            .header("access-control-request-method", p.method.as_str())
            .header(
                "access-control-request-headers",
                "content-type, authorization",
            );
        match send(preflight).await {
            Ok(r) if grants(&r, EVIL) => {
                return fail(format!(
                    "a preflight from {EVIL} was granted ({})",
                    r.brief()
                ))
            }
            Ok(_) => {}
            Err(e) => return fail(e),
        }
        judged.push(p.label);
    }
    if judged.is_empty() {
        return Check::skip(
            id,
            Level::Must,
            "could not judge: no protected route answered",
        );
    }
    let trust = if trusted.is_empty() {
        "this client is trusted as local nowhere, so refusal by name was not exercised".to_string()
    } else {
        format!("refused by name where trusted ({trusted:?})")
    };
    Check::pass(
        id,
        Level::Must,
        format!("no page admitted on {judged:?}; {trust}"),
    )
}
