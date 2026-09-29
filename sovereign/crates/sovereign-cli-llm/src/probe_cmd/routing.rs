// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn __probe` routing: the router's classifier alone, per question — no
//! retrieval, no synthesis, no expectation.

use std::time::Instant;

use sovereign_contracts::probe::{ProbeQuestion, RoutingEvidence};

use crate::chat_cmd::bootstrap::ChatSession;

/// Classify every question in order.
pub(crate) async fn probe(
    session: &ChatSession,
    questions: &[ProbeQuestion],
) -> Vec<RoutingEvidence> {
    let mut rows = Vec::with_capacity(questions.len());
    for q in questions {
        rows.push(probe_question(session, q).await);
    }
    rows
}

async fn probe_question(session: &ChatSession, q: &ProbeQuestion) -> RoutingEvidence {
    use sovereign_core::types::{
        ConversationContext, Effect, Idempotency, Latency, Scope, ToolDescriptor,
    };

    // Build a near-empty context. The classifier prompt reads
    // `installed_corpora` from this struct (it tells the model "we
    // have wikipedia, sep loaded — prefer LOOKUP for factual
    // questions"), so we mirror what `build_session` would have
    // surfaced. Skill hints / corrections are intentionally absent:
    // the eval scores BASE classifier behaviour, not the corrected
    // behaviour.
    let installed = session
        .corpus_engine
        .installed_indexes()
        .await
        .map(|ix| ix.into_iter().map(|i| i.corpus_id).collect::<Vec<_>>())
        .unwrap_or_default();
    let context = ConversationContext {
        conversation: sovereign_core::types::Conversation {
            id: "eval-routing".into(),
            title: None,
            messages: vec![],
            created_at: 0,
            updated_at: 0,
            version: 0,
            deleted_at: None,
            skill_id: None,
            enabled_corpora: None,
            searched_sources: None,
        },
        memories: vec![],
        working_memory: None,
        installed_corpora: installed,
        corpus_ceiling: None,
        document_session: None,
        topic_context: None,
        knowledge_view_digests: None,
        temporal_tensions: Vec::new(),
        compacted_history: None,
        history_retrieval_hits: None,
        tool_dossier: None,
        intent_policy: None,
    };

    // Expose a `web_search` tool descriptor so the router's
    // `force_action` gate (which checks `has_search` against
    // available_tools) fires under the same conditions as the
    // production desktop, where SearchTool is always registered.
    // Without this, routing-only eval underrepresents the daemon's
    // real behaviour — temporal/future questions fall through to
    // the LLM Pass 1 instead of taking the heuristic ACTION path.
    let eval_tools = vec![ToolDescriptor {
        id: "web_search".to_string(),
        name: "web_search".to_string(),
        description: "Search the web for current information".to_string(),
        parameters: serde_json::json!({}),
        examples: vec![],
        effect: Effect::Read,
        idempotency: Idempotency::Idempotent,
        latency: Latency::Slow,
        scope: Scope::External,
        output_schema: None,
    }];

    let t = Instant::now();
    let classification = match session
        .runtime
        .router
        .classify(&q.question, &context, &eval_tools)
        .await
    {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!(id = %q.id, error = %e, "classify failed");
            return RoutingEvidence {
                id: q.id.clone(),
                error: Some(e.to_string()),
                intent: String::new(),
                coarse_intent: None,
                confidence: 0.0,
                rationale: None,
                latency_ms: t.elapsed().as_millis() as u64,
            };
        }
    };
    let latency_ms = t.elapsed().as_millis() as u64;

    RoutingEvidence {
        id: q.id.clone(),
        error: None,
        intent: intent_wire_label(&classification.primary.intent),
        coarse_intent: classification.coarse_intent.clone(),
        confidence: classification.primary.confidence,
        rationale: classification.rationale.clone(),
        latency_ms,
    }
}

/// Lowercase wire form of an Intent — matches the strings used in the bank's
/// `expected_intent` field and the category-default map.
///
/// The `slug` column of the intent table. It was a thirteen-arm `match` here
/// until 2026-08-20, one of THREE independent implementations of this one wire
/// key (the others in `sovereign_core::router_embed::intent_label` and
/// `runtime::intent_helpers::intent_hint`). One key, one decider.
fn intent_wire_label(intent: &sovereign_core::types::Intent) -> String {
    intent.row().slug.to_string()
}
