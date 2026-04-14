//! Conversation title generation.
//!
//! Two responsibilities:
//!
//! 1. `generate_title_from_messages` — pure "given these messages, produce a
//!    short title" function. Uses the Fast slot, no thinking budget, low
//!    temperature. Strips quotes and punctuation the model likes to add.
//!
//! 2. `try_auto_title` — gated helper that fetches the conversation, decides
//!    whether a title should be generated (no existing title, at least one
//!    full user+assistant exchange), generates one, and persists it via
//!    `ConversationStore::update_conversation_title`.
//!
//! `try_auto_title` is idempotent — calling it on a conversation that already
//! has a title is a no-op that returns `Ok(None)`. Safe to call after every
//! assistant save.

use crate::error::Result;
use crate::traits::{InferenceProvider, StateStore};
use crate::types::{CompletionRequest, Message, Role, Speed};

/// Hard cap on the characters we include from a single message so the prompt
/// stays small — title prompts run on the Fast slot and must be cheap.
const MESSAGE_SNIPPET_CHARS: usize = 400;

/// Maximum tokens the model may emit for the title. A 4–8 word title is
/// typically well under 30 tokens; this cap bounds the worst case.
const TITLE_MAX_TOKENS: usize = 30;

/// Hard cap on the stored title length (characters, not tokens).
const TITLE_MAX_CHARS: usize = 120;

/// Minimum message count before we generate a title — we want at least one
/// user turn and one assistant turn so the model has something to summarise.
const MIN_MESSAGES_FOR_TITLE: usize = 2;

/// Produce a short conversation title from the first user+assistant exchange.
///
/// Runs on the Fast slot with `think_budget: 0`. The result is post-processed
/// to strip leading/trailing quotes, trailing periods, and any newlines the
/// model likes to add.
pub async fn generate_title_from_messages(
    inference: &dyn InferenceProvider,
    messages: &[Message],
) -> Result<String> {
    // Find the first user and first assistant message — not necessarily
    // messages[0] and messages[1], though in practice they usually are.
    let user_msg = messages
        .iter()
        .find(|m| m.role == Role::User)
        .map(|m| m.content.as_str())
        .unwrap_or("");
    let assistant_msg = messages
        .iter()
        .find(|m| m.role == Role::Assistant)
        .map(|m| m.content.as_str())
        .unwrap_or("");

    let user_snippet = truncate_to_char_boundary(user_msg, MESSAGE_SNIPPET_CHARS);
    let assistant_snippet = truncate_to_char_boundary(assistant_msg, MESSAGE_SNIPPET_CHARS);

    let prompt = format!(
        "Write a short, specific title (4-8 words) for this conversation. \
         Use sentence case. Do not wrap it in quotes. Do not end with a period. \
         Do not add any explanation — just the title.\n\n\
         User: {user_snippet}\n\n\
         Assistant: {assistant_snippet}\n\n\
         Title:"
    );

    let request = CompletionRequest {
        prompt,
        system_message: None,
        preferred_speed: Speed::Fast,
        max_tokens: Some(TITLE_MAX_TOKENS),
        temperature: Some(0.3),
        think_budget: Some(0),
        structured_output: None,
        top_k: None,
        top_p: None,
        oicp: None,
    };

    let response = inference.complete(&request).await?;
    let cleaned = sanitize_title(&response.text);

    tracing::debug!(
        title = %cleaned,
        model = %response.model_id,
        latency_ms = response.latency_ms,
        "title: generated"
    );

    Ok(cleaned)
}

/// Gate + generate + persist. Safe to call after every assistant message save.
///
/// - `Ok(None)` when the conversation already has a title, or there are not
///   yet enough messages to generate a meaningful one.
/// - `Ok(Some(title))` when a new title was generated and saved.
/// - `Err(_)` only when the store fails; inference failures are wrapped
///   through `generate_title_from_messages`.
pub async fn try_auto_title(
    inference: &dyn InferenceProvider,
    store: &dyn StateStore,
    conversation_id: &str,
) -> Result<Option<String>> {
    let conversation = store.get_conversation(conversation_id).await?;

    if conversation.title.is_some() {
        tracing::debug!(conversation_id = %conversation_id, "title: already set, skipping");
        return Ok(None);
    }

    if conversation.messages.len() < MIN_MESSAGES_FOR_TITLE {
        tracing::debug!(
            conversation_id = %conversation_id,
            messages = conversation.messages.len(),
            "title: not enough messages yet"
        );
        return Ok(None);
    }

    // Confirm we have at least one user AND one assistant message — otherwise
    // the prompt would be lopsided.
    let has_user = conversation.messages.iter().any(|m| m.role == Role::User);
    let has_assistant = conversation
        .messages
        .iter()
        .any(|m| m.role == Role::Assistant);
    if !has_user || !has_assistant {
        tracing::debug!(
            conversation_id = %conversation_id,
            has_user,
            has_assistant,
            "title: missing one side of the exchange"
        );
        return Ok(None);
    }

    let title = generate_title_from_messages(inference, &conversation.messages).await?;

    if title.is_empty() {
        tracing::warn!(
            conversation_id = %conversation_id,
            "title: generated title was empty after sanitisation, skipping save"
        );
        return Ok(None);
    }

    store
        .update_conversation_title(conversation_id, &title)
        .await?;

    tracing::info!(
        conversation_id = %conversation_id,
        title = %title,
        "title: auto-generated and saved"
    );

    Ok(Some(title))
}

/// Clean up model output into a storable title:
/// - strip outer quotes (both straight and curly)
/// - strip trailing period
/// - collapse whitespace
/// - trim to TITLE_MAX_CHARS
/// - drop anything after the first newline (models sometimes explain)
fn sanitize_title(raw: &str) -> String {
    // First-line only — the prompt asks for no explanation, but models
    // occasionally ignore that. Take just the first non-empty line.
    let first_line = raw
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");

    let mut s = first_line.trim().to_string();

    // Strip a wrapping pair of quotes if the model added them.
    for _ in 0..2 {
        if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
            || (s.starts_with('\u{201C}') && s.ends_with('\u{201D}') && s.len() >= 2)
            || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
        {
            s = s
                .chars()
                .skip(1)
                .take(s.chars().count().saturating_sub(2))
                .collect();
            s = s.trim().to_string();
        }
    }

    // Strip trailing period (but not "!" or "?") — it was explicitly disallowed
    // in the prompt, but catch it just in case.
    while s.ends_with('.') {
        s.pop();
    }
    s = s.trim().to_string();

    // Clamp to max chars at a char boundary.
    if s.chars().count() > TITLE_MAX_CHARS {
        s = s.chars().take(TITLE_MAX_CHARS).collect();
    }

    s
}

/// Truncate a string to at most `max` bytes, walking back to a valid UTF-8
/// char boundary so we never split a multi-byte codepoint.
fn truncate_to_char_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_straight_quotes() {
        assert_eq!(sanitize_title("\"Hello world\""), "Hello world");
    }

    #[test]
    fn sanitize_strips_curly_quotes() {
        assert_eq!(
            sanitize_title("\u{201C}Hello world\u{201D}"),
            "Hello world"
        );
    }

    #[test]
    fn sanitize_strips_trailing_period() {
        assert_eq!(sanitize_title("Hello world."), "Hello world");
    }

    #[test]
    fn sanitize_keeps_question_and_exclaim() {
        assert_eq!(sanitize_title("What is quantum?"), "What is quantum?");
        assert_eq!(sanitize_title("Eureka!"), "Eureka!");
    }

    #[test]
    fn sanitize_takes_first_line_only() {
        assert_eq!(
            sanitize_title("Hello world\nExplanation: this title is about..."),
            "Hello world"
        );
    }

    #[test]
    fn sanitize_empty_yields_empty() {
        assert_eq!(sanitize_title(""), "");
        assert_eq!(sanitize_title("   \n  "), "");
    }

    #[test]
    fn truncate_respects_multibyte_boundary() {
        let s = "Schrödinger's cat";
        // byte position 7 lands inside 'ö' — should walk back.
        let t = truncate_to_char_boundary(s, 7);
        assert!(s.starts_with(t));
        // Valid UTF-8: no panic on str::chars().
        let _ = t.chars().count();
    }
}
