// SPDX-License-Identifier: AGPL-3.0-or-later
//! The turn-level foreground lease (issue #57 rec 4).
//!
//! A turn is foreground for its whole life. Every public turn entry on
//! [`Runtime`] wraps its [`StreamHandle`] here so the corpus engine's
//! `ForegroundLease` is held until the stream drops — when the turn ends
//! or the client goes away — and every background yield gate (ingest,
//! enrichment, the newsworthy tick) parks for the entire turn. One site
//! for every turn shape; no per-operation bump list. The `_unleased`
//! bodies live in `streaming.rs`.

use std::pin::Pin;

use futures::Stream;

use super::{Intent, ResumeSession, Runtime, StreamHandle};
use crate::error::Result;

/// A turn's stream, carrying the turn's foreground lease (issue #57 rec 4).
/// The lease is dropped with the stream — when the turn ends or the client
/// goes away — so every yield gate in the daemon (ingest, enrichment, the
/// newsworthy tick) parks for the WHOLE turn, not for the seconds after
/// each model call. One site for every turn shape; nothing to remember.
struct LeasedTurnStream {
    inner: Pin<Box<dyn Stream<Item = Result<String>> + Send>>,
    _lease: Option<corpus_engine::ForegroundLease>,
}

impl Stream for LeasedTurnStream {
    type Item = Result<String>;
    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.get_mut().inner.as_mut().poll_next(cx)
    }
}

impl Runtime {
    /// Wrap a turn's handle in its foreground lease. With no signal
    /// installed (no daemon) the wrap is a plain passthrough.
    fn leased(&self, mut handle: StreamHandle) -> StreamHandle {
        let lease = self
            .corpus_engine
            .as_ref()
            .and_then(|engine| engine.foreground_lease());
        handle.stream = Box::pin(LeasedTurnStream {
            inner: handle.stream,
            _lease: lease,
        });
        handle
    }

    pub async fn handle_message_stream(
        &self,
        message: &str,
        conversation_id: &str,
    ) -> Result<StreamHandle> {
        let h = self
            .handle_message_stream_unleased(message, conversation_id)
            .await?;
        Ok(self.leased(h))
    }

    pub async fn handle_message_stream_as(
        &self,
        message: &str,
        conversation_id: &str,
        intent: Intent,
    ) -> Result<StreamHandle> {
        let h = self
            .handle_message_stream_as_unleased(message, conversation_id, intent)
            .await?;
        Ok(self.leased(h))
    }

    pub async fn handle_message_stream_naked(
        &self,
        message: &str,
        conversation_id: &str,
    ) -> Result<StreamHandle> {
        let h = self
            .handle_message_stream_naked_unleased(message, conversation_id)
            .await?;
        Ok(self.leased(h))
    }

    pub async fn resume_session_stream(
        &self,
        message: &str,
        conversation_id: &str,
        resume: ResumeSession,
    ) -> Result<StreamHandle> {
        let h = self
            .resume_session_stream_unleased(message, conversation_id, resume)
            .await?;
        Ok(self.leased(h))
    }

    pub async fn redirect_turn_stream(
        &self,
        session_id: &str,
        intent_hint: &str,
    ) -> Result<StreamHandle> {
        let h = self
            .redirect_turn_stream_unleased(session_id, intent_hint)
            .await?;
        Ok(self.leased(h))
    }
}
