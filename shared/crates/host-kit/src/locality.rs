// SPDX-License-Identifier: AGPL-3.0-or-later
//! Whether a request came from a process on this machine: the one decider
//! every site that trusts a loopback caller reads.
//!
//! A loopback peer address used to be the whole test, and a browser on the
//! owner's machine is a loopback peer. So any page the owner had open reached
//! every route that trusts loopback: `/mcp` answered `OPTIONS` from
//! `https://evil.example` with `access-control-allow-origin: *` and served
//! that origin `tools/list` (observed against the running daemon,
//! 2026-10-08). A page served under a name that resolves to 127.0.0.1 needs
//! no CORS at all, because to the browser it IS the daemon's origin (DNS
//! rebinding). MCP's Streamable HTTP transport requires the check: "Servers
//! MUST validate the `Origin` header on all incoming connections"
//! (revision 2025-06-18).
//!
//! A request is [`RequestLocality::Local`] only when all four hold:
//!
//! 1. its peer address is loopback;
//! 2. its `Host`, when present, names loopback: `localhost`, or an IP
//!    literal that is loopback. A browser always sends `Host`, and a
//!    rebinding page's is its own name, so a request with none is no page;
//! 3. its `Origin`, when present, is the request's own origin: the
//!    authority equals `Host`;
//! 4. its `Sec-Fetch-Site`, when present, is `same-origin` or `none`. A
//!    no-cors cross-site `GET` carries no `Origin`, but it carries this.
//!
//! The owner's own tools send none of the browser headers this reads. curl
//! and reqwest send `Host` alone. Node's `fetch` adds `sec-fetch-mode: cors`
//! and nothing else here (recorded 2026-10-08, node v20.20.2, against a
//! listener on 127.0.0.1).

use std::net::{IpAddr, SocketAddr};

use axum::http::{header, HeaderMap, HeaderName, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;

/// `Sec-Fetch-Site`, which `http` does not name.
const SEC_FETCH_SITE: HeaderName = HeaderName::from_static("sec-fetch-site");

/// Where a request came from, as far as trusting it as local goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestLocality {
    /// A process on this machine.
    Local,
    /// The peer address is not loopback.
    Remote,
    /// A loopback peer, addressed to a name that is not loopback: a page
    /// served under a name that resolved to this machine, or a proxy on this
    /// host forwarding someone else's request. Carries the `Host`.
    ForeignHost(String),
    /// A loopback peer, sent by a web page of another origin. Carries what
    /// said so: the `Origin`, or `sec-fetch-site: <value>` when no `Origin`
    /// came.
    CrossOrigin(String),
}

impl RequestLocality {
    /// The one decision. Pure, so every site that trusts loopback reads the
    /// same answer, and a test needs no listener.
    pub fn of(peer: &SocketAddr, headers: &HeaderMap) -> Self {
        if !peer.ip().is_loopback() {
            return Self::Remote;
        }
        let host = match headers.get(header::HOST).map(|v| v.to_str()) {
            None => None,
            Some(Ok(h)) if names_loopback(h) => Some(h),
            Some(Ok(h)) => return Self::ForeignHost(h.to_string()),
            Some(Err(_)) => return Self::ForeignHost("<not visible ASCII>".to_string()),
        };
        if let Some(origin) = headers.get(header::ORIGIN) {
            let Ok(origin) = origin.to_str() else {
                return Self::CrossOrigin("<not visible ASCII>".to_string());
            };
            if !is_own_origin(origin, host) {
                return Self::CrossOrigin(origin.to_string());
            }
        }
        match headers.get(SEC_FETCH_SITE).map(|v| v.to_str()) {
            None | Some(Ok("same-origin")) | Some(Ok("none")) => Self::Local,
            Some(Ok(site)) => Self::CrossOrigin(format!("sec-fetch-site: {site}")),
            Some(Err(_)) => Self::CrossOrigin("sec-fetch-site: <not visible ASCII>".to_string()),
        }
    }

    /// Whether the request may be trusted as a process on this machine.
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local)
    }
}

/// What a web page of another origin is told, wherever a guard answers in
/// plain JSON: `403 {"error": "cross-origin", "origin": …}`. One body for
/// every site (ARCH 8), and never the `local-only` a stranger gets, so the
/// operator can tell the two apart (ARCH 6).
pub fn cross_origin_refusal(origin: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(serde_json::json!({ "error": "cross-origin", "origin": origin })),
    )
        .into_response()
}

/// Whether a `Host` (or an origin's authority) names this machine:
/// `localhost`, or an IP literal that is loopback, with or without a port.
fn names_loopback(authority: &str) -> bool {
    let name = match authority.strip_prefix('[') {
        // `[::1]:9741`: the literal is everything inside the brackets.
        Some(rest) => match rest.split_once(']') {
            Some((literal, _)) => literal,
            None => return false,
        },
        None => match authority.rsplit_once(':') {
            Some((name, port)) if port.bytes().all(|b| b.is_ascii_digit()) => name,
            _ => authority,
        },
    };
    name.eq_ignore_ascii_case("localhost")
        || name.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Whether `origin` is the request's own: its authority equals the `Host`
/// the request was sent to. With no `Host` to compare, it must at least name
/// this machine. `null` (a sandboxed frame, a file) is never anyone's own.
fn is_own_origin(origin: &str, host: Option<&str>) -> bool {
    let Some(authority) = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
    else {
        return false;
    };
    match host {
        Some(host) => authority.eq_ignore_ascii_case(host),
        None => names_loopback(authority),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn at(peer: &str, headers: &[(&str, &str)]) -> RequestLocality {
        let mut map = HeaderMap::new();
        for (k, v) in headers {
            map.insert(
                HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        RequestLocality::of(&peer.parse().unwrap(), &map)
    }

    const LO: &str = "127.0.0.1:50000";

    #[test]
    fn a_process_on_this_machine_is_local() {
        for headers in [
            &[][..],
            &[("host", "127.0.0.1:9741")][..],
            &[("host", "localhost:9741")][..],
            &[("host", "LOCALHOST")][..],
            &[("host", "[::1]:9741")][..],
            &[("host", "127.1.2.3:9741")][..],
            // Node's fetch, as recorded.
            &[
                ("host", "127.0.0.1:9741"),
                ("sec-fetch-mode", "cors"),
                ("user-agent", "node"),
            ][..],
            // A page the daemon itself served, and an address-bar navigation.
            &[
                ("host", "localhost:9741"),
                ("origin", "http://localhost:9741"),
                ("sec-fetch-site", "same-origin"),
            ][..],
            &[("host", "127.0.0.1:9741"), ("sec-fetch-site", "none")][..],
        ] {
            assert_eq!(at(LO, headers), RequestLocality::Local, "{headers:?}");
        }
        assert_eq!(
            at("[::1]:50000", &[("host", "[::1]:9741")]),
            RequestLocality::Local
        );
    }

    #[test]
    fn a_peer_off_this_machine_is_remote_whatever_it_says() {
        assert_eq!(
            at("192.168.1.7:50000", &[("host", "localhost:9741")]),
            RequestLocality::Remote
        );
        assert_eq!(at("100.64.0.2:50000", &[]), RequestLocality::Remote);
    }

    #[test]
    fn a_rebinding_page_is_a_foreign_host() {
        let got = at(
            LO,
            &[
                ("host", "evil.example:9741"),
                ("origin", "http://evil.example:9741"),
                ("sec-fetch-site", "same-origin"),
            ],
        );
        assert_eq!(
            got,
            RequestLocality::ForeignHost("evil.example:9741".into())
        );
        // A name that merely starts like loopback is not loopback.
        assert!(matches!(
            at(LO, &[("host", "localhost.evil.example")]),
            RequestLocality::ForeignHost(_)
        ));
        assert!(matches!(
            at(LO, &[("host", "127.0.0.1.evil.example:9741")]),
            RequestLocality::ForeignHost(_)
        ));
    }

    #[test]
    fn a_page_of_another_origin_is_cross_origin() {
        let evil = at(
            LO,
            &[
                ("host", "127.0.0.1:9741"),
                ("origin", "https://evil.example"),
            ],
        );
        assert_eq!(
            evil,
            RequestLocality::CrossOrigin("https://evil.example".into())
        );
        // Another port on this machine is another origin (a dev server, a ring app).
        assert!(matches!(
            at(
                LO,
                &[
                    ("host", "127.0.0.1:9741"),
                    ("origin", "http://127.0.0.1:5173")
                ]
            ),
            RequestLocality::CrossOrigin(_)
        ));
        assert!(matches!(
            at(LO, &[("host", "127.0.0.1:9741"), ("origin", "null")]),
            RequestLocality::CrossOrigin(_)
        ));
        // No Origin, but the browser says it was cross-site, or same-site.
        assert_eq!(
            at(
                LO,
                &[("host", "127.0.0.1:9741"), ("sec-fetch-site", "cross-site")]
            ),
            RequestLocality::CrossOrigin("sec-fetch-site: cross-site".into())
        );
        assert!(matches!(
            at(
                LO,
                &[("host", "127.0.0.1:9741"), ("sec-fetch-site", "same-site")]
            ),
            RequestLocality::CrossOrigin(_)
        ));
    }
}
