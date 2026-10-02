// SPDX-License-Identifier: AGPL-3.0-or-later
//! The OpenAI wire's rendering at its historical path, and serve's axum face
//! of it. The rendering is `sovereign_contracts::openai_http`
//! (pb-svrn-serving-ports); this module turns its [`SseItem`]s into axum
//! `Event`s.

use axum::response::sse::Event;
use oicp_types::openai_types::StreamFrame;

pub use sovereign_contracts::openai_http::*;

/// serve's axum face of an [`SseItem`].
pub fn event(item: SseItem) -> Event {
    match item {
        SseItem::Data(data) => Event::default().data(data),
        SseItem::Comment(comment) => Event::default().comment(comment),
    }
}

/// One chat stream frame as its OpenAI SSE event
/// (`sovereign_contracts::openai_http::sse_item`, rendered).
pub fn sse_event(header: &ChunkHeader, frame: StreamFrame) -> Event {
    event(sse_item(header, frame))
}
