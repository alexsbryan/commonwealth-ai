// SPDX-License-Identifier: AGPL-3.0-or-later
//! svrn's axum face of the OpenAI wire's rendering. The rendering itself —
//! chat chunks, embeddings, `/v1/models` rows and the FIM response — is the
//! contracts leaf's (`sovereign_contracts::{openai_http, fim_http}`), shared
//! with serve so the two doors cannot render one stream two ways
//! (pb-svrn-serving-ports).

use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures::StreamExt;
use oicp_types::openai_types::StreamFrame;
use oicp_types::FimStreamStart;
use sovereign_contracts::fim_http;
use sovereign_contracts::openai_http::{sse_item, ChunkHeader, SseItem};

/// An [`SseItem`] as an axum `Event`.
pub fn event(item: SseItem) -> Event {
    match item {
        SseItem::Data(data) => Event::default().data(data),
        SseItem::Comment(comment) => Event::default().comment(comment),
    }
}

/// One chat stream frame as its OpenAI SSE event.
pub fn sse_event(header: &ChunkHeader, frame: StreamFrame) -> Event {
    event(sse_item(header, frame))
}

/// Non-streaming FIM: one OpenAI `text_completion` object.
pub async fn serve_fim_aggregated(
    start: FimStreamStart,
    debug_wanted: bool,
    model_echo: Option<String>,
) -> Response {
    fim_http::fim_aggregated(start, debug_wanted, model_echo)
        .await
        .into_response()
}

/// Streaming FIM: SSE chunks then `[DONE]`.
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
