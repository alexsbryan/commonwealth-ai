// SPDX-License-Identifier: AGPL-3.0-or-later
//! The MCP HTTP+SSE framing (phase-b pb-code-server, decision phase-b-13):
//! `POST /mcp` and `POST /mcp/message` answer JSON-RPC bodies through any
//! [`McpRequestHandler`], and `GET /mcp` is the SSE stream that carries the
//! 2024-11-05 `endpoint` event and server-pushed notifications from an
//! [`McpNotifier`]. Moved from the daemon's `mcp_router`, which mounts it at
//! its historical paths. Local-only: a non-loopback peer gets `403`.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Extension};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use futures::stream::{self, Stream, StreamExt};
use oicp_types::jsonrpc::JsonRpcResponse;
use oicp_types::mcp::{MCP_CORPUS_HEADER, MCP_EFFECTS_HEADER, MCP_EFFECTS_READ};
use serde_json::Value;

use super::{dispatch_body, McpRequestContext, McpRequestHandler};
use crate::locality::RequestLocality;

/// The header an agent names its session with (the work atlas groups
/// claims by it).
const AGENT_SESSION_HEADER: &str = "x-agent-session";

/// Broadcast surface for server-initiated MCP notifications.
///
/// MCP defines `notifications/tools/list_changed` as a server-pushed signal
/// that the tool list has changed and the client should refetch. It is
/// delivered over the SSE channel (`GET /mcp`), which every spec-compliant
/// client opens after `initialize`.
///
/// A [`tokio::sync::broadcast::Sender`] fans one payload out to every
/// connected SSE subscriber. The buffer is small: clients refetch on any
/// signal, so queued duplicates collapse into one re-fetch, and a lagging
/// subscriber that drops items re-syncs on its next `tools/list`.
#[derive(Clone)]
pub struct McpNotifier {
    sender: Arc<tokio::sync::broadcast::Sender<Value>>,
}

impl McpNotifier {
    /// Subscribers that lag past this many messages see
    /// `RecvError::Lagged`; `tools/list_changed` is idempotent, so dropping
    /// is fine.
    const BUFFER_SIZE: usize = 16;

    /// A fresh notifier with no subscribers. SSE clients subscribe as they
    /// connect.
    pub fn new() -> Self {
        let (sender, _) = tokio::sync::broadcast::channel(Self::BUFFER_SIZE);
        Self {
            sender: Arc::new(sender),
        }
    }

    /// Push a `notifications/tools/list_changed` frame to every connected
    /// SSE client. A no-op with no subscribers (before any client opens
    /// `GET /mcp`).
    pub fn notify_tools_list_changed(&self) {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/tools/list_changed"
        });
        // `send` errs only when there are zero receivers.
        let _ = self.sender.send(payload);
    }

    /// Subscribe one SSE handler; each gets its own receive cursor.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Value> {
        self.sender.subscribe()
    }
}

impl Default for McpNotifier {
    fn default() -> Self {
        Self::new()
    }
}

/// `/mcp` (POST and the SSE GET) and `/mcp/message` (POST) over `handler`.
/// `POST /mcp` is the 2025-03-26 Streamable HTTP entry point; `/mcp/message`
/// stays for clients that followed the 2024-11-05 HTTP+SSE transport, where
/// the message endpoint was a separate URL. The caller adds its own routes,
/// guard layers and CORS around it.
pub fn routes<H: McpRequestHandler + 'static>(handler: Arc<H>, notifier: McpNotifier) -> Router {
    Router::new()
        .route("/mcp", post(mcp_handle::<H>).get(mcp_sse))
        .route("/mcp/message", post(mcp_handle::<H>))
        .layer(Extension(handler))
        .layer(Extension(notifier))
}

/// The `endpoint` event the 2024-11-05 transport requires, then every
/// notification the [`McpNotifier`] broadcasts, each as an unnamed `data:`
/// event whose body is the JSON-RPC frame. The endpoint points back at
/// `/mcp`, so both transports converge on one handler.
async fn mcp_sse(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Extension(notifier): Extension<McpNotifier>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, StatusCode> {
    let at = RequestLocality::of(&peer, &headers);
    if !at.is_local() {
        tracing::debug!(%peer, locality = ?at, "mcp: sse refused, not a local process");
        return Err(StatusCode::FORBIDDEN);
    }
    let endpoint_event = stream::once(async {
        Ok::<_, Infallible>(Event::default().event("endpoint").data("/mcp"))
    });
    let notifications =
        tokio_stream::wrappers::BroadcastStream::new(notifier.subscribe()).map(|res| match res {
            Ok(payload) => Ok::<_, Infallible>(Event::default().data(payload.to_string())),
            // A lagged cursor missed items; a list_changed frame makes a
            // well-behaved client refetch, which re-syncs the truth.
            Err(_lagged) => Ok::<_, Infallible>(
                Event::default()
                    .data(r#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}"#),
            ),
        });
    Ok(Sse::new(endpoint_event.chain(notifications)).keep_alive(KeepAlive::default()))
}

/// One JSON-RPC body, single or batch, through [`dispatch_body`]. Nothing to
/// reply (notifications only) is an empty `202`.
async fn mcp_handle<H: McpRequestHandler + 'static>(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(handler): Extension<Arc<H>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> axum::response::Response {
    let at = RequestLocality::of(&peer, &headers);
    if !at.is_local() {
        tracing::debug!(%peer, locality = ?at, "mcp: request refused, not a local process");
        let why = match &at {
            RequestLocality::CrossOrigin(origin) => {
                format!("MCP refuses a request from another origin ({origin})")
            }
            RequestLocality::Local | RequestLocality::Remote | RequestLocality::ForeignHost(_) => {
                "MCP is local-only".to_string()
            }
        };
        return (
            StatusCode::FORBIDDEN,
            Json(JsonRpcResponse::error(Value::Null, -32001, why)),
        )
            .into_response();
    }
    let ctx = match request_context(&headers) {
        Ok(ctx) => ctx,
        Err(why) => {
            tracing::debug!(reason = %why, "mcp: request refused at the edge");
            return (
                StatusCode::BAD_REQUEST,
                Json(JsonRpcResponse::error(Value::Null, -32600, why)),
            )
                .into_response();
        }
    };
    match dispatch_body(&*handler, body, &ctx).await {
        Some(reply) => (StatusCode::OK, Json(reply)).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

/// The request's [`McpRequestContext`], read off its headers. An effect cap
/// this framing does not know is refused, never read as no cap: a typo in a
/// read-only client's config must not hand it the write tools. Which corpora
/// exist is the tool host's to judge ([`super::McpToolHost::admit`]).
fn request_context(headers: &HeaderMap) -> Result<McpRequestContext, String> {
    let text = |name: &str| -> Result<Option<String>, String> {
        headers
            .get(name)
            .map(|v| {
                v.to_str()
                    .map(str::to_string)
                    .map_err(|_| format!("the {name} header is not visible ASCII"))
            })
            .transpose()
    };
    let read_only = match text(MCP_EFFECTS_HEADER)?.as_deref() {
        None => false,
        Some(MCP_EFFECTS_READ) => true,
        Some(other) => {
            return Err(format!(
                "{MCP_EFFECTS_HEADER}: `{other}` is not an effect cap; the one value is \
                 `{MCP_EFFECTS_READ}`"
            ))
        }
    };
    let ctx = McpRequestContext {
        agent_session: text(AGENT_SESSION_HEADER).ok().flatten(),
        corpus: text(MCP_CORPUS_HEADER)?,
        read_only,
    };
    if ctx.corpus.is_some() || ctx.read_only {
        tracing::debug!(corpus = ?ctx.corpus, read_only = ctx.read_only, "mcp: connection scope");
    }
    Ok(ctx)
}

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;
