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
