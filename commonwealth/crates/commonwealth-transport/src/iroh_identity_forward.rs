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
//! `Range` response stays byte-exact. A chunked request body is the one shape
//! this does not frame (media clients do not send them); the rest of that
//! connection passes through unrewritten, and the branch says so at info.
use std::net::SocketAddr;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

/// The namespace the acceptor owns. A client-supplied header under it is
/// stripped before the verified ones are added — the failing input this
/// module exists for is a forged `X-Mesh-Member` reaching the origin.
pub const MESH_HEADER_PREFIX: &str = "x-mesh-";

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
}

impl Forward {
    /// The local address the bytes reach, whichever kind this is.
    pub fn target(self) -> SocketAddr {
        match self {
            Forward::Splice(a) => a,
            Forward::Http { origin, .. } => origin,
        }
    }
}

/// How the bytes after a request head are framed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyFraming {
    /// No body follows; the next head starts immediately.
    None,
    /// Exactly this many body bytes follow.
    Length(u64),
    /// `Transfer-Encoding: chunked` — not framed here.
    Chunked,
}

/// Read a request head's framing. `Transfer-Encoding: chunked` wins over a
/// `Content-Length`, as RFC 9112 §6.3 says it must.
pub fn body_framing(head: &[u8]) -> BodyFraming {
    let mut length = BodyFraming::None;
    for line in head.split(|&b| b == b'\n').skip(1) {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let Some(colon) = line.iter().position(|&b| b == b':') else {
            continue;
        };
        let name = String::from_utf8_lossy(&line[..colon])
            .trim()
            .to_ascii_lowercase();
        let value = String::from_utf8_lossy(&line[colon + 1..])
            .trim()
            .to_string();
        match name.as_str() {
            "transfer-encoding" if value.to_ascii_lowercase().contains("chunked") => {
                return BodyFraming::Chunked;
            }
            "content-length" => {
                if let Ok(n) = value.parse::<u64>() {
                    length = BodyFraming::Length(n);
                }
            }
            _ => {}
        }
    }
    length
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
/// publisher's own credential for its own origin (`X-Emby-Token`, an
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
async fn read_head<R: AsyncBufReadExt + Unpin>(
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
            let framing = body_framing(&buf);
            let (head, stripped) = rewrite_head(&buf, &headers);
            if stripped > 0 {
                tracing::info!(
                    target: "transport",
                    stripped,
                    "iroh acceptor: dropped client-supplied x-mesh-* header(s) before adding the verified identity"
                );
            }
            if tcp_w.write_all(&head).await.is_err() {
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
        }
        let _ = tcp_w.shutdown().await;
    };
    tokio::join!(up, down);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> Vec<(String, String)> {
        vec![
            ("X-Mesh-Member".into(), "LittleMac".into()),
            ("X-Mesh-Node".into(), "node-0b0b".into()),
        ]
    }

    fn header_values<'a>(head: &'a str, name: &str) -> Vec<&'a str> {
        head.lines()
            .filter_map(|l| l.split_once(':'))
            .filter(|(n, _)| n.trim().eq_ignore_ascii_case(name))
            .map(|(_, v)| v.trim())
            .collect()
    }

    /// THE failing input: a client that types the identity header itself.
    /// Exactly one `X-Mesh-Member` reaches the origin, and it is the
    /// acceptor's, whatever the client sent and however it cased the name.
    #[test]
    fn a_forged_identity_header_is_replaced_by_the_verified_one() {
        let head = b"GET /whoami HTTP/1.1\r\nHost: x\r\nx-mesh-member: forged\r\nX-MESH-NODE: node-forged\r\nX-Mesh-Anything: nope\r\n\r\n";
        let (out, stripped) = rewrite_head(head, &identity());
        let out = String::from_utf8(out).unwrap();
        assert_eq!(stripped, 3);
        assert_eq!(header_values(&out, "x-mesh-member"), vec!["LittleMac"]);
        assert_eq!(header_values(&out, "x-mesh-node"), vec!["node-0b0b"]);
        assert!(header_values(&out, "x-mesh-anything").is_empty());
        assert!(out.ends_with("\r\n\r\n"));
    }

    /// The request line and every other header pass byte for byte — `Range`
    /// is a seek and the splice must not touch it.
    #[test]
    fn the_request_line_and_other_headers_pass_byte_exact() {
        let head = b"GET /library/title.bin HTTP/1.1\r\nHost: 127.0.0.1:1\r\nRange: bytes=10-19\r\nAccept: */*\r\n\r\n";
        let (out, stripped) = rewrite_head(head, &identity());
        assert_eq!(stripped, 0);
        let out = String::from_utf8(out).unwrap();
        assert!(out.starts_with("GET /library/title.bin HTTP/1.1\r\nHost: 127.0.0.1:1\r\nRange: bytes=10-19\r\nAccept: */*\r\n"));
        assert_eq!(header_values(&out, "x-mesh-member"), vec!["LittleMac"]);
    }

    /// A member name cannot end a line: control characters are dropped from
    /// the value, so a name like `"Mac\r\nX-Admin: yes"` injects nothing.
    #[test]
    fn a_header_value_cannot_smuggle_a_second_header() {
        let hostile = vec![(
            "X-Mesh-Member".to_string(),
            "Mac\r\nX-Admin: yes".to_string(),
        )];
        let (out, _) = rewrite_head(b"GET / HTTP/1.1\r\n\r\n", &hostile);
        let out = String::from_utf8(out).unwrap();
        assert_eq!(
            header_values(&out, "x-mesh-member"),
            vec!["MacX-Admin: yes"]
        );
        assert!(header_values(&out, "x-admin").is_empty());
    }

    /// The failing input hm-1 exists for. A publisher declares its own
    /// credential for its own origin; a viewer sends the same header name
    /// with a value of its choosing. Append-only would put the viewer's copy
    /// first and let the origin pick — so the declared one must DISPLACE it,
    /// not merely follow it.
    #[test]
    fn a_declared_header_displaces_the_clients_copy_of_that_name() {
        let declared = vec![("X-Emby-Token".to_string(), "the-holders-key".to_string())];
        let (out, stripped) = rewrite_head(
            b"GET /Items HTTP/1.1\r\nHost: h\r\nX-Emby-Token: the-viewers-key\r\n\r\n",
            &declared,
        );
        let out = String::from_utf8(out).unwrap();
        assert_eq!(
            header_values(&out, "x-emby-token"),
            vec!["the-holders-key"],
            "exactly one token on the wire, and it is the holder's"
        );
        assert_eq!(stripped, 1, "the client's attempt is counted, not silent");
        assert!(out.contains("Host: h"), "unrelated headers still pass");
    }

    /// Case and padding are the obvious ways around a naive comparison, and
    /// `wire_value` erases a third: a name carrying bytes the wire drops.
    #[test]
    fn the_displacement_survives_case_padding_and_unwriteable_bytes() {
        let declared = vec![("Authorization".to_string(), "holder".to_string())];
        let (out, stripped) = rewrite_head(
            "GET / HTTP/1.1\r\nAUTHORIZATION: viewer-upper\r\n  authorization  : viewer-pad\r\nAuthoriz\u{7f}ation: viewer-ctl\r\n\r\n".as_bytes(),
            &declared,
        );
        let out = String::from_utf8(out).unwrap();
        assert_eq!(header_values(&out, "authorization"), vec!["holder"]);
        assert_eq!(stripped, 3, "all three client spellings are displaced");
    }

    /// The strip must not become a general-purpose header eater: a name the
    /// publisher did NOT declare is the client's business and passes through.
    #[test]
    fn an_undeclared_client_header_is_untouched() {
        let declared = vec![("X-Emby-Token".to_string(), "k".to_string())];
        let (out, stripped) = rewrite_head(
            b"GET / HTTP/1.1\r\nRange: bytes=0-9\r\nCookie: c\r\n\r\n",
            &declared,
        );
        let out = String::from_utf8(out).unwrap();
        assert_eq!(header_values(&out, "range"), vec!["bytes=0-9"]);
        assert_eq!(header_values(&out, "cookie"), vec!["c"]);
        assert_eq!(stripped, 0);
    }

    #[test]
    fn body_framing_reads_content_length_and_chunked_wins() {
        assert_eq!(
            body_framing(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n"),
            BodyFraming::None
        );
        assert_eq!(
            body_framing(b"POST / HTTP/1.1\r\nContent-Length: 12\r\n\r\n"),
            BodyFraming::Length(12)
        );
        assert_eq!(
            body_framing(
                b"POST / HTTP/1.1\r\nContent-Length: 12\r\nTransfer-Encoding: chunked\r\n\r\n"
            ),
            BodyFraming::Chunked
        );
    }

    /// Keep-alive: two requests on one connection, a body between them, and
    /// the identity lands on BOTH heads while the body is copied untouched.
    #[tokio::test]
    async fn every_request_on_a_kept_alive_connection_carries_the_identity() {
        let input = b"POST /a HTTP/1.1\r\nContent-Length: 5\r\nX-Mesh-Member: forged\r\n\r\nhelloGET /b HTTP/1.1\r\n\r\n".to_vec();
        let mut reader = tokio::io::BufReader::new(std::io::Cursor::new(input));
        let mut out = Vec::new();
        let headers = identity();
        let mut buf = Vec::new();
        while read_head(&mut reader, &mut buf).await.unwrap() {
            let framing = body_framing(&buf);
            let (head, _) = rewrite_head(&buf, &headers);
            out.extend_from_slice(&head);
            if let BodyFraming::Length(n) = framing {
                let mut body = (&mut reader).take(n);
                tokio::io::copy(&mut body, &mut out).await.unwrap();
            }
        }
        let out = String::from_utf8(out).unwrap();
        assert_eq!(out.matches("X-Mesh-Member: LittleMac").count(), 2);
        assert!(out.contains("\r\n\r\nhelloGET /b HTTP/1.1\r\n"), "{out}");
        assert!(!out.contains("forged"));
    }
}
