// SPDX-License-Identifier: AGPL-3.0-or-later
//! ConationQuery dispatch — adjustments to the prior assistant turn.
//! The handler rebinds against the prior turn's intent and re-issues
//! synthesis with the user's tweak folded in.

use crate::error::Result;

use super::super::*;

impl Runtime {
    /// Handle ConationQuery: act on the prior assistant turn as a
    /// situated artifact. We do NOT reclassify or re-retrieve — we
    /// transform the prior reply with a style directive, or cancel
    /// the in-flight session. The whole point of the situated design
    /// is that the artifact is already there; conation just adjusts
    /// how it's expressed.
    pub(crate) async fn handle_conation_query(
        &self,
        message: &str,
        conversation_id: &str,
        context: &ConversationContext,
    ) -> Result<Response> {
        let lower = message.to_lowercase();
        let lower_tr = lower.trim();

        // TEACHABLE P0 capture: durative coaching ("always…", "keep
        // answers shorter from now on", "stop <gerund>…") forks a
        // detached lesson draft — see `crate::lessons`. The turn's
        // normal behavior below (cancel / empty-state / transform) is
        // byte-identical: the spawn never blocks or fails the turn,
        // and deictic conation ("make this shorter") carries no
        // durative marker so it never reaches the spawn. Consent
        // happens later on the card; nothing is stored here.
        if crate::lessons::detect_durative(&lower) {
            let prior_id = context
                .conversation
                .messages
                .iter()
                .rev()
                .find(|m| m.role == Role::Assistant)
                .map(|m| m.id.clone())
                .unwrap_or_default();
            let session_id = self
                .sessions
                .latest_for_conversation(conversation_id)
                .map(|s| s.id.clone());
            let inference = std::sync::Arc::clone(&self.inference);
            let approval = std::sync::Arc::clone(&self.approval);
            let routing_events = std::sync::Arc::clone(&self.routing_events);
            let cid = conversation_id.to_string();
            let msg = message.to_string();
            tokio::spawn(async move {
                crate::lessons::capture_lesson(
                    inference,
                    approval,
                    routing_events,
                    session_id,
                    cid,
                    prior_id,
                    msg,
                )
                .await;
            });
        }

        // Cancel sub-shape — short-circuits without synthesis.
        let is_cancel = ["stop", "cancel", "abort", "halt"].iter().any(|k| {
            lower_tr == *k
                || lower_tr.starts_with(&format!("{k} "))
                || lower_tr.starts_with(&format!("{k},"))
        });
        if is_cancel {
            if let Some(s) = self.sessions.latest_for_conversation(conversation_id) {
                s.cancel.cancel();
                tracing::info!(session = %s.id, "ConationQuery: cancelled in-flight session");
            }
            let response_msg = Message {
                id: uuid::Uuid::new_v4().to_string(),
                conversation_id: conversation_id.to_string(),
                role: Role::Assistant,
                content: "Stopped.".to_string(),
                created_at: now(),
                metadata: Some(serde_json::json!({
                    "intent": "ConationQuery",
                    "subshape": "cancel",
                })),
                version: 0,
            };
            return Ok(Response {
                message: response_msg,
                task: None,
                metrics: None,
            });
        }

        // Find the prior user message + assistant reply to transform.
        let last_assistant: Option<&Message> = context
            .conversation
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant);
        let last_user: Option<&Message> = context
            .conversation
            .messages
            .iter()
            .rev()
            .skip_while(|m| m.role != Role::Assistant)
            .find(|m| m.role == Role::User);

        if last_assistant.is_none() {
            let empty = "I don't see a previous reply to act on \u{2014} could you rephrase \
                         what you'd like?"
                .to_string();
            let response_msg = Message {
                id: uuid::Uuid::new_v4().to_string(),
                conversation_id: conversation_id.to_string(),
                role: Role::Assistant,
                content: empty,
                created_at: now(),
                metadata: Some(serde_json::json!({
                    "intent": "ConationQuery",
                    "subshape": "empty_state",
                })),
                version: 0,
            };
            return Ok(Response {
                message: response_msg,
                task: None,
                metrics: None,
            });
        }
        let prior_assistant = last_assistant.unwrap();
        let prior_user_text = last_user.map(|m| m.content.as_str()).unwrap_or("");

        // Map the directive to a transformation cue.
        let directive_phrase = if lower.contains("shorter")
            || lower.contains("terse")
            || lower.contains("concise")
            || lower.contains("tldr")
            || lower.contains("skip")
        {
            "Produce a shorter version of the prior reply. Skip preamble and recapping; \
             keep only the load-bearing claims."
        } else if lower.contains("longer")
            || lower.contains("more detail")
            || lower.contains("expand")
            || lower.contains("elaborate")
        {
            "Produce a more detailed version of the prior reply with worked examples \
             and additional context. Keep the same factual claims."
        } else if lower.contains("slower")
            || lower.contains("step by step")
            || lower.contains("walk through")
        {
            "Re-express the prior reply as a step-by-step walkthrough. Number the steps; \
             keep one idea per step."
        } else {
            // Default for "try again" / "retry" / "regenerate" / unrecognised conation.
            "Re-express the prior reply with a fresh phrasing while keeping all factual \
             claims intact."
        };

        let prompt = format!(
            "PRIOR USER QUESTION: {prior_user_text}\n\n\
             PRIOR ASSISTANT REPLY:\n{prior_reply}\n\n\
             DIRECTIVE: {directive_phrase}\n\n\
             Produce the adjusted reply. Apply only the requested change; do not \
             introduce new factual claims.",
            prior_reply = prior_assistant.content,
        );
        let request = CompletionRequest {
            prompt,
            system_message: None,
            preferred_speed: Speed::Fast,
            max_tokens: Some(FAST_KNOWLEDGE_MAX_TOKENS as usize),
            temperature: Some(self.inference_config.temperature),
            think_budget: Some(0),
            structured_output: None,
            top_k: self.inference_config.top_k,
            top_p: None,
            oicp: None,
            tools: None,
            tool_choice: None,
            model_id: None,
            enable_thinking: None,
            sampling_mode: None,
            assistant_prefix: None,
            cmd_prefix: None,
            url_allowlist: None,
            evidence_id_allowlist: None,
            lark_grammar: None,
            prompt_shape: None,
        };
        let completion = self.inference.complete(&request).await?;
        let response_msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            role: Role::Assistant,
            content: completion.text,
            created_at: now(),
            metadata: Some(serde_json::json!({
                "intent": "ConationQuery",
                "subshape": "transform",
                "prior_message_id": prior_assistant.id,
            })),
            version: 0,
        };
        Ok(Response {
            message: response_msg,
            task: None,
            metrics: None,
        })
    }
}
