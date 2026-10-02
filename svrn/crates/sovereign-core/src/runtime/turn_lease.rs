// SPDX-License-Identifier: AGPL-3.0-or-later
//! The turn-level foreground lease (issue #57 rec 4) — and, since
//! 2026-09-07, the turn's ADMISSION.
//!
//! A turn is foreground for its whole life. Every public turn entry on
//! [`Runtime`] wraps its [`StreamHandle`] here so the corpus engine's
//! `ForegroundLease` is held until the stream drops — when the turn ends
//! or the client goes away — and every background yield gate (ingest,
//! enrichment, the newsworthy tick) parks for the entire turn. One site
//! for every turn shape; no per-operation bump list. The `_unleased`
//! bodies live in `streaming.rs`.
//!
//! **The lease is now taken BEFORE the body runs, not after it returns.**
//! It used to be acquired on the handle the `_unleased` body had already
//! produced, which meant routing and retrieval — the first third of the
//! turn's wall clock — ran with no lease held at all. Taking it first is
//! what makes the sentence "a turn that has been admitted, routed and
//! retrieved holds the lease" true, and it is the precondition for
//! [`crate::runtime::admission`]: the same object is the token the turn's
//! model calls carry to the slot queue, so a continuation of accepted work
//! is parked rather than shed.

use std::pin::Pin;

use futures::Stream;

use super::admission::{self, AdmittedTurn};
use super::{Intent, ResumeSession, Runtime, StreamHandle};
use crate::error::Result;

/// A turn's stream, carrying the turn's foreground lease (issue #57 rec 4).
/// The lease is dropped with the stream — when the turn ends or the client
/// goes away — so every yield gate in the daemon (ingest, enrichment, the
/// newsworthy tick) parks for the WHOLE turn, not for the seconds after
/// each model call. One site for every turn shape; nothing to remember.
struct LeasedTurnStream {
    inner: Pin<Box<dyn Stream<Item = Result<String>> + Send>>,
    /// The turn's admission — its foreground lease and the id its model
    /// calls carried. Held, never read: dropping it ends both, together.
    _admitted: Option<AdmittedTurn>,
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
    /// Admit the turn: take its foreground lease and mint the id its model
    /// calls carry. `None` with no corpus engine or no signal installed
    /// (no daemon), in which case every entry below is the passthrough it
    /// always was.
    fn admit(&self) -> Option<AdmittedTurn> {
        self.corpus_engine.as_ref().and_then(AdmittedTurn::open)
    }

    /// Hand the turn's admission to the stream, so lease and token live
    /// exactly as long as the turn does.
    fn leased(&self, mut handle: StreamHandle, admitted: Option<AdmittedTurn>) -> StreamHandle {
        handle.stream = Box::pin(LeasedTurnStream {
            inner: handle.stream,
            _admitted: admitted,
        });
        handle
    }

    pub async fn handle_message_stream(
        &self,
        message: &str,
        conversation_id: &str,
    ) -> Result<StreamHandle> {
        let admitted = self.admit();
        let h = admission::scope(
            admitted.as_ref().map(AdmittedTurn::token),
            self.handle_message_stream_unleased(message, conversation_id),
        )
        .await?;
        Ok(self.leased(h, admitted))
    }

    pub async fn handle_message_stream_as(
        &self,
        message: &str,
        conversation_id: &str,
        intent: Intent,
    ) -> Result<StreamHandle> {
        let admitted = self.admit();
        let h = admission::scope(
            admitted.as_ref().map(AdmittedTurn::token),
            self.handle_message_stream_as_unleased(message, conversation_id, intent),
        )
        .await?;
        Ok(self.leased(h, admitted))
    }

    pub async fn handle_message_stream_naked(
        &self,
        message: &str,
        conversation_id: &str,
    ) -> Result<StreamHandle> {
        let admitted = self.admit();
        let h = admission::scope(
            admitted.as_ref().map(AdmittedTurn::token),
            self.handle_message_stream_naked_unleased(message, conversation_id),
        )
        .await?;
        Ok(self.leased(h, admitted))
    }

    pub async fn resume_session_stream(
        &self,
        message: &str,
        conversation_id: &str,
        resume: ResumeSession,
    ) -> Result<StreamHandle> {
        let admitted = self.admit();
        let h = admission::scope(
            admitted.as_ref().map(AdmittedTurn::token),
            self.resume_session_stream_unleased(message, conversation_id, resume),
        )
        .await?;
        Ok(self.leased(h, admitted))
    }

    pub async fn redirect_turn_stream(
        &self,
        session_id: &str,
        intent_hint: &str,
    ) -> Result<StreamHandle> {
        let admitted = self.admit();
        let h = admission::scope(
            admitted.as_ref().map(AdmittedTurn::token),
            self.redirect_turn_stream_unleased(session_id, intent_hint),
        )
        .await?;
        Ok(self.leased(h, admitted))
    }
}
