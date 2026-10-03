// SPDX-License-Identifier: AGPL-3.0-or-later
//! The FIM route's response half, serve's axum face of it. The rendering is
//! `sovereign_contracts::fim_http` (pb-svrn-serving-ports), shared with svrn's
//! door so the two cannot render one stream two ways.

use axum::response::sse::{KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures::StreamExt;
use oicp_types::FimStreamStart;
use sovereign_contracts::fim_http;

use crate::openai_http::event;

/// Non-streaming: one OpenAI `text_completion` object
/// (`fim_http::fim_aggregated`, rendered).
pub async fn serve_fim_aggregated(
    start: FimStreamStart,
    debug_wanted: bool,
    model_echo: Option<String>,
) -> Response {
    fim_http::fim_aggregated(start, debug_wanted, model_echo)
        .await
        .into_response()
}

/// Streaming: SSE chunks then `[DONE]` (`fim_http::fim_sse_items`, rendered).
pub fn serve_fim_sse(
    start: FimStreamStart,
    debug_wanted: bool,
    model_echo: Option<String>,
) -> Response {
    let events = fim_http::fim_sse_items(start, debug_wanted, model_echo)
        .map(|item| Ok::<_, std::convert::Infallible>(event(item)));
    Sse::new(events)
        .keep_alive(KeepAlive::default())
        .into_response()
}
