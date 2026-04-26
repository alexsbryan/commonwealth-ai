use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{Error, Result};
use crate::traits::{InferenceProvider, StateStore};
use crate::types::*;

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Build a ConversationContext from the store, creating the conversation if it doesn't exist.
/// The `query` parameter is used for memory retrieval (FTS5 matching).
pub async fn build_context(
    store: &dyn StateStore,
    conversation_id: &str,
    query: &str,
) -> Result<ConversationContext> {
    let conversation = match store.get_conversation(conversation_id).await {
        Ok(c) => c,
        Err(Error::NotFound(_)) => Conversation {
            id: conversation_id.to_string(),
            title: None,
            messages: Vec::new(),
            created_at: now(),
            updated_at: now(),
            version: 0,
            deleted_at: None,
            skill_id: None,
        },
        Err(e) => return Err(e),
    };

    let memories = store
        .get_relevant_memories(query, 5)
        .await
        .unwrap_or_default();

    let installed_corpora = store
        .list_corpus_states()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s.deleted_at.is_none())
        .map(|s| s.corpus_id)
        .collect();

    // Check for an active document session in this conversation.
    let document_session = store
        .get_document_session_by_conversation(conversation_id)
        .await
        .unwrap_or(None);

    Ok(ConversationContext {
        conversation,
        memories,
        working_memory: None,
        installed_corpora,
        document_session,
        topic_context: None,
        // None here is intentional: landscape digests are spliced
        // in by the Runtime after skill routing completes.
        // See `ConversationContext::set_landscape_digests` and
        // the KnowledgeViewManager integration note on the field
        // docs.
        knowledge_view_digests: None,
    })
}

/// Update the conversation's topic context by extracting the dominant topic
/// and domain from recent messages. Uses a Fast-slot inference call.
///
/// Returns a new `ConversationTopicContext` reflecting the current conversation
/// state. If inference fails, returns a default context rather than propagating
/// the error (topic tracking is best-effort, not critical path).
pub async fn update_topic_context(
    inference: &dyn InferenceProvider,
    messages: &[Message],
    previous: Option<&ConversationTopicContext>,
    document_session: Option<&DocumentSession>,
) -> Result<ConversationTopicContext> {
    tracing::debug!(
        messages = messages.len(),
        has_previous = previous.is_some(),
        has_document_session = document_session.is_some(),
        "context: update_topic_context — begin"
    );

    // Need at least one user message to extract a topic.
    let recent: Vec<_> = messages
        .iter()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    if recent.is_empty() {
        tracing::debug!("context: update_topic_context — no messages, returning default");
        return Ok(ConversationTopicContext::default());
    }

    let history_summary: String = recent
        .iter()
        .map(|m| {
            let role = match m.role {
                Role::User => "User",
                Role::Assistant => "Assistant",
                Role::System => "System",
            };
            let mut end = m.content.len().min(150);
            // Walk back to a valid char boundary if we landed mid-character.
            while end > 0 && !m.content.is_char_boundary(end) {
                end -= 1;
            }
            let content = &m.content[..end];
            format!("{role}: {content}")
        })
        .collect::<Vec<_>>()
        .join("\n");

    let previous_info = if let Some(prev) = previous {
        format!(
            "\nPrevious topic: {}\nPrevious domain: {}",
            prev.topic.as_deref().unwrap_or("none"),
            prev.domain.as_deref().unwrap_or("none"),
        )
    } else {
        String::new()
    };

    let prompt = format!(
        "Extract the topic and domain from this conversation.\n\
         {previous_info}\n\
         Recent messages:\n{history_summary}\n\n\
         Reply with JSON only, no explanation:\n\
         {{\"topic\": \"short phrase\", \"domain\": \"one word\"}}"
    );

    let request = CompletionRequest {
        prompt,
        system_message: None,
        preferred_speed: Speed::Fast,
        max_tokens: Some(60),
        temperature: Some(0.0),
        think_budget: Some(0),
        structured_output: None,
        top_k: None,
        top_p: None,
        oicp: None,
                tools: None,
                tool_choice: None,
                    model_id: None,
    };

    let response = inference.complete(&request).await?;
    let raw = response.text.trim();

    // Parse the JSON response. Strip markdown fences if the model wraps them.
    let json_str = raw
        .strip_prefix("```json")
        .and_then(|s| s.strip_suffix("```"))
        .unwrap_or(raw)
        .trim();

    let topic = if let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str) {
        let new_topic = val["topic"].as_str().map(|s| s.to_string());
        let new_domain = val["domain"].as_str().map(|s| s.to_string());

        // Determine turn depth: increment if topic matches, reset on pivot.
        let prev_depth = previous.map(|p| p.turn_depth).unwrap_or(0);
        let topic_matches = match (new_topic.as_deref(), previous.and_then(|p| p.topic.as_deref())) {
            (Some(new), Some(old)) => {
                // Fuzzy match: if the new topic contains the old or vice versa.
                let new_lower = new.to_lowercase();
                let old_lower = old.to_lowercase();
                new_lower.contains(&old_lower) || old_lower.contains(&new_lower)
            }
            _ => false,
        };
        let turn_depth = if topic_matches { prev_depth + 1 } else { 1 };

        // If a document session is active, anchor to it.
        let anchored_source = document_session
            .map(|ds| ds.filename.clone())
            .or_else(|| previous.and_then(|p| p.anchored_source.clone()));

        ConversationTopicContext {
            topic: new_topic,
            domain: new_domain,
            anchored_source,
            turn_depth,
        }
    } else {
        tracing::debug!(raw = %raw, "Failed to parse topic context JSON — using default");
        ConversationTopicContext::default()
    };

    tracing::debug!(
        topic = ?topic.topic,
        domain = ?topic.domain,
        anchored_source = ?topic.anchored_source,
        turn_depth = topic.turn_depth,
        "context: update_topic_context — done"
    );

    Ok(topic)
}

/// Format conversation messages into a prompt string for the model.
/// Takes the last `max_messages` to stay within context limits.
pub fn format_history_as_prompt(context: &ConversationContext, max_messages: usize) -> String {
    let messages = &context.conversation.messages;
    let start = messages.len().saturating_sub(max_messages);
    let recent = &messages[start..];

    if recent.is_empty() {
        return String::new();
    }

    let mut parts = Vec::new();
    for msg in recent {
        let role = match msg.role {
            Role::User => "User",
            Role::Assistant => "Assistant",
            Role::System => "System",
        };
        parts.push(format!("{role}: {}", msg.content));
    }

    parts.join("\n\n")
}
