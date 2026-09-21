// SPDX-License-Identifier: AGPL-3.0-or-later
//! The DeepQuery streaming arm must report the gate decision it makes.
//!
//! `stream_deep_query_turn` computes `deep_gate_on` and `deep_hold` and, until
//! rb-deep-arm-gate-trace, emitted nothing — while its KnowledgeQuery twin has
//! reported the same decision under `target: "grounding_gate"` all along. A
//! Deep turn's gate state was therefore readable only by inference from the
//! rows that came after it.

use std::sync::Arc;

use futures::StreamExt;

use sovereign_core::types::Intent;

use crate::harness::TestHarness;

/// Captures the `grounding_gate` rows a turn emits. Same shape as
/// `PipelineCapture` in `retrieval_pipeline_mechanics.rs`, narrowed to this one
/// target; no level filter, so the DEBUG row on the empty-pool branch is
/// captured alongside the INFO decision.
#[derive(Default, Clone)]
struct GateCapture {
    rows: Arc<std::sync::Mutex<Vec<String>>>,
}

impl tracing::Subscriber for GateCapture {
    fn enabled(&self, meta: &tracing::Metadata<'_>) -> bool {
        meta.target() == "grounding_gate"
    }
    fn new_span(&self, _attrs: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct V(Vec<String>);
        impl tracing::field::Visit for V {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                self.0.push(format!("{}={:?}", field.name(), value));
            }
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                self.0.push(format!("{}={}", field.name(), value));
            }
        }
        let mut v = V(Vec::new());
        event.record(&mut v);
        self.rows.lock().unwrap().push(v.0.join(" "));
    }
    fn enter(&self, _span: &tracing::span::Id) {}
    fn exit(&self, _span: &tracing::span::Id) {}
}

/// One streamed DeepQuery turn must leave exactly one gate decision in the
/// trace, naming the route it decided for. `handle_message_stream_as` pins the
/// intent, so the turn is a DeepQuery one by construction rather than by the
/// router's mood.
#[tokio::test]
async fn deep_stream_emits_its_gate_decision() {
    let harness = TestHarness::new();
    let conv = uuid::Uuid::new_v4().to_string();
    let sink = GateCapture::default();
    let rows = Arc::clone(&sink.rows);

    // The decision is made on THIS task, before the synthesis spawn — which is
    // both why a thread-local subscriber sees it and why the turn's own verdict
    // is not the subject (`DeterministicInference` has no `complete_stream`, so
    // the handle may be an Err).
    let guard = tracing::subscriber::set_default(sink);
    let handle = harness
        .runtime
        .handle_message_stream_as("how does the scheduler work", &conv, Intent::DeepQuery)
        .await;
    drop(guard);
    if let Ok(h) = handle {
        let _drained: Vec<_> = h.stream.collect().await;
    }

    let rows = rows.lock().unwrap();
    let decision: Vec<&String> = rows
        .iter()
        .filter(|r| r.contains("route=deep_query") && r.contains("gate_on="))
        .collect();
    assert_eq!(
        decision.len(),
        1,
        "the deep streaming arm must emit exactly one gate decision under \
         `grounding_gate`, naming the turn's route; captured {rows:?}"
    );
    let row = decision[0];
    for field in ["chunks=", "trace_chars=", "trace_labels=", "seal_trace="] {
        assert!(
            row.contains(field),
            "the decision carries its twin's fields plus the chunk count the \
             gate flag turns on; `{field}` is missing from {row}"
        );
    }
    // This harness attaches no corpus, so the pool is empty and THAT is why the
    // gate is off. Pinned rather than assumed: if the harness ever grows a
    // corpus this fails loudly instead of leaving the branch below uncovered.
    assert!(
        row.contains("chunks=0") && row.contains("gate_on=false"),
        "expected an empty-pool turn from the corpus-free harness: {row}"
    );
    assert!(
        rows.iter().any(|r| r.contains("deep gate off")),
        "an empty evidence pool is the reason the gate is off, and the arm \
         must name it on that branch: {rows:?}"
    );
}
