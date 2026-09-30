// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one per-stream router under every "which local origin does THIS
//! request go to" forward: the app forward, which picks by the first path
//! segment ([`crate::iroh_identity_forward::pump_by_name`]), and any later
//! rule that picks by something else in the first request head.
//!
//! Split out of `iroh_identity_forward` (phase-b pb-rails-origins) so a
//! second rule is a second ROUTER, never a second pump (ARCH principle 8).
//! The pump owns what every rule shares: the first head binds the stream,
//! every head is rewritten with the verified identity, bodies are framed by
//! `Content-Length`, responses are a byte copy, and a later head that routes
//! somewhere else closes the stream — see `pump_by_name` for why the binding
//! is per stream.

use std::net::SocketAddr;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::iroh_identity_forward::{body_framing, read_head, rewrite_head, BodyFraming};

/// Where a stream's request goes: the key it bound under (an app name, a
/// prefix), the local origin, the head to send (a rule may rewrite the
/// target), and headers this origin alone is handed on top of the identity.
pub struct Routed {
    pub key: String,
    pub origin: SocketAddr,
    pub head: Vec<u8>,
    pub extra: Vec<(String, String)>,
}

/// Why a request goes nowhere, answered on the stream itself: an HTTP status
/// and a reason the dialer reads.
pub struct Unrouted {
    pub status: u16,
    pub key: String,
    pub why: String,
}

/// Pump one accepted bi-stream to the origin `route` picks for its FIRST
/// request head. `what` names the rule in every log line ("app", "origin");
/// `listing` is what a refusal may tell the dialer this node serves, `None`
/// where the dialer is not entitled to the list.
pub async fn pump_routed(
    mut send: iroh::endpoint::SendStream,
    recv: iroh::endpoint::RecvStream,
    headers: std::sync::Arc<Vec<(String, String)>>,
    what: &'static str,
    listing: Option<String>,
    route: impl Fn(&[u8]) -> Result<Routed, Unrouted>,
) {
    let mut reader = tokio::io::BufReader::new(recv);
    let mut buf = Vec::with_capacity(4096);
    // The first head decides where this stream goes, so it is read before any
    // TCP connection exists — which is also why the refusals below answer on
    // `send` directly instead of relaying an origin's answer.
    match read_head(&mut reader, &mut buf).await {
        Ok(true) => {}
        Ok(false) => return,
        Err(e) => {
            tracing::info!(
                target: "transport",
                error = %e,
                "iroh acceptor: {what} forward closed a request it could not frame"
            );
            return;
        }
    }
    let routed = match route(&buf) {
        Ok(r) => r,
        Err(u) => {
            tracing::info!(
                target: "transport",
                key = %u.key,
                status = u.status,
                "{what}: REFUSED a request — {}",
                u.why
            );
            say(
                refuse(&mut send, u.status, &u.why, listing.as_deref()).await,
                what,
                &u.key,
            );
            return;
        }
    };
    let Routed {
        key,
        origin,
        head: first_head,
        extra,
    } = routed;
    let tcp = match tokio::net::TcpStream::connect(origin).await {
        Ok(t) => {
            t.set_nodelay(true).ok();
            t
        }
        Err(e) => {
            // The registry says this is published and the process behind it
            // is gone. That is a 502 and it is NAMED: a stale registration
            // reading as "no such app" would send the operator looking for a
            // config bug that is not there.
            tracing::warn!(
                target: "transport",
                key = %key,
                origin = %origin,
                error = %e,
                "{what}: published origin did not accept — the registration outlived its process"
            );
            say(
                refuse(
                    &mut send,
                    502,
                    &format!("{what} {key:?} is published on {origin} but did not accept"),
                    listing.as_deref(),
                )
                .await,
                what,
                &key,
            );
            return;
        }
    };
    tracing::info!(
        target: "transport",
        key = %key,
        origin = %origin,
        "{what}: dial admitted — the origin is told who is asking"
    );
    let mut all = headers.as_ref().clone();
    all.extend(extra);
    let (mut tcp_r, mut tcp_w) = tcp.into_split();
    let down = async {
        let _ = tokio::io::copy(&mut tcp_r, &mut send).await;
        let _ = send.finish();
    };
    let up = async {
        let mut head = first_head;
        loop {
            let framing = body_framing(&head);
            let (out, stripped) = rewrite_head(&head, &all);
            if stripped > 0 {
                tracing::info!(
                    target: "transport",
                    stripped,
                    "iroh acceptor: dropped client-supplied x-mesh-* header(s) before adding the verified identity"
                );
            }
            if tcp_w.write_all(&out).await.is_err() {
                break;
            }
            match framing {
                BodyFraming::None => {}
                BodyFraming::Length(n) => {
                    let mut body = (&mut reader).take(n);
                    if tokio::io::copy(&mut body, &mut tcp_w).await.is_err() {
                        break;
                    }
                }
                BodyFraming::Chunked => {
                    tracing::info!(
                        target: "transport",
                        "iroh acceptor: chunked request body — the rest of this connection passes through unrewritten"
                    );
                    let _ = tokio::io::copy(&mut reader, &mut tcp_w).await;
                    break;
                }
            }
            match read_head(&mut reader, &mut buf).await {
                Ok(true) => {}
                Ok(false) => break,
                Err(e) => {
                    tracing::info!(
                        target: "transport",
                        error = %e,
                        "iroh acceptor: {what} forward closed a request it could not frame"
                    );
                    break;
                }
            }
            match route(&buf) {
                Ok(next) if next.key == key => head = next.head,
                other => {
                    tracing::info!(
                        target: "transport",
                        bound = %key,
                        requested = other.as_ref().map(|r| r.key.as_str()).unwrap_or_else(|u| u.key.as_str()),
                        "{what}: closing a kept-alive stream that changed {what} mid-connection — \
                         the binding is per stream (see pump_by_name)"
                    );
                    break;
                }
            }
        }
        let _ = tcp_w.shutdown().await;
    };
    tokio::join!(up, down);
}

/// Report a refusal that could not be DELIVERED.
///
/// The dialer then sees a connection that closed with no answer, which looks
/// exactly like the origin hanging — so the reason has to survive on this
/// side even though it never reached the other. Discarding it was the shape
/// ARCH §6 names: a failure collapsed into a path that reads as handled.
fn say(sent: std::io::Result<()>, what: &str, key: &str) {
    if let Err(e) = sent {
        tracing::info!(
            target: "transport",
            key = %key,
            error = %e,
            "{what}: the refusal could not be written back — the dialer sees a silent close"
        );
    }
}

/// Answer the dialer directly, before any origin is involved. `listing`, when
/// the rule may disclose it, is appended so "no app named chore" beside
/// "chores, printer" is a typo found in one second.
async fn refuse(
    send: &mut iroh::endpoint::SendStream,
    status: u16,
    why: &str,
    listing: Option<&str>,
) -> std::io::Result<()> {
    let body = match listing {
        Some(listing) => format!("{why}\n{listing}\n"),
        None => format!("{why}\n"),
    };
    let reason = match status {
        502 => "Bad Gateway",
        403 => "Forbidden",
        400 => "Bad Request",
        _ => "Not Found",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    send.write_all(head.as_bytes()).await?;
    send.write_all(body.as_bytes()).await?;
    let _ = send.finish();
    Ok(())
}

pub use kernel_types::member::ORIGIN_TIE_HEADER;

/// The header the verified dialer key rides in
/// (`kernel_types::member::verified_headers`).
const PUBKEY_HEADER: &str = "X-Mesh-Pubkey";

/// A fresh registration tie: 32 random bytes, hex. Minted once per
/// registration, handed only to its registrant, never logged.
pub fn mint_tie() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("failed to generate a registration tie");
    hex::encode(bytes)
}

/// The registrant's half: the verified dialer key a forwarded request
/// carries, or `None` when the request is not tied to `tie`.
///
/// `header` reads one request header by name. A request with no tie, or the
/// wrong one, is a caller that typed `x-mesh-*` at a local port — its claim
/// is not believed, whatever it says (ARCH principle 6: unverified is never
/// trusted). Compared in constant time, so the check is not a timing oracle.
pub fn tied_pubkey<'a>(tie: &str, header: impl Fn(&str) -> Option<&'a str>) -> Option<&'a str> {
    let presented = header(ORIGIN_TIE_HEADER)?;
    if !commonwealth_core::ct::constant_time_eq(presented.as_bytes(), tie.as_bytes()) {
        return None;
    }
    header(PUBKEY_HEADER)
}

/// One registered path prefix on `cwth/http/0`: its origin, the headers only
/// it is handed (its registration's tie), and whether THIS dialer may reach
/// it — decided once per connection by the acceptor, so a refused prefix is
/// answered 403 by name rather than read as unregistered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefixRoute {
    pub origin: SocketAddr,
    pub headers: Vec<(String, String)>,
    pub admitted: bool,
}

/// The request target's path, when the head's request line is well formed
/// (three fields, origin-form target, an HTTP version).
fn request_path(head: &[u8]) -> Option<&[u8]> {
    let end = head.iter().position(|&b| b == b'\n').unwrap_or(head.len());
    let line = head[..end].strip_suffix(b"\r").unwrap_or(&head[..end]);
    let mut parts = line.split(|&b| b == b' ');
    let (_method, target, version) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || !version.starts_with(b"HTTP/") || !target.starts_with(b"/") {
        return None;
    }
    let cut = target
        .iter()
        .position(|&b| b == b'?')
        .unwrap_or(target.len());
    Some(&target[..cut])
}

/// Whether `path` is under `prefix` at a segment boundary: `/internal/ring`
/// covers `/internal/ring` and `/internal/ring/sync`, never `/internal/rings`.
fn covers(prefix: &str, path: &str) -> bool {
    path == prefix
        || (path.starts_with(prefix)
            && (prefix.ends_with('/') || path.as_bytes().get(prefix.len()) == Some(&b'/')))
}

/// The prefix router: the LONGEST registered prefix covering the request's
/// path, the head forwarded unchanged.
///
/// Refused by name: a malformed head (400), a path that could step out of its
/// prefix (`..` or an encoded `.`/`/`, 400), a prefix nobody registered (404),
/// and a registered prefix this dialer was not admitted to (403). The 404
/// does not list what IS registered: any dialer reaches this ALPN, and the
/// list is not a stranger's to read.
pub fn route_by_prefix(
    routes: &std::collections::BTreeMap<String, PrefixRoute>,
    head: &[u8],
) -> Result<Routed, Unrouted> {
    let Some(path) = request_path(head) else {
        return Err(Unrouted {
            status: 400,
            key: "<none>".into(),
            why: "the request line is not a path request".into(),
        });
    };
    let path = String::from_utf8_lossy(path).into_owned();
    let lower = path.to_ascii_lowercase();
    if path.split('/').any(|seg| seg == "..") || lower.contains("%2e") || lower.contains("%2f") {
        return Err(Unrouted {
            status: 400,
            key: path,
            why: "a path that can step out of its prefix is not forwarded".into(),
        });
    }
    let best = routes
        .iter()
        .filter(|(prefix, _)| covers(prefix, &path))
        .max_by_key(|(prefix, _)| prefix.len());
    match best {
        None => Err(Unrouted {
            status: 404,
            why: format!("no origin is registered for {}", truncate(&path)),
            key: path,
        }),
        Some((prefix, route)) if !route.admitted => Err(Unrouted {
            status: 403,
            why: format!("{prefix} is registered for members only"),
            key: prefix.clone(),
        }),
        Some((prefix, route)) => Ok(Routed {
            key: prefix.clone(),
            origin: route.origin,
            head: head.to_vec(),
            extra: route.headers.clone(),
        }),
    }
}

fn truncate(path: &str) -> &str {
    match path.char_indices().nth(128) {
        Some((at, _)) => &path[..at],
        None => path,
    }
}

/// Pump one accepted `cwth/http/0` bi-stream to the origin whose registered
/// prefix covers its first request.
pub async fn pump_by_prefix(
    send: iroh::endpoint::SendStream,
    recv: iroh::endpoint::RecvStream,
    routes: std::sync::Arc<std::collections::BTreeMap<String, PrefixRoute>>,
    headers: std::sync::Arc<Vec<(String, String)>>,
) {
    pump_routed(send, recv, headers, "origin", None, |head| {
        route_by_prefix(&routes, head)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn routes() -> BTreeMap<String, PrefixRoute> {
        let at = |port: u16, admitted| PrefixRoute {
            origin: ([127, 0, 0, 1], port).into(),
            headers: vec![(ORIGIN_TIE_HEADER.to_string(), format!("tie-{port}"))],
            admitted,
        };
        [
            ("/internal/ring".to_string(), at(1, true)),
            ("/internal/ring/checkpoint".to_string(), at(2, true)),
            ("/internal/index".to_string(), at(3, false)),
        ]
        .into_iter()
        .collect()
    }

    fn get(path: &str) -> Vec<u8> {
        format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").into_bytes()
    }

    /// The longest prefix wins, the head is forwarded unchanged, and the
    /// route's own tie rides with it.
    #[test]
    fn the_longest_registered_prefix_takes_the_request() {
        let r = route_by_prefix(&routes(), &get("/internal/ring/checkpoint/ns?x=1"))
            .ok()
            .unwrap();
        assert_eq!(r.key, "/internal/ring/checkpoint");
        assert_eq!(r.origin.port(), 2);
        assert_eq!(r.head, get("/internal/ring/checkpoint/ns?x=1"));
        assert_eq!(r.extra[0].1, "tie-2");
        let r = route_by_prefix(&routes(), &get("/internal/ring/sync"))
            .ok()
            .unwrap();
        assert_eq!(r.origin.port(), 1);
    }

    /// **The failing inputs.** An unregistered prefix, a sibling that only
    /// shares characters with a registered one, and a traversal each refuse
    /// by name — before this, the daemon forwarded every `/internal/*` path
    /// to any dialer (sovereign-mesh iroh_access.rs, `forward_for`).
    #[test]
    fn nothing_unregistered_is_forwarded() {
        let status = |p: &str| route_by_prefix(&routes(), &get(p)).err().map(|u| u.status);
        assert_eq!(status("/internal/pipeline/pause"), Some(404));
        assert_eq!(status("/internal/rings"), Some(404));
        assert_eq!(status("/internal/ring/../index/serve"), Some(400));
        assert_eq!(status("/internal/ring/%2e%2e/index"), Some(400));
        assert_eq!(status("/internal/index/serve"), Some(403));
        assert_eq!(
            route_by_prefix(&routes(), b"garbage\r\n\r\n")
                .err()
                .map(|u| u.status),
            Some(400)
        );
    }

    /// **The tie's failing input.** A caller that reaches the origin's port
    /// directly can type `X-Mesh-Pubkey`; without the registration's tie, or
    /// with a guessed one, it is not believed.
    #[test]
    fn a_forged_identity_without_the_tie_is_not_believed() {
        let tie = mint_tie();
        let forged = |name: &str| match name {
            ORIGIN_TIE_HEADER => Some("not-the-tie"),
            PUBKEY_HEADER => Some("forged"),
            _ => None,
        };
        assert_eq!(tied_pubkey(&tie, forged), None);
        let untied = |name: &str| (name == PUBKEY_HEADER).then_some("forged");
        assert_eq!(tied_pubkey(&tie, untied), None);
        let tied = |name: &str| match name {
            ORIGIN_TIE_HEADER => Some(tie.as_str()),
            PUBKEY_HEADER => Some("abcd"),
            _ => None,
        };
        assert_eq!(tied_pubkey(&tie, tied), Some("abcd"));
        assert_ne!(mint_tie(), tie, "each registration gets its own tie");
    }
}
