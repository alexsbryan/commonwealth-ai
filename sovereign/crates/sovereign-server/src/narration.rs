// SPDX-License-Identifier: AGPL-3.0-or-later
//! Broadcast bridge for runtime "glassbox" progress narration.
//!
//! The chat runtime narrates a turn's real work as it happens — routing,
//! retrieval, synthesis, gap checks, tool calls — via
//! [`RoutingEventSink::emit_turn_narration`]. The desktop renders each
//! event as a live progress chip; the WS host historically installed the
//! default [`NoOpRoutingEventSink`] and dropped them on the floor, so a
//! mobile client only ever saw "wait … wait … answer".
//!
//! [`BroadcastRoutingEventSink`] republishes every narration onto a
//! [`tokio::sync::broadcast`] channel. Each in-flight WS turn subscribes,
//! filters to its own `conversation_id`, and forwards the events as
//! `Narration` frames interleaved with the token stream — giving the
//! client the same "what is the host doing right now" surface as desktop.
//!
//! The channel is intentionally decoupled from the token stream: a slow
//! WS forwarder can drop a narration frame (`Lagged`) without ever
//! stalling token delivery, and narration is best-effort live signal that
//! is never persisted.

use async_trait::async_trait;
use tokio::sync::broadcast;

use sovereign_core::traits::RoutingEventSink;
use sovereign_core::types::{ClarificationRequest, InterpretationProposed, TurnNarration};

/// Broadcast capacity. Narration is sparse (the runtime caps a turn at a
/// handful of events) but we keep headroom so a brief forwarder stall
/// doesn't drop a stage frame; on overflow the receiver sees `Lagged` and
/// skips that frame — tokens, on a separate channel, are unaffected.
const NARRATION_CHANNEL_CAP: usize = 256;

/// A [`RoutingEventSink`] that republishes turn narration on a broadcast
/// channel. Install on the `Runtime` via `with_routing_events`; hand the
/// returned [`broadcast::Sender`] to the WS layer (as an axum `Extension`)
/// so each turn can `subscribe()`.
pub struct BroadcastRoutingEventSink {
    tx: broadcast::Sender<TurnNarration>,
}

impl BroadcastRoutingEventSink {
    /// Build the sink and return it alongside the `Sender` the WS layer
    /// subscribes against. Both hold clones of the same channel.
    pub fn new() -> (Self, broadcast::Sender<TurnNarration>) {
        let (tx, _rx) = broadcast::channel(NARRATION_CHANNEL_CAP);
        (Self { tx: tx.clone() }, tx)
    }
}

#[async_trait]
impl RoutingEventSink for BroadcastRoutingEventSink {
    // Antifragile-routing surfaces (interpretation proposals, low-
    // confidence clarification cards) are desktop-only today — the mobile
    // client doesn't render them yet, so we drop them rather than widen
    // the wire contract before there's a consumer.
    async fn emit_interpretation_proposed(&self, _payload: InterpretationProposed) {}
    async fn emit_clarification_request(&self, _payload: ClarificationRequest) {}

    async fn emit_turn_narration(&self, payload: TurnNarration) {
        // `send` errors only when there are no live subscribers — the
        // common case between turns. Best-effort: drop and move on.
        let _ = self.tx.send(payload);
    }
}
