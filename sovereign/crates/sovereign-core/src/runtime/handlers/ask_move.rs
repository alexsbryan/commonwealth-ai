// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ask move — non-streaming (`handle_ask_move_turn`) and streaming
//! (`handle_ask_move_stream`) variants of the "deliberate, ask for
//! clarification" path. Fires when classification confidence is too
//! low to commit silently. Cost is one saved message + one event.

use crate::error::Result;
use crate::traits::*;

use super::super::*;

impl Runtime {
    /// PR2 — non-streaming `MoveKind::Ask` handler. Same shape as
    /// `handle_ask_move_stream` but returns a `Response` instead of
    /// a `StreamHandle`. CLI / server callers receive the placeholder
    /// assistant message with clarification metadata; the `Ask` event
    /// is emitted on the routing sink (no-op in headless builds).
    pub(crate) async fn handle_ask_move_turn(
        &self,
        original_message: &str,
        conversation_id: &str,
        session_id: &str,
        classification: &RouterClassification,
    ) -> Result<Response> {
        // Mirror the streaming-path glassbox surfacing — emit the
        // deliberation chip and pause briefly before computing /
        // emitting the clarification. Same rationale: the Ask path
        // is too fast to surface its "let me ask first" moment
        // unless we deliberately make it visible.
        emit_ask_deliberation_chip(
            self.routing_events.as_ref(),
            session_id,
            conversation_id,
            classification,
        )
        .await;
        tokio::time::sleep(std::time::Duration::from_millis(
            ASK_MOVE_DELIBERATION_LINGER_MS,
        ))
        .await;

        let message_id = uuid::Uuid::new_v4().to_string();
        let question =
            build_clarification_question(original_message, &classification.primary.intent);
        let options: Vec<ClarificationOption> = classification
            .alternatives
            .iter()
            .map(|c| ClarificationOption {
                label: label_for_intent(&c.intent),
                follow_up: original_message.to_string(),
                intent_hint: intent_hint(&c.intent),
            })
            .collect();

        let clarification_payload = ClarificationRequest {
            session_id: session_id.to_string(),
            conversation_id: conversation_id.to_string(),
            question: question.clone(),
            options: options.clone(),
        };

        let placeholder_body =
            "I want to make sure I give you the right shape of answer.".to_string();
        let metadata = serde_json::json!({
            "move_kind": "ask",
            "confidence": classification.primary.confidence,
            "clarification": {
                "session_id": session_id,
                "question": question,
                "options": options,
            },
            "coarse_intent": classification.coarse_intent,
        });
        let assistant_msg = Message {
            id: message_id,
            conversation_id: conversation_id.to_string(),
            role: Role::Assistant,
            content: placeholder_body,
            created_at: now(),
            metadata: Some(metadata),
            version: 0,
        };
        self.store.save_message(&assistant_msg).await?;
        let response_msg = assistant_msg.clone();

        self.routing_events
            .emit_clarification_request(clarification_payload)
            .await;

        tracing::info!(
            session_id,
            conversation_id,
            options = classification.alternatives.len(),
            "routing:ask — clarification requested (non-streaming path)"
        );

        Ok(Response {
            message: response_msg,
            task: None,
            metrics: None,
        })
    }
    /// PR2 — streaming `MoveKind::Ask` handler. Suppress synthesis,
    /// persist a placeholder assistant message whose metadata carries
    /// the clarification payload (so the UI's existing
    /// message-metadata plumbing can render the `ClarificationCard`
    /// without a second event channel), emit
    /// `clarification-request`, and return an already-closed stream
    /// so the desktop relay promptly fires `message-complete`.
    ///
    /// No Fast-slot synthesis runs. No retrieval runs. The only cost
    /// is saving one message + emitting one event — the whole point
    /// of the Ask move is cheap engagement when confidence is low.
    pub(crate) async fn handle_ask_move_stream(
        &self,
        original_message: &str,
        conversation_id: &str,
        session_id: &str,
        classification: &RouterClassification,
    ) -> Result<StreamHandle> {
        let message_id = uuid::Uuid::new_v4().to_string();

        // Glassbox surfacing: emit a "deliberating, about to ask"
        // narration chip BEFORE the clarification card so the user
        // sees the system's "let me check first" moment instead of
        // the card popping in fully formed. The Ask path runs in
        // milliseconds — well below `NARRATION_MIN_ELAPSED` — so we
        // bypass `try_emit_narration` and build the event directly.
        // The whole point of the chip here is to fire fast; gating
        // would defeat it.
        emit_ask_deliberation_chip(
            self.routing_events.as_ref(),
            session_id,
            conversation_id,
            classification,
        )
        .await;

        // Brief pause so the user registers the chip before the
        // clarification card lands underneath it. Below ~250ms
        // feels jumpy; above ~700ms feels deliberate-to-the-point-
        // of-theatre. 400ms is the empirical sweet spot — long
        // enough to read "I'm not sure — let me ask," short enough
        // not to feel slow.
        tokio::time::sleep(std::time::Duration::from_millis(
            ASK_MOVE_DELIBERATION_LINGER_MS,
        ))
        .await;

        // Build clarification payload from the classifier's
        // alternatives. If the heuristic surfaced fewer than two, pad
        // with a free-text prompt so the user always has a way forward.
        let question =
            build_clarification_question(original_message, &classification.primary.intent);
        let options: Vec<ClarificationOption> = classification
            .alternatives
            .iter()
            .map(|c| ClarificationOption {
                label: label_for_intent(&c.intent),
                follow_up: original_message.to_string(),
                intent_hint: intent_hint(&c.intent),
            })
            .collect();

        let clarification_payload = ClarificationRequest {
            session_id: session_id.to_string(),
            conversation_id: conversation_id.to_string(),
            question: question.clone(),
            options: options.clone(),
        };

        // Persist a placeholder assistant message so the turn shows
        // up in history. Body is intentionally terse — the
        // ClarificationCard above the message is the actual UX.
        let placeholder_body =
            "I want to make sure I give you the right shape of answer.".to_string();
        let metadata = serde_json::json!({
            "move_kind": "ask",
            "confidence": classification.primary.confidence,
            "clarification": {
                "session_id": session_id,
                "question": question,
                "options": options,
            },
            "coarse_intent": classification.coarse_intent,
        });
        let assistant_msg = Message {
            id: message_id.clone(),
            conversation_id: conversation_id.to_string(),
            role: Role::Assistant,
            content: placeholder_body.clone(),
            created_at: now(),
            metadata: Some(metadata),
            version: 0,
        };
        self.store.save_message(&assistant_msg).await?;

        // Emit the clarification event (no-op for NoOpRoutingEventSink,
        // Tauri emit in desktop builds).
        self.routing_events
            .emit_clarification_request(clarification_payload)
            .await;

        tracing::info!(
            session_id,
            conversation_id,
            options = classification.alternatives.len(),
            "routing:ask — clarification requested, synthesis suppressed"
        );

        // Return an already-closed stream. The desktop relay reads
        // until the stream ends, then fetches metadata and fires
        // `message-complete` as normal.
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<String>>(1);
        // Send the placeholder text as one chunk so the bubble
        // renders immediately and the UI can read metadata. Drop `tx`
        // right after so the relay sees EOF on the next poll.
        let _ = tx.send(Ok(placeholder_body)).await;
        drop(tx);

        let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        Ok(StreamHandle {
            message_id,
            stream: Box::pin(stream),
        })
    }
}
