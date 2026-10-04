// SPDX-License-Identifier: AGPL-3.0-or-later
//! The acceptor's second forward kind: an HTTP/1.1 origin that is told WHO is
//! asking.
//!
//! [`crate::iroh::IrohAcceptor`] verifies a dialer's Ed25519 key in the QUIC
//! handshake and then splices bytes to a local listener that never learns it.
//! For a listener that authenticates nothing — somebody's media server on
//! loopback — that is the whole story, and it means the server cannot tell one
//! member from another, cannot map a member to one of its own users, and
//! cannot be asked to. This module carries the verified identity across the
//! hop as request headers, so the origin (or a shim beside it) can decide per
//! member without ever holding a mesh key.
//!
//! **What it parses, and what it does not.** Only request HEADS, on the
//! client→origin direction: the request line and header lines up to the blank
//! line. Any header in the mesh's own namespace (`x-mesh-*`) that the CLIENT
//! sent is dropped — a viewer's HTTP client can type anything — and the
//! verified ones are appended. The body, if the head declares one, is copied
//! by its `Content-Length` untouched, and the next head is read after it, so
//! keep-alive connections carry the identity on every request. Responses are
//! never parsed: the origin→client direction is a byte copy, which is why a
//! `Range` response stays byte-exact. A head whose body this cannot delimit
//! exactly as any origin would — `Transfer-Encoding` of any kind, two
//! `Content-Length`s, or one that is not a single decimal number — ends the
//! connection there, at warn ([`body_framing`]): the bytes behind it would
//! otherwise reach the origin as requests no rewrite touched, carrying
//! whatever identity the client typed. Media clients send none of these.
use std::net::SocketAddr;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

pub use kernel_types::member::MESH_HEADER_PREFIX;

/// The header an acceptor stamps on a forward to a listener that will READ the
/// verified identity as an identity, rather than merely log it.
///
/// A forwarded request arrives at a local port, and a caller that reaches that
/// port without the acceptor in front can type `x-mesh-*` exactly as a viewer
/// can. So a reader that authorizes on the identity needs one more fact than
/// the headers carry: *this hop is my own acceptor's*. This header is that
/// fact. It lives under [`MESH_HEADER_PREFIX`], so a client-supplied one is
/// stripped by the same [`rewrite_head`] pass that strips a forged
/// `X-Mesh-Member` — the guard needs no second implementation.
///
/// It is deliberately NOT part of the `X-Mesh-*` identity triple a media or
/// app origin is told (`commonwealth_media::verified_headers`): those origins
/// are somebody else's software, and handing them the mark would let one of
/// them speak to this daemon's internal port as the acceptor. The acceptor
/// adds it on the arm whose origin is THIS process, and nowhere else.
pub const ACCEPTOR_MARK_HEADER: &str = "X-Mesh-Acceptor";

/// This process's acceptor mark — 32 random bytes, hex, minted once on first
/// use and never persisted, never logged, never sent to a foreign origin.
///
/// Per PROCESS rather than per mesh or per node: the only claim it makes is
/// "the acceptor that stamped this is the one running in the same process as
/// the listener reading it", which is exactly the tie a reader needs and is
/// the shortest-lived secret that establishes it. A restart mints a fresh one
/// and both halves move together, so there is nothing to rotate.
pub fn acceptor_mark() -> &'static str {
    static MARK: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    MARK.get_or_init(|| {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).expect("failed to generate the acceptor mark");
        hex::encode(bytes)
    })
}

/// Whether `presented` is this process's [`acceptor_mark`], compared in
/// constant time so the check cannot be turned into an oracle by timing.
///
/// A `false` here is "I cannot tie this connection to my acceptor", never
/// "this caller is hostile" — the caller decides what an untied connection
/// means, and the distinction is why this returns a bool rather than refusing.
pub fn is_acceptor_mark(presented: &str) -> bool {
    commonwealth_core::ct::constant_time_eq(presented.as_bytes(), acceptor_mark().as_bytes())
}

/// A request head larger than this is not a media request; the connection is
/// closed rather than buffered without bound.
const HEAD_CAP: usize = 64 * 1024;

/// Where an accepted connection's streams go — decided once per connection
/// by the acceptor's resolver, from the negotiated ALPN and the verified key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Forward {
    /// A byte splice to a local listener, both directions untouched.
    Splice(SocketAddr),
    /// An HTTP/1.1 origin that is handed `headers` on every request — the
    /// verified identity, named by the resolver — with any client-supplied
    /// `x-mesh-*` header stripped first.
    Http {
        origin: SocketAddr,
        headers: Vec<(String, String)>,
    },
    /// One of SEVERAL named HTTP origins, chosen by the first path segment of
    /// the request (`GET /chores/tasks` → the `chores` origin, forwarded as
    /// `GET /tasks`). Otherwise identical to [`Forward::Http`]: the same
    /// headers, the same strip rule, the same untouched responses.
    ///
    /// The resolver still decides ONCE per connection, on the verified key,
    /// whether this dialer may reach the App class at all. Only the *which
    /// app* lookup happens per request, and it can only choose among origins
    /// this node published. Do not read the per-request half as a loosening
    /// of the admission gate; they are different questions.
    HttpByName {
        /// Published apps by name. Ordered so the "no such app" refusal can
        /// list what this node does publish in a stable order.
        apps: std::sync::Arc<std::collections::BTreeMap<String, SocketAddr>>,
        headers: Vec<(String, String)>,
    },
    /// One of several registered HTTP origins on `cwth/http/0`, chosen per
    /// stream by the LONGEST registered path prefix covering the first
    /// request, which is forwarded unchanged
    /// ([`crate::iroh_routed_forward::route_by_prefix`]). Each route adds its
    /// own registration's tie to `headers`; an unregistered path is refused
    /// by name.
    HttpByPrefix {
        routes: std::sync::Arc<
            std::collections::BTreeMap<String, crate::iroh_routed_forward::PrefixRoute>,
        >,
        headers: Vec<(String, String)>,
    },
}

impl Forward {
    /// The one local address the bytes reach, or `None` for a kind that has
    /// no single target.
    ///
    /// `Option` rather than a representative address: [`Forward::HttpByName`]
    /// resolves per request, and answering with (say) the first app's port
    /// would be a plausible, wrong answer to "where does this go" — the shape
    /// of silent substitution this workspace keeps paying for (ARCH §6).
    pub fn target(self) -> Option<SocketAddr> {
        match self {
            Forward::Splice(a) => Some(a),
            Forward::Http { origin, .. } => Some(origin),
            Forward::HttpByName { .. } | Forward::HttpByPrefix { .. } => None,
        }
    }
}

/// Whether `name` can be an app name: non-empty ASCII alphanumerics, `_` and
/// `-`, and nothing else.
///
/// One rule, read from both ends of the same wire (ARCH principle 8): the
/// registry refuses a name it could not later match, and [`split_app_name`]
/// refuses a request that could not name a registered app. Splitting these
/// into two character tables is how a name becomes publishable and
/// unreachable at the same time, which reads to the publisher as the mesh
/// being down.
///
/// The table is what keeps `..`, encoded slashes and stray bytes out of the
/// lookup key structurally rather than by a sanitizer someone has to
/// remember to call (ARCH principle 10).
pub fn valid_app_name(name: &[u8]) -> bool {
    !name.is_empty()
        && name
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
}

/// The leading path segment of a request head's target, and the head with
/// that segment removed — `GET /chores/tasks?x=1 HTTP/1.1` becomes
/// `("chores", "GET /tasks?x=1 HTTP/1.1")`.
///
/// `None` when the head has no usable name: a malformed request line, an
/// absolute-form target (`GET http://host/p`, which a bridge client does not
/// send), a bare `/`, or a segment outside `[A-Za-z0-9_-]`. The character
/// rule is what keeps `..`, encoded slashes and stray bytes out of the
/// lookup key structurally, rather than by a sanitizer someone has to
/// remember to call (ARCH §10) — the registry is a map, so an unmatched key
/// is refused anyway, but a name that cannot express traversal never gets
/// the chance to be interesting.
///
/// The name travels in the PATH rather than in a header on purpose. A header
/// would have to be exempted from the `x-mesh-*` strip to survive, which
/// means one forgeable header becomes load-bearing; and a path prefix is
/// something a browser or `curl` can express with no custom client at all.
/// The cost is the usual sub-path reverse-proxy cost: an app that emits
/// absolute links (`/static/app.css`) emits them without the prefix. Apps
/// published here should use relative URLs, which is what a one-file Flask
/// app does anyway.
pub fn split_app_name(head: &[u8]) -> Option<(String, Vec<u8>)> {
    let end = head.iter().position(|&b| b == b'\n').unwrap_or(head.len());
    let line = head[..end].strip_suffix(b"\r").unwrap_or(&head[..end]);
    // Exactly three space-separated fields, and the third must be a version.
    // `splitn(3, ' ')` would accept `GET /cho res/x HTTP/1.1` by sweeping the
    // extra spaces into the version field and hand back `cho` as the app — a
    // malformed request line silently resolving to a REAL app name is the
    // wrong kind of tolerant.
    let mut parts = line.split(|&b| b == b' ');
    let method = parts.next()?;
    let target = parts.next()?;
    let version = parts.next()?;
    if parts.next().is_some() || !version.starts_with(b"HTTP/") || !target.starts_with(b"/") {
        return None;
    }
    let rest = &target[1..];
    let cut = rest
        .iter()
        .position(|&b| b == b'/' || b == b'?')
        .unwrap_or(rest.len());
    let (name, tail) = rest.split_at(cut);
    if !valid_app_name(name) {
        return None;
    }
    let mut out = Vec::with_capacity(head.len());
    out.extend_from_slice(method);
    out.push(b' ');
    if tail.is_empty() || tail.starts_with(b"?") {
        out.push(b'/');
    }
    out.extend_from_slice(tail);
    out.push(b' ');
    out.extend_from_slice(version);
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(&head[end.saturating_add(1).min(head.len())..]);
    Some((String::from_utf8_lossy(name).into_owned(), out))
}

/// How the bytes after a request head are framed, or why this forward will
/// not frame them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyFraming {
    /// No body follows; the next head starts immediately.
    None,
    /// Exactly this many body bytes follow.
    Length(u64),
    /// This forward cannot say where the body ends exactly as every origin
    /// would, so the connection carries no further request. The reason is
    /// for the log.
    Refused(&'static str),
}

/// Read a request head's framing — strictly, because the forward and the
/// origin must agree on where every request starts.
///
/// Where they disagree, bytes the forward copied as a body are parsed by the
/// origin as a request whose head no rewrite touched, so a member could name
/// another member's key, or none. A chunked body was copied through that way
/// until 2026-10-04. So anything but one plain `Content-Length` is refused:
/// `Transfer-Encoding` of any kind (this forward decodes none; RFC 9112 §6.3
/// lets a server reject any it does not), a second `Content-Length` (origins
/// differ on duplicates), and a value that is not one decimal number (`5, 5`
/// is a body to hyper and no body to a stricter reader). Header names are
/// compared as [`rewrite_head`] compares them, so the two cannot read one
/// line two ways.
pub fn body_framing(head: &[u8]) -> BodyFraming {
    let mut length = None;
    for line in head.split(|&b| b == b'\n').skip(1) {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let Some(colon) = line.iter().position(|&b| b == b':') else {
            continue;
        };
        let name = compare_name(&String::from_utf8_lossy(&line[..colon]));
        let raw = String::from_utf8_lossy(&line[colon + 1..]);
        let value = raw.trim_matches(|c| c == ' ' || c == '\t');
        match name.as_str() {
            "transfer-encoding" => {
                return BodyFraming::Refused("a Transfer-Encoding request body is not framed here")
            }
            "content-length" if length.is_some() => {
                return BodyFraming::Refused("a request with more than one Content-Length")
            }
            "content-length" => {
                if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                    return BodyFraming::Refused("a Content-Length that is not one decimal number");
                }
                match value.parse::<u64>() {
                    Ok(n) => length = Some(n),
                    Err(_) => return BodyFraming::Refused("a Content-Length past 2^64"),
                }
            }
            _ => {}
        }
    }
    length.map_or(BodyFraming::None, BodyFraming::Length)
}

/// A header value the wire can carry: visible ASCII only. A member name is
/// operator-chosen text and must not be able to end a line.
fn wire_value(v: &str) -> String {
    v.chars()
        .filter(|c| (' '..='~').contains(c))
        .collect::<String>()
        .trim()
        .to_string()
}

/// The name a header is compared under: sanitized the same way it will be
/// written, then lowercased. Comparing raw text would let a declared
/// `X-Emby-Token` miss a client's `X-Emby-Token\u{7f}` — two names that reach
/// the origin identically once [`wire_value`] has run.
fn compare_name(name: &str) -> String {
    wire_value(name).to_ascii_lowercase()
}

/// Rewrite one request head (which ends with the blank line): drop every
/// client-supplied `x-mesh-*` header AND every client-supplied header whose
/// name a caller is about to declare, then append `headers`. Returns the new
/// head and how many lines were stripped, so the acceptor can say when a
/// client tried. Everything else — the request line, `Range`, `Host`, cookies
/// — is copied byte for byte.
///
/// # Why the collision strip is not optional
///
/// Appending alone is NOT enough to make a declared header authoritative.
/// `headers` carries two different things now: the verified identity, which
/// lives in the `x-mesh-*` namespace the prefix rule already clears, and a
/// publisher's own credential for its own origin (`Authorization`, an
/// `Authorization`), which does not. Append-only would leave the client's
/// copy AHEAD of ours in the head, and which one an origin honours on a
/// duplicate name is its business, not ours — Jellyfin, nginx and axum do not
/// agree. A viewer could then choose the credential its request runs under.
/// So a declared name displaces the client's: stripped first, appended last,
/// exactly one on the wire.
pub fn rewrite_head(head: &[u8], headers: &[(String, String)]) -> (Vec<u8>, usize) {
    let declared: Vec<String> = headers.iter().map(|(n, _)| compare_name(n)).collect();
    let mut out = Vec::with_capacity(head.len() + 128);
    let mut stripped = 0usize;
    let mut lines = head.split(|&b| b == b'\n');
    if let Some(request_line) = lines.next() {
        out.extend_from_slice(request_line.strip_suffix(b"\r").unwrap_or(request_line));
        out.extend_from_slice(b"\r\n");
    }
    for line in lines {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        let name_end = line.iter().position(|&b| b == b':').unwrap_or(line.len());
        let name = compare_name(&String::from_utf8_lossy(&line[..name_end]));
        if name.starts_with(MESH_HEADER_PREFIX) || declared.iter().any(|d| *d == name) {
            stripped += 1;
            continue;
        }
        out.extend_from_slice(line);
        out.extend_from_slice(b"\r\n");
    }
    for (name, value) in headers {
        out.extend_from_slice(wire_value(name).as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(wire_value(value).as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"\r\n");
    (out, stripped)
}

/// Read one request head into `buf` (including its blank line). `Ok(false)`
/// on a clean EOF before any byte; `Err` on EOF mid-head or a head past
/// [`HEAD_CAP`].
pub(crate) async fn read_head<R: AsyncBufReadExt + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
) -> std::io::Result<bool> {
    buf.clear();
    loop {
        let before = buf.len();
        let n = reader.read_until(b'\n', buf).await?;
        if n == 0 {
            return if buf.is_empty() {
                Ok(false)
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "connection ended inside a request head",
                ))
            };
        }
        let line = &buf[before..];
        if line == b"\r\n" || line == b"\n" {
            // The first line cannot be blank; a leading blank line is
            // tolerated by RFC 9112 §2.2 and skipped here.
            if before == 0 {
                buf.clear();
                continue;
            }
            return Ok(true);
        }
        if buf.len() > HEAD_CAP {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "request head exceeds 64 KiB",
            ));
        }
    }
}

/// Forward ONE request already read into `head`: rewrite it with `headers`,
/// write it, then copy its body by its framing. `Break` when this connection
/// must carry no further request.
///
/// The one step both pumps run per request ([`pump_with_identity`] and
/// `iroh_routed_forward::pump_routed`), so how a request is framed and
/// rewritten has one implementation (ARCH principle 8).
pub(crate) async fn forward_request<R, W>(
    head: &[u8],
    reader: &mut R,
    writer: &mut W,
    headers: &[(String, String)],
) -> std::ops::ControlFlow<()>
where
    R: tokio::io::AsyncBufRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    use std::ops::ControlFlow::{Break, Continue};
    let framing = body_framing(head);
    if let BodyFraming::Refused(why) = framing {
        tracing::warn!(
            target: "transport",
            why,
            "iroh acceptor: refused a request whose body this forward cannot frame — \
             nothing more on this connection reaches the origin"
        );
        return Break(());
    }
    let (out, stripped) = rewrite_head(head, headers);
    if stripped > 0 {
        tracing::info!(
            target: "transport",
            stripped,
            "iroh acceptor: dropped client-supplied x-mesh-* header(s) before adding the verified identity"
        );
    }
    if writer.write_all(&out).await.is_err() {
        return Break(());
    }
    match framing {
        BodyFraming::None => Continue(()),
        BodyFraming::Length(n) => {
            let mut body = reader.take(n);
            if tokio::io::copy(&mut body, writer).await.is_err() {
                return Break(());
            }
            Continue(())
        }
        // Answered above, before the head was written.
        BodyFraming::Refused(_) => Break(()),
    }
}

/// Pump one accepted bi-stream to `tcp`, rewriting every request head on the
/// way in and copying the responses on the way out untouched.
pub async fn pump_with_identity(
    tcp: tokio::net::TcpStream,
    mut send: iroh::endpoint::SendStream,
    recv: iroh::endpoint::RecvStream,
    headers: std::sync::Arc<Vec<(String, String)>>,
) {
    let (mut tcp_r, mut tcp_w) = tcp.into_split();
    let down = async {
        let _ = tokio::io::copy(&mut tcp_r, &mut send).await;
        let _ = send.finish();
    };
    let up = async {
        let mut reader = tokio::io::BufReader::new(recv);
        let mut buf = Vec::with_capacity(4096);
        loop {
            match read_head(&mut reader, &mut buf).await {
                Ok(true) => {}
                Ok(false) => break,
                Err(e) => {
                    tracing::info!(
                        target: "transport",
                        error = %e,
                        "iroh acceptor: identity forward closed a request it could not frame"
                    );
                    break;
                }
            }
            if forward_request(&buf, &mut reader, &mut tcp_w, &headers)
                .await
                .is_break()
            {
                break;
            }
        }
        let _ = tcp_w.shutdown().await;
    };
    tokio::join!(up, down);
}

/// Pump one accepted bi-stream to whichever published app its FIRST request
/// names, rewriting heads exactly as [`pump_with_identity`] does.
///
/// # Why the name binds per STREAM and not per request
///
/// Retargeting between requests on one stream would mean switching TCP
/// connections mid-flight, and doing that safely requires knowing that the
/// previous response has finished — which requires PARSING responses. This
/// module deliberately never parses the origin→client direction: that byte
/// copy is what makes a `Range` response byte-exact and lets a player seek as
/// if the library were local. Buying per-request retargeting with response
/// parsing would trade the one guarantee the media path is built on for a
/// case no real client produces — an HTTP client keeping a connection alive
/// does not switch origins on it, and a browser opens a fresh connection per
/// origin regardless.
///
/// So the first head binds the name, and a later head naming a DIFFERENT app
/// closes the stream with a logged reason rather than being silently served
/// by the wrong origin. The client reconnects and gets its app.
pub async fn pump_by_name(
    send: iroh::endpoint::SendStream,
    recv: iroh::endpoint::RecvStream,
    apps: std::sync::Arc<std::collections::BTreeMap<String, SocketAddr>>,
    headers: std::sync::Arc<Vec<(String, String)>>,
) {
    use crate::iroh_routed_forward::{pump_routed, Routed, Unrouted};
    // Safe to list here and not elsewhere: the caller is already an admitted
    // member of the App class, so the names are not a disclosure.
    let published: Vec<&str> = apps.keys().map(String::as_str).collect();
    let listing = format!(
        "this node publishes: {}",
        if published.is_empty() {
            "(nothing)".to_string()
        } else {
            published.join(", ")
        }
    );
    pump_routed(send, recv, headers, "app", Some(listing), |head| {
        let Some((name, rest)) = split_app_name(head) else {
            return Err(Unrouted {
                status: 404,
                key: "<none>".into(),
                why: "no app named in the request path".into(),
            });
        };
        match apps.get(&name) {
            Some(origin) => Ok(Routed {
                key: name,
                origin: *origin,
                head: rest,
                extra: Vec::new(),
            }),
            None => Err(Unrouted {
                status: 404,
                why: format!("no app named {name:?} here"),
                key: name,
            }),
        }
    })
    .await
}

#[cfg(test)]
#[path = "iroh_identity_forward_tests.rs"]
mod tests;
