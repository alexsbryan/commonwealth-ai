// SPDX-License-Identifier: AGPL-3.0-or-later
//! Runner-mechanics tests for the retrieval pipeline: the runner must
//! execute steps strictly in list order, threading ONE mutable
//! [`PipelineState`] through, with the real `Runtime` type in the
//! signature (mocked step bodies — no embeddings, no model, no corpus).
//!
//! Retrieval *behavior* is deliberately not tested here: the unit-test
//! inference double returns zero vectors, so every chunk scores 0 and
//! the pipeline is degenerate. Behavior = live benches
//! (`bench all --synth`), per the pipeline-collapse plan.

mod harness;

use harness::TestHarness;
use sovereign_core::runtime::retrieval_pipeline::{
    step, PipelineState, RetrievalPipeline, StepFuture, StepOutcome,
};
use sovereign_core::runtime::Runtime;
use sovereign_core::types::*;

fn test_context() -> ConversationContext {
    ConversationContext {
        conversation: Conversation {
            id: "c1".to_string(),
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

fn marker_chunk(title: &str) -> corpus_engine::ScoredChunk {
    corpus_engine::ScoredChunk {
        content: title.to_string(),
        title: Some(title.to_string()),
        url: None,
        corpus_id: "test".to_string(),
        score: 1.0,
        metadata: std::collections::HashMap::new(),
        chunk_id: None,
        source_doc_id: None,
        vector_distance: None,
        // Fixture chunk: nothing acquired it (TOPOLOGY §10 rung 9.1).
        provenance: corpus_engine::index::ChunkProvenance::manufactured("test_fixture"),
    }
}

fn step_push_a<'a, 'ctx>(_rt: &'a Runtime, st: &'a mut PipelineState<'ctx>) -> StepFuture<'a> {
    Box::pin(async move {
        st.chunks.push(marker_chunk("a"));
        // Thread a non-chunk product too, so the test witnesses that
        // step products (not just the pool) survive across steps.
        st.entities.push("marker-entity".to_string());
        StepOutcome::default()
    })
}

fn step_push_b_after_a<'a, 'ctx>(
    _rt: &'a Runtime,
    st: &'a mut PipelineState<'ctx>,
) -> StepFuture<'a> {
    Box::pin(async move {
        // Ordering witness: step A's mutations must be visible here.
        assert_eq!(st.chunks.len(), 1, "step B ran before step A");
        assert_eq!(st.chunks[0].title.as_deref(), Some("a"));
        assert_eq!(st.entities, vec!["marker-entity".to_string()]);
        st.chunks.push(marker_chunk("b"));
        StepOutcome::default()
    })
}

fn step_drop_all<'a, 'ctx>(_rt: &'a Runtime, st: &'a mut PipelineState<'ctx>) -> StepFuture<'a> {
    Box::pin(async move {
        st.chunks.clear();
        StepOutcome {
            note: Some("dropped everything".to_string()),
        }
    })
}

#[tokio::test]
async fn runner_executes_steps_in_order_threading_state() {
    let h = TestHarness::new();
    let context = test_context();
    let intent = Intent::KnowledgeQuery;
    let mut state = PipelineState::new(
        "test question",
        &context,
        &intent,
        None,
        Vec::new(),
        "KnowledgeQuery",
        "KnowledgeQuery".to_string(),
        // A stage under test needs no providers — which is the point of
        // Phase 4a: a turn is drivable without wiring an enrichment stack.
        sovereign_core::runtime::Lane::none(),
    );
    let pipeline = RetrievalPipeline {
        name: "test",
        steps: vec![
            step("push_a", None, step_push_a),
            step("push_b", None, step_push_b_after_a),
        ],
    };
    pipeline.run(&h.runtime, &mut state).await;
    let titles: Vec<_> = state
        .chunks
        .iter()
        .map(|c| c.title.as_deref().unwrap_or(""))
        .collect();
    assert_eq!(titles, vec!["a", "b"]);
}

#[tokio::test]
async fn runner_survives_steps_that_shrink_the_pool() {
    // The trace computes a signed delta; a step that REMOVES chunks
    // (noise floor, scope filter, truncate) must not confuse the
    // runner or later steps.
    let h = TestHarness::new();
    let context = test_context();
    let intent = Intent::DeepQuery;
    let mut state = PipelineState::new(
        "test question",
        &context,
        &intent,
        None,
        Vec::new(),
        "DeepQuery",
        "DeepQuery".to_string(),
        // A stage under test needs no providers — which is the point of
        // Phase 4a: a turn is drivable without wiring an enrichment stack.
        sovereign_core::runtime::Lane::none(),
    );
    let pipeline = RetrievalPipeline {
        name: "test",
        steps: vec![
            step("push_a", None, step_push_a),
            step("drop_all", None, step_drop_all),
            step("push_a", None, step_push_a),
        ],
    };
    pipeline.run(&h.runtime, &mut state).await;
    assert_eq!(state.chunks.len(), 1);
    // Products written by earlier steps persist across the drop.
    assert_eq!(state.entities.len(), 2);
}
