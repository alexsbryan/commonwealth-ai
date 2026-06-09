// SPDX-License-Identifier: AGPL-3.0-or-later
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
            enabled_corpora: None,
            searched_sources: None,
        },
        Err(e) => return Err(e),
    };

    // Scope the recall to the conversation's skill — the inner-work
    // memory wall enforces that scoped pools (e.g. `inner-work`) only
    // recall their own memories, and general pools never see scoped
    // memories. Without this, a general chat could surface a memory
    // extracted in inner-work, breaching the trust contract behind
    // the wall. See `MemoryScope` docs for the bidirectional invariant.
    let scope =
        crate::traits::MemoryScope::from_conversation_skill(conversation.skill_id.as_deref());
    let memories = store
        .get_relevant_memories_for_scope(&scope, query, 5)
        .await
        .unwrap_or_default();

    // Apply the conversation's per-turn corpus allow-list, if any.
    // `None` (the default for fresh + legacy conversations) means
    // "all installed corpora" — bit-identical to pre-feature behavior.
    // `Some(allow)` restricts the prompt-side display + the retrieval
    // filter to just the corpus_ids in the allow-list. Layer/satellite
    // expansion happens at retrieval time (where IndexInfo carries
    // `parent_corpus_id`); here we only do the parent-level
    // intersection that drives the model's "installed corpora" prompt
    // list. See `Conversation::enabled_corpora` docs.
    let all_installed: Vec<String> = store
        .list_corpus_states()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s.deleted_at.is_none())
        .map(|s| s.corpus_id)
        .collect();
    let installed_corpora: Vec<String> = match &conversation.enabled_corpora {
        Some(allow) => {
            let allow_set: std::collections::HashSet<&str> =
                allow.iter().map(String::as_str).collect();
            all_installed
                .into_iter()
                .filter(|id| allow_set.contains(id.as_str()))
                .collect()
        }
        None => all_installed,
    };

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
        // Empty initially; populated by the Runtime via
        // `memory::detect_temporal_tensions` after memory load,
        // before prompt assembly, only for relational-register
        // skills. Empty also means "no tensions found / pre-pass
        // skipped" — the renderer simply omits the section.
        temporal_tensions: Vec::new(),
        // None until Runtime runs `maybe_compact_dropped_history`
        // after the conversation grows past the visible window.
        compacted_history: None,
        history_retrieval_hits: None,
        tool_dossier: None,
        intent_policy: None,
    })
}

/// Update the conversation's topic context by extracting the dominant topic
/// and domain from recent messages. Uses a Fast-slot inference call.
///
/// Returns a new `ConversationTopicContext` reflecting the current conversation
/// state. If inference fails, returns a default context rather than propagating
/// the error (topic tracking is best-effort, not critical path).
/// Summarize a window of dropped conversation turns into a compact
/// "earlier conversation" preamble. Returned summary is prepended to
/// the visible-history block in the synthesis system prompt by
/// `runtime::format_conversation_history` so the model retains
/// access to entities, decisions, and topical arc that have rolled
/// off the verbatim window.
///
/// The Fast-slot extractor is JSON-schema-constrained so the parse
/// either succeeds or fails loudly. A failure leaves
/// `compacted_history = None`; the synthesis path then operates on
/// just the visible window — graceful degradation, not silent loss.
///
/// Surfaced by `sovereign/bench/wikipedia_learn` 2026-05-17 marathon
/// thread: a 12-turn arc with callbacks across turns blows past the
/// 8-message visible window. Without this primitive, turn 11's
/// "Going back to Babbage's original vision …" finds no Babbage in
/// the prompt because T0/T1 fell off the rolling window.
pub async fn summarize_dropped_history(
    inference: &dyn InferenceProvider,
    dropped: &[Message],
) -> Result<Option<String>> {
    if dropped.is_empty() {
        return Ok(None);
    }

    let transcript: String = dropped
        .iter()
        .map(|m| {
            let role = match m.role {
                Role::User => "User",
                Role::Assistant => "Assistant",
                Role::System => "System",
            };
            let mut end = m.content.len().min(400);
            while end > 0 && !m.content.is_char_boundary(end) {
                end -= 1;
            }
            format!("{role}: {}", &m.content[..end])
        })
        .collect::<Vec<_>>()
        .join("\n");

    let prompt = format!(
        "Summarize the conversation excerpt below into a compact \
         preamble (≤120 words) the assistant can read at the top of \
         a continuing conversation. Keep named entities, decisions \
         the user made, and the topical arc; drop pleasantries and \
         filler. Write in past tense, third-person (e.g., \"The user \
         asked about X. The assistant explained Y, Z.\").\n\n\
         Excerpt:\n{transcript}\n\n\
         Reply with JSON only:\n\
         {{\"summary\": \"…\"}}"
    );

    let schema = serde_json::json!({
        "type": "object",
        "properties": { "summary": {"type": "string"} },
        "required": ["summary"],
    });

    let request = CompletionRequest {
        prompt,
        system_message: None,
        preferred_speed: Speed::Fast,
        max_tokens: Some(400),
        temperature: Some(0.0),
        think_budget: Some(0),
        structured_output: Some(schema),
        top_k: None,
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
    };

    let response = inference.complete(&request).await?;
    let raw = response.text.trim();
    let json_str = raw
        .strip_prefix("```json")
        .and_then(|s| s.strip_suffix("```"))
        .unwrap_or(raw)
        .trim();

    let parsed: serde_json::Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(error = %e, raw = %raw, "summarize_dropped_history: parse failed");
            return Ok(None);
        }
    };
    let summary = parsed
        .get("summary")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if let Some(ref s) = summary {
        tracing::info!(
            dropped_messages = dropped.len(),
            summary_chars = s.chars().count(),
            "context: summarize_dropped_history — done"
        );
    }
    Ok(summary)
}

pub async fn update_topic_context(
    inference: &dyn InferenceProvider,
    messages: &[Message],
    previous: Option<&ConversationTopicContext>,
    document_session: Option<&DocumentSession>,
    incoming_user_message: Option<&str>,
) -> Result<ConversationTopicContext> {
    tracing::debug!(
        messages = messages.len(),
        has_previous = previous.is_some(),
        has_document_session = document_session.is_some(),
        has_incoming = incoming_user_message.is_some(),
        "context: update_topic_context — begin"
    );

    // Need at least one user message to extract a topic. The
    // incoming message (when present) is what makes pivot detection
    // possible — without it the extractor sees only the prior arc
    // and a learner question that pivots topic ("Why didn't
    // relativity win the Nobel?" after a photoelectric chain) leaves
    // the topic stuck on the prior subject. Surfaced by
    // sovereign/bench/wikipedia_learn 2026-05-17 (einstein T4
    // regressed 0.67→0.00 with topic still "photoelectric effect").
    let recent: Vec<_> = messages
        .iter()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    if recent.is_empty() && incoming_user_message.is_none() {
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

    let current_question_block = incoming_user_message
        .map(|m| {
            let mut end = m.len().min(200);
            while end > 0 && !m.is_char_boundary(end) {
                end -= 1;
            }
            format!("Current question:\n{}\n\n", &m[..end])
        })
        .unwrap_or_default();

    // Topic-extractor prompt. Two design choices, both motivated by
    // the wikipedia_learn bench (2026-05-17, v6 → v7):
    //
    // 1. **Lead with the current question**, frame prior turns as
    //    *context* for it. The extractor's job is to name what the
    //    user is asking about RIGHT NOW. A learner who pivots
    //    (photoelectric → relativity) should produce a topic that
    //    follows the pivot, not one weighted by the long assistant
    //    answer about the prior subject.
    //
    // 2. **Don't show `previous topic` to the extractor.** Passing
    //    it in biased the 2B/4B classifier toward continuity ("the
    //    prior topic was X, so the next must be X-related"). Topic
    //    continuity belongs in the post-extraction `turn_depth`
    //    comparison, not in the prompt that decides the topic.
    let prompt = format!(
        "Identify what the user is asking about in the CURRENT question. \
         Prior turns are context — the topic should reflect the user's \
         present intent, even if it pivots from earlier in the conversation.\n\n\
         {current_question_block}\
         Prior conversation (most recent last):\n{history_summary}\n\n\
         Reply with JSON only:\n\
         {{\"topic\": \"short noun phrase naming the current subject\", \
         \"domain\": \"one-word category (history, physics, biology, …)\"}}"
    );
    let _ = previous; // retained for turn_depth comparison below

    // Constrain the Fast-slot output to the topic/domain JSON shape.
    // The Fast slot is a 2B model whose free-form JSON is often
    // malformed (extra prose, missing braces, escaped strings) —
    // under serde_json::from_str the response then falls to default,
    // leaving `topic = None` for every turn. With a JSON-schema
    // constraint the daemon enforces the grammar at the token level,
    // so the parse below either succeeds or surfaces a real signal.
    // Surfaced by `sovereign/bench/wikipedia_learn` 2026-05-17
    // (topic anchored retrieval query saw `topic=None` on every
    // turn because the extractor silently failed).
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "topic": {"type": "string"},
            "domain": {"type": "string"},
        },
        "required": ["topic", "domain"],
    });

    let request = CompletionRequest {
        prompt,
        system_message: None,
        preferred_speed: Speed::Fast,
        max_tokens: Some(60),
        temperature: Some(0.0),
        think_budget: Some(0),
        structured_output: Some(schema),
        top_k: None,
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
        let topic_matches = match (
            new_topic.as_deref(),
            previous.and_then(|p| p.topic.as_deref()),
        ) {
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
