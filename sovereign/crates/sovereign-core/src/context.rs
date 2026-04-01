use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{Error, Result};
use crate::traits::StateStore;
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
        },
        Err(e) => return Err(e),
    };

    let memories = store
        .get_relevant_memories(query, 5)
        .await
        .unwrap_or_default();

    Ok(ConversationContext {
        conversation,
        memories,
        working_memory: None,
    })
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
