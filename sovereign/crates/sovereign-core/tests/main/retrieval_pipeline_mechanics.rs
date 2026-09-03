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

use crate::harness::TestHarness;
use sovereign_core::runtime::retrieval_pipeline::{
    step, PipelineState, RetrievalPipeline, StepFuture, StepKind, StepOutcome,
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
            ..Default::default()
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
            step("push_a", StepKind::Injector, None, step_push_a),
            step("push_b", StepKind::Injector, None, step_push_b_after_a),
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
            step("push_a", StepKind::Injector, None, step_push_a),
            step(
                "drop_all",
                StepKind::Filter(
                    sovereign_core::runtime::retrieval_pipeline::DropReason::NotSelectedByObjective,
                ),
                None,
                step_drop_all,
            ),
            step("push_a", StepKind::Injector, None, step_push_a),
        ],
    };
    pipeline.run(&h.runtime, &mut state).await;
    assert_eq!(state.chunks.len(), 1);
    // Products written by earlier steps persist across the drop.
    assert_eq!(state.entities.len(), 2);
}

/// The violation counter is PROCESS-GLOBAL, so the two ledger tests below
/// must not run concurrently: the lying one deliberately increments it and the
/// honest one asserts it did not move. Nextest runs tests in parallel within a
/// binary, so without this lock the honest test reads the liar's increment and
/// fails for a reason that has nothing to do with the runner. Found by the
/// test itself on its first run.
/// `unwrap_or_else(|e| e.into_inner())` on the lock below is a NAMED
/// substitution (ARCH §18.3), not a swallowed error: this mutex guards no
/// data, only ordering, so a poisoned lock carries no corrupt state to
/// protect against. Recovering keeps a panicking test's own failure visible
/// instead of cascading a PoisonError into the sibling and hiding it.
static LEDGER_COUNTER: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Every pipeline this file runs must leave the ledger clean.
///
/// Cheap insurance with a wide blast radius: any future mechanics test that
/// builds a pipeline gets the accounting invariant for free, because the
/// counter is process-global and this asserts on the whole test binary's
/// total. A step that lies about its `StepKind` — the exact mutation used to
/// prove the eval gate fires — turns this red in milliseconds rather than
/// needing a bench run against a live daemon.
///
/// Not a substitute for the eval gate: that one watches the REAL pipeline
/// against REAL corpora. This one watches the runner.
#[tokio::test]
async fn the_runner_reports_no_ledger_violations_for_honest_steps() {
    use sovereign_core::runtime::retrieval_pipeline::ledger_violation_count;

    let _serial = LEDGER_COUNTER.lock().unwrap_or_else(|e| e.into_inner());
    let before = ledger_violation_count();

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
        sovereign_core::runtime::Lane::none(),
    );
    let pipeline = RetrievalPipeline {
        name: "ledger_honesty",
        steps: vec![
            step("push_a", StepKind::Injector, None, step_push_a),
            step(
                "drop_all",
                StepKind::Filter(
                    sovereign_core::runtime::retrieval_pipeline::DropReason::NotSelectedByObjective,
                ),
                None,
                step_drop_all,
            ),
        ],
    };
    pipeline.run(&h.runtime, &mut state).await;

    assert_eq!(
        ledger_violation_count(),
        before,
        "honest steps must produce no ledger violations; the runner flagged \
         {} — run with RUST_LOG=retrieval.pipeline=error to see which",
        ledger_violation_count() - before
    );
}

/// The negative control for the test above: a step that LIES about its kind
/// must be caught. Without this, `the_runner_reports_no_ledger_violations`
/// could pass because the check is broken rather than because the steps are
/// honest (ARCH §18.1 — a gate nobody has watched fail is not a gate).
#[tokio::test]
async fn a_step_that_lies_about_its_kind_is_caught() {
    use sovereign_core::runtime::retrieval_pipeline::ledger_violation_count;

    let _serial = LEDGER_COUNTER.lock().unwrap_or_else(|e| e.into_inner());
    let before = ledger_violation_count();

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
        sovereign_core::runtime::Lane::none(),
    );
    // `step_push_a` demonstrably ADDS a chunk. Declaring it Inert is a lie of
    // exactly the shape that let atlas grounding claim it had done its job.
    let pipeline = RetrievalPipeline {
        name: "ledger_lie",
        steps: vec![step("push_a", StepKind::Inert, None, step_push_a)],
    };
    pipeline.run(&h.runtime, &mut state).await;

    assert!(
        ledger_violation_count() > before,
        "a step declared Inert that added a chunk MUST be reported; the \
         violation counter did not move, so the invariant is not wired"
    );
}
