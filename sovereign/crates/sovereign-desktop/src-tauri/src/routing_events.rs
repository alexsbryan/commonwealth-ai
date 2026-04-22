//! `TauriRoutingEventSink` — desktop implementation of the
//! [`RoutingEventSink`](sovereign_core::traits::RoutingEventSink)
//! trait defined in sovereign-core.
//!
//! The runtime emits three antifragile-routing events via this sink:
//!
//! - `interpretation-proposed` → UI renders an inline banner on
//!   moderate-confidence turns with an interpretation + redirect
//!   chips drawn from `RouterClassification.alternatives`.
//! - `clarification-request` → UI renders a ClarificationCard on
//!   low-confidence turns; synthesis is suppressed until the user
//!   picks an option or types freeform input.
//! - `turn-narration` → UI renders a model-voice chip mid-turn on
//!   long operations; capped at 3 per turn and suppressed below 5s.
//!
//! Per the PR2 plan, these payloads flow from Rust → Tauri emit →
//! Svelte `routing.machine.ts` FSM event → component `$derived`
//! view. Components never call `listen()` directly; they read from
//! the FSM store singleton.

use async_trait::async_trait;
use tauri::Emitter;

use sovereign_core::traits::RoutingEventSink;
use sovereign_core::types::{
    ClarificationRequest, InterpretationProposed, TurnNarration,
};

/// Emits the three routing events to the frontend via `AppHandle::emit`.
/// Event names intentionally match the strings the Svelte listener
/// wrapper subscribes to — changing either side requires updating
/// both; a grep for the event name finds both ends.
pub struct TauriRoutingEventSink {
    app_handle: tauri::AppHandle,
}

impl TauriRoutingEventSink {
    pub fn new(app_handle: tauri::AppHandle) -> Self {
        Self { app_handle }
    }

    fn emit<S: serde::Serialize + Clone>(&self, event: &str, payload: S) {
        if let Err(e) = self.app_handle.emit(event, payload) {
            tracing::warn!(event, error = %e, "routing_events: emit failed");
        }
    }
}

#[async_trait]
impl RoutingEventSink for TauriRoutingEventSink {
    async fn emit_interpretation_proposed(&self, payload: InterpretationProposed) {
        tracing::debug!(
            session_id = %payload.session_id,
            alternatives = payload.alternatives.len(),
            confidence = payload.confidence,
            "routing_events: interpretation-proposed"
        );
        self.emit("interpretation-proposed", payload);
    }

    async fn emit_clarification_request(&self, payload: ClarificationRequest) {
        tracing::debug!(
            session_id = %payload.session_id,
            options = payload.options.len(),
            "routing_events: clarification-request"
        );
        self.emit("clarification-request", payload);
    }

    async fn emit_turn_narration(&self, payload: TurnNarration) {
        tracing::debug!(
            session_id = %payload.session_id,
            phase = ?payload.event.phase,
            elapsed_ms = payload.event.elapsed_ms,
            "routing_events: turn-narration"
        );
        self.emit("turn-narration", payload);
    }
}
