// SPDX-License-Identifier: AGPL-3.0-or-later
//! A `routing_log` row must be joinable to the turn it routed.
//!
//! The table was keyed by `message_hash` alone, so two conversations asking
//! the same question produced two rows indistinguishable from one
//! conversation asking twice — and the post-router `IntentPolicy` override
//! was recorded nowhere at all.

use std::sync::Arc;

use sovereign_core::router::LlmRouter;
use sovereign_core::traits::{InferenceProvider, Router, StateStore};
use sovereign_core::types::{Conversation, ConversationContext};
use sovereign_core::SkillRegistry;
use sovereign_store::sqlite::SqliteStateStore;

use crate::harness::DeterministicInference;

fn context_for(conversation_id: &str) -> ConversationContext {
    ConversationContext {
        conversation: Conversation {
            id: conversation_id.to_string(),
            title: None,
            messages: Vec::new(),
            created_at: 0,
            updated_at: 0,
            version: 0,
            deleted_at: None,
            skill_id: None,
            enabled_corpora: None,
            searched_sources: None,
        },
        memories: Vec::new(),
        working_memory: None,
        installed_corpora: vec![],
        corpus_ceiling: None,
        document_session: None,
        topic_context: None,
        knowledge_view_digests: None,
        temporal_tensions: Vec::new(),
        compacted_history: None,
        history_retrieval_hits: None,
        tool_dossier: None,
        intent_policy: None,
    }
}

/// The router writes the conversation it was handed, at INSERT time.
///
/// Drives the real `LlmRouter::classify` down the conversation-locator
/// direct route (router.rs pre-check -3), which returns without an
/// inference call — so what this asserts is the threading, not the
/// classifier. Revert `log_route` to a bare `self.store.log_routing(..)`
/// with no conversation and the first assertion goes red.
#[tokio::test]
async fn routing_log_row_joins_to_its_conversation() {
    let store = Arc::new(SqliteStateStore::open_in_memory().expect("in-memory store"));
    let router = LlmRouter::new(
        Arc::new(DeterministicInference) as Arc<dyn InferenceProvider>,
        Arc::clone(&store) as Arc<dyn StateStore>,
        Arc::new(SkillRegistry::new()),
    );

    let conversation_id = "conv-join-test";
    let message = "what was the first thing I asked you in this conversation?";
    router
        .classify(message, &context_for(conversation_id), &[])
        .await
        .expect("classify");

    let hash = sovereign_core::router::message_hash(message);
    let (conv, policy_intent) = store
        .read_routing_join(&hash)
        .await
        .expect("the router wrote a routing_log row");
    assert_eq!(
        conv.as_deref(),
        Some(conversation_id),
        "the row must name the conversation it routed"
    );
    assert_eq!(
        policy_intent, None,
        "the router writes no override — NULL means the router's verdict stood"
    );
}
