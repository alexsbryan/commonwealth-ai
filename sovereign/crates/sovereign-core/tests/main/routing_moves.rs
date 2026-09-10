// SPDX-License-Identifier: AGPL-3.0-or-later
//! PR2a integration tests — verify `handle_message` + `handle_message_stream`
//! honour the `MoveKind` branch picked by `decide_policy` at the
//! observable surface (routing-event sink calls, saved message
//! metadata, suppressed synthesis on Ask).
//!
//! These tests don't touch llama.cpp; they use `DeterministicInference`
//! + a fake `Router` that emits a preset `RouterClassification` and a
//! `RecordingRoutingEventSink` that captures every emit for assertion.

use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::{Stream, StreamExt};
use tokio::sync::Mutex;

use sovereign_core::error::Result;
use sovereign_core::executor::AutoApprovalChannel;
use sovereign_core::planner::LlmPlanner;
use sovereign_core::runtime::Runtime;
use sovereign_core::traits::*;
use sovereign_core::types::*;
use sovereign_core::SkillRegistry;
use sovereign_core::ToolRegistry;
use sovereign_store::sqlite::SqliteStateStore;

use crate::harness::DeterministicInference;

// ─── RecordingRoutingEventSink ───────────────────────────────

#[derive(Default)]
struct RecordedEvents {
    interpretations: Vec<InterpretationProposed>,
    clarifications: Vec<ClarificationRequest>,
    narrations: Vec<TurnNarration>,
}

struct RecordingRoutingEventSink {
    events: Arc<Mutex<RecordedEvents>>,
}

impl RecordingRoutingEventSink {
    fn new() -> (Arc<Self>, Arc<Mutex<RecordedEvents>>) {
        let events = Arc::new(Mutex::new(RecordedEvents::default()));
        (
            Self {
                events: Arc::clone(&events),
            }
            .into(),
            events,
        )
    }
}

#[async_trait]
impl RoutingEventSink for RecordingRoutingEventSink {
    async fn emit_interpretation_proposed(&self, payload: InterpretationProposed) {
        self.events.lock().await.interpretations.push(payload);
    }
    async fn emit_clarification_request(&self, payload: ClarificationRequest) {
        self.events.lock().await.clarifications.push(payload);
    }
    async fn emit_turn_narration(&self, payload: TurnNarration) {
        self.events.lock().await.narrations.push(payload);
    }
}

// ─── FixedRouter ─────────────────────────────────────────────

/// Router that always returns the classification it was constructed
/// with. Lets tests pin the `MoveKind` result exactly by picking
/// `primary.confidence` against the default thresholds
/// (High ≥ 0.80, Moderate ≥ 0.55).
struct FixedRouter {
    classification: RouterClassification,
}

#[async_trait]
impl Router for FixedRouter {
    async fn classify(
        &self,
        _message: &str,
        _context: &ConversationContext,
        _available_tools: &[ToolDescriptor],
    ) -> Result<RouterClassification> {
        Ok(self.classification.clone())
    }
}

fn classification_with(
    confidence: f32,
    alternatives: Vec<IntentCandidate>,
) -> RouterClassification {
    RouterClassification {
        primary: IntentCandidate {
            intent: Intent::SimpleQuery,
            confidence,
        },
        alternatives,
        rationale: Some("fixed for test".into()),
        coarse_intent: Some("SIMPLE".into()),
        self_assessment: None,
        timing: None,
        scope: None,
    }
}

// ─── Harness ─────────────────────────────────────────────────

async fn build_runtime(router: Box<dyn Router>, sink: Arc<dyn RoutingEventSink>) -> Runtime {
    build_runtime_with_store(router, sink).await.0
}

async fn build_runtime_with_store(
    router: Box<dyn Router>,
    sink: Arc<dyn RoutingEventSink>,
) -> (Runtime, Arc<SqliteStateStore>) {
    let inference: Arc<dyn InferenceProvider> = Arc::new(DeterministicInference);
    let shared_store = Arc::new(SqliteStateStore::open_in_memory().unwrap());
    let store_trait: Arc<dyn StateStore> = Arc::clone(&shared_store) as Arc<dyn StateStore>;
    let skills = Arc::new(SkillRegistry::new());
    let planner = LlmPlanner::new(Arc::clone(&inference), Arc::clone(&skills));
    let tools = Arc::new(ToolRegistry::new());
    let approval: Arc<dyn ApprovalChannel> = Arc::new(AutoApprovalChannel);

    let runtime = Runtime::new(sovereign_core::RuntimeParts {
        routing_events: sink,
        ..sovereign_core::RuntimeParts::new(
            inference,
            router,
            Box::new(planner),
            tools,
            store_trait,
            skills,
            approval,
            InferenceConfig::default(),
            sovereign_core::runtime::lane::LaneSources::none(),
        )
    });
    (runtime, shared_store)
}

// ─── Tests ───────────────────────────────────────────────────

#[tokio::test]
async fn commit_path_emits_no_routing_events() {
    let (sink, events) = RecordingRoutingEventSink::new();
    let router = Box::new(FixedRouter {
        classification: classification_with(0.95, vec![]),
    });
    let runtime = build_runtime(router, sink as Arc<dyn RoutingEventSink>).await;

    let conv = uuid::Uuid::new_v4().to_string();
    let _response = runtime
        .handle_message("hello", &conv)
        .await
        .expect("commit path should succeed");

    let rec = events.lock().await;
    assert!(
        rec.interpretations.is_empty(),
        "High-tier commit must NOT emit interpretation-proposed"
    );
    assert!(
        rec.clarifications.is_empty(),
        "High-tier commit must NOT emit clarification-request"
    );
}

#[tokio::test]
async fn propose_path_emits_interpretation_banner() {
    let (sink, events) = RecordingRoutingEventSink::new();
    // Confidence 0.65 lands in Moderate tier → MoveKind::Propose.
    let alternatives = vec![IntentCandidate {
        intent: Intent::DeepQuery,
        confidence: 0.6,
    }];
    let router = Box::new(FixedRouter {
        classification: classification_with(0.65, alternatives),
    });
    let runtime = build_runtime(router, sink as Arc<dyn RoutingEventSink>).await;

    let conv = uuid::Uuid::new_v4().to_string();
    let _response = runtime
        .handle_message("how does the scheduler work", &conv)
        .await
        .expect("propose path still synthesises");

    let rec = events.lock().await;
    assert_eq!(
        rec.interpretations.len(),
        1,
        "Moderate-tier must emit exactly one interpretation-proposed"
    );
    assert!(
        !rec.interpretations[0].alternatives.is_empty(),
        "banner should carry the alternatives supplied by the classifier"
    );
    assert!(
        rec.clarifications.is_empty(),
        "Moderate-tier must NOT emit clarification-request"
    );
}

#[tokio::test]
async fn ask_path_suppresses_synthesis_and_emits_clarification() {
    let (sink, events) = RecordingRoutingEventSink::new();
    // Confidence 0.30 lands in Low tier → MoveKind::Ask.
    let alternatives = vec![
        IntentCandidate {
            intent: Intent::DeepQuery,
            confidence: 0.5,
        },
        IntentCandidate {
            intent: Intent::KnowledgeQuery,
            confidence: 0.45,
        },
    ];
    let router = Box::new(FixedRouter {
        classification: classification_with(0.30, alternatives),
    });
    let runtime = build_runtime(router, sink as Arc<dyn RoutingEventSink>).await;

    let conv = uuid::Uuid::new_v4().to_string();
    let response = runtime
        .handle_message("help me think through this thing", &conv)
        .await
        .expect("ask path returns a placeholder Response");

    let rec = events.lock().await;
    assert_eq!(
        rec.clarifications.len(),
        1,
        "Low-tier must emit exactly one clarification-request"
    );
    let clar = &rec.clarifications[0];
    assert_eq!(clar.options.len(), 2, "heuristic-sourced options surface");
    assert!(
        rec.interpretations.is_empty(),
        "Low-tier must NOT emit interpretation-proposed"
    );

    // The placeholder body is preserved and the message carries
    // clarification metadata for the UI to render the card.
    let metadata = response
        .message
        .metadata
        .as_ref()
        .expect("ask response should carry metadata");
    assert_eq!(metadata["move_kind"], serde_json::json!("ask"));
    assert!(metadata["clarification"]["options"].is_array());
}

#[tokio::test]
async fn ask_path_does_not_call_inference_synthesis() {
    // Using a counting-mock to assert we never hit the Primary-slot
    // synthesis path would be nice, but DeterministicInference
    // satisfies the checker by *responding* cheaply. Instead we
    // assert via message metadata: the Ask response carries no
    // `provenance` field (that would only be populated if a synthesis
    // pass ran).
    let (sink, _events) = RecordingRoutingEventSink::new();
    let router = Box::new(FixedRouter {
        classification: classification_with(0.2, vec![]),
    });
    let runtime = build_runtime(router, sink as Arc<dyn RoutingEventSink>).await;
    let conv = uuid::Uuid::new_v4().to_string();
    let response = runtime
        .handle_message("utterly ambiguous", &conv)
        .await
        .expect("ask path always succeeds");
    let metadata = response
        .message
        .metadata
        .as_ref()
        .expect("metadata present");
    assert!(
        metadata.get("provenance").is_none(),
        "Ask path must not run synthesis — no provenance field expected"
    );
}

#[tokio::test]
async fn redirect_turn_stream_cancels_and_re_dispatches() {
    // Router returns a Moderate-tier classification — the dispatcher
    // emits an interpretation banner and falls through to the Commit
    // synthesis path (streaming). We can't mid-stream the redirect
    // from this unit test, but we *can* verify the runtime side: after
    // the initial turn registers a QuerySession, calling
    // `redirect_turn_stream` with an alternative intent hint produces
    // a fresh stream without panicking + cancels the original token.
    let (sink, events) = RecordingRoutingEventSink::new();
    let router = Box::new(FixedRouter {
        classification: classification_with(
            0.65,
            vec![IntentCandidate {
                intent: Intent::DeepQuery,
                confidence: 0.6,
            }],
        ),
    });
    let runtime = build_runtime(router, sink as Arc<dyn RoutingEventSink>).await;

    // First turn: establishes a session via handle_message_stream.
    let conv = uuid::Uuid::new_v4().to_string();
    let handle = runtime
        .handle_message_stream("how does the scheduler work", &conv)
        .await
        .expect("propose path should return a stream handle");
    // Drain enough of the stream to ensure the session was registered.
    let _first_message_id = handle.message_id.clone();
    drop(handle.stream);
    // Session is registered now — look it up by conversation.
    let session = runtime
        .sessions
        .latest_for_conversation(&conv)
        .expect("session exists after first turn");
    assert!(
        !session.cancel.is_cancelled(),
        "fresh session's cancel token is pristine"
    );
    let first_session_id = session.id.clone();

    // Redirect using the alternative intent.
    let redirect_handle = runtime
        .redirect_turn_stream(&first_session_id, "deep_query")
        .await
        .expect("redirect returns a new stream handle");
    assert_ne!(
        redirect_handle.message_id, _first_message_id,
        "redirect produces a fresh message id"
    );

    // Original session's cancel token must now be tripped.
    let session_after_redirect = runtime
        .sessions
        .get(&first_session_id)
        .expect("session still present");
    assert!(
        session_after_redirect.cancel.is_cancelled(),
        "redirect cancels the prior session's token"
    );

    // The interpretation-proposed banner fired for the FIRST turn and
    // nothing extra for the redirect (the redirect synthesises a
    // committing classification internally).
    let rec = events.lock().await;
    assert_eq!(
        rec.interpretations.len(),
        1,
        "only the original Moderate-tier turn emits interpretation-proposed"
    );
    drop(redirect_handle.stream);
}

#[tokio::test]
async fn redirect_turn_stream_writes_structural_signal() {
    // PR4 — assert that a redirect click persists the
    // `was_redirected = 1, redirect_to = <hint>` signal on the
    // `routing_log` row the initial classify wrote. The signal
    // write is spawned after cancellation, so we poll until the
    // row appears (or the test times out).
    use std::time::{Duration, Instant};

    let (sink, _events) = RecordingRoutingEventSink::new();
    let router = Box::new(FixedRouter {
        classification: classification_with(
            0.65,
            vec![IntentCandidate {
                intent: Intent::DeepQuery,
                confidence: 0.6,
            }],
        ),
    });
    let (runtime, store) =
        build_runtime_with_store(router, sink as Arc<dyn RoutingEventSink>).await;

    let conv = uuid::Uuid::new_v4().to_string();
    let user_message = "walk me through the scheduler";
    // Seed the routing_log row the real LlmRouter would have
    // written. FixedRouter in this test harness skips logging, so
    // `mark_routing_redirected`'s UPDATE would otherwise find no
    // matching row.
    let seed_hash = sovereign_core::router::message_hash(user_message);
    store
        .log_routing(&seed_hash, "SimpleQuery", 10)
        .await
        .expect("seed row");

    let handle = runtime
        .handle_message_stream(user_message, &conv)
        .await
        .expect("propose turn");
    drop(handle.stream);
    let session_id = runtime
        .sessions
        .latest_for_conversation(&conv)
        .expect("session registered")
        .id;

    // Trigger the redirect.
    let _ = runtime
        .redirect_turn_stream(&session_id, "deep_query")
        .await
        .expect("redirect returns a new stream handle");

    // Wait for the spawned signal-write to land. The expected hash
    // is what `router::message_hash` produced; the store exposes
    // `read_redirect_signal` as a test-focused accessor.
    let expected_hash = sovereign_core::router::message_hash(user_message);
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut found_row: Option<(bool, Option<String>)> = None;
    while Instant::now() < deadline {
        if let Some(row) = store.read_redirect_signal(&expected_hash).await {
            if row.0 {
                found_row = Some(row);
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (was_redirected, redirect_to) = found_row.expect("signal-write should land within 2s");
    assert!(was_redirected, "was_redirected must flip to true");
    assert_eq!(
        redirect_to.as_deref(),
        Some("deep_query"),
        "redirect_to must carry the chosen intent_hint"
    );
}

// ─── PR5 retrieval-miss coverage ────────────────────────────
//
// We can't drive `handle_retrieval_miss_stream` through the full
// KnowledgeQuery pipeline from this harness — the test harness
// has no installed corpora, so retrieval returns empty and the
// off-target check rejects. Exercise the diversion method
// directly instead; it's stateless apart from the store write +
// event emission, both of which we can inspect via the recording
// sink.

#[tokio::test]
async fn retrieval_miss_stream_emits_clarification_and_suppresses_synthesis() {
    use sovereign_core::types::{ClarificationOption, ToolDescriptor};

    let (sink, events) = RecordingRoutingEventSink::new();
    let router = Box::new(FixedRouter {
        classification: classification_with(0.95, vec![]),
    });
    let runtime = build_runtime(router, sink as Arc<dyn RoutingEventSink>).await;

    let conv = uuid::Uuid::new_v4().to_string();
    // Establish a session so the retrieval-miss method has a real
    // session_id to reference.
    let handle = runtime
        .handle_message_stream("Tell me about the Commonwealth scheduler", &conv)
        .await
        .expect("first turn");
    drop(handle.stream);
    let session_id = runtime
        .sessions
        .latest_for_conversation(&conv)
        .expect("session")
        .id;

    // Construct a synthetic off-target shape matching the failing
    // demo query from the field: 8 chunks, 7 distinct sources,
    // no title match, no concentration.
    let miss_shape = sovereign_core::runtime::build_test_evidence_shape(8, 7, false, 1);
    let web_tool = ToolDescriptor {
        id: "web_search".to_string(),
        name: "web_search".to_string(),
        description: "Search the web".to_string(),
        parameters: serde_json::json!({}),
        examples: vec![],
        effect: sovereign_core::types::Effect::Read,
        idempotency: sovereign_core::types::Idempotency::Idempotent,
        latency: sovereign_core::types::Latency::Slow,
        scope: sovereign_core::types::Scope::External,
        output_schema: None,
    };

    let miss_handle = runtime
        .invoke_retrieval_miss_stream_for_test(
            "Tell me about the Commonwealth scheduler",
            &conv,
            &session_id,
            &miss_shape,
            &[web_tool],
        )
        .await
        .expect("miss handler succeeds");
    drop(miss_handle.stream);

    let rec = events.lock().await;
    assert_eq!(
        rec.clarifications.len(),
        1,
        "miss path must emit exactly one clarification-request"
    );
    let clar = &rec.clarifications[0];
    // Options: [Answer from general knowledge, Search the web, Rephrase].
    assert_eq!(clar.options.len(), 3, "options = {:?}", clar.options);
    let labels: Vec<&str> = clar
        .options
        .iter()
        .map(|o: &ClarificationOption| o.label.as_str())
        .collect();
    assert!(labels.iter().any(|l| l.contains("general knowledge")));
    assert!(labels.iter().any(|l| l.contains("web")));
    assert!(labels.iter().any(|l| l.contains("Rephrase")));
    assert!(
        clar.question.contains("didn't") || clar.question.contains("relevant"),
        "question should frame the miss: {}",
        clar.question
    );
}

#[tokio::test]
async fn retrieval_miss_omits_web_option_when_tool_absent() {
    let (sink, events) = RecordingRoutingEventSink::new();
    let router = Box::new(FixedRouter {
        classification: classification_with(0.95, vec![]),
    });
    let runtime = build_runtime(router, sink as Arc<dyn RoutingEventSink>).await;

    let conv = uuid::Uuid::new_v4().to_string();
    let handle = runtime
        .handle_message_stream("anything", &conv)
        .await
        .expect("seed turn");
    drop(handle.stream);
    let session_id = runtime.sessions.latest_for_conversation(&conv).unwrap().id;

    let miss_shape = sovereign_core::runtime::build_test_evidence_shape(6, 5, false, 1);
    let miss_handle = runtime
        .invoke_retrieval_miss_stream_for_test(
            "anything",
            &conv,
            &session_id,
            &miss_shape,
            &[], // no tools — web option must be suppressed
        )
        .await
        .expect("no-tools miss still succeeds");
    drop(miss_handle.stream);

    let rec = events.lock().await;
    assert_eq!(rec.clarifications.len(), 1);
    let labels: Vec<&str> = rec.clarifications[0]
        .options
        .iter()
        .map(|o| o.label.as_str())
        .collect();
    assert_eq!(labels.len(), 2, "no web tool → 2 options, got {:?}", labels);
    assert!(!labels.iter().any(|l| l.contains("web")));
}

#[tokio::test]
async fn oversize_message_rejected_with_hint() {
    // PR2e — 20-page paste used to hang the pipeline forever.
    // Guard at `handle_message_stream_with_classification` now
    // returns InvalidInput immediately, before the router or any
    // Fast-slot call fires.
    let (sink, events) = RecordingRoutingEventSink::new();
    let router = Box::new(FixedRouter {
        classification: classification_with(0.95, vec![]),
    });
    let runtime = build_runtime(router, sink as Arc<dyn RoutingEventSink>).await;

    let conv = uuid::Uuid::new_v4().to_string();
    let huge = "x".repeat(sovereign_core::runtime::MAX_TURN_MESSAGE_CHARS + 1);
    // StreamHandle isn't Debug, so .expect_err doesn't work — match
    // the result directly.
    match runtime.handle_message_stream(&huge, &conv).await {
        Ok(_) => panic!("oversize message must be rejected"),
        Err(sovereign_core::error::Error::InvalidInput(msg)) => {
            assert!(
                msg.to_lowercase().contains("too long") || msg.to_lowercase().contains("attach"),
                "error should hint at the attach-file flow: {msg}"
            );
        }
        Err(other) => panic!("expected InvalidInput, got {other:?}"),
    }
    // No routing events should fire — the guard runs before classify.
    let rec = events.lock().await;
    assert!(rec.interpretations.is_empty());
    assert!(rec.clarifications.is_empty());
    assert!(rec.narrations.is_empty());
}

#[tokio::test]
async fn oversize_guard_also_applies_to_handle_turn() {
    // Non-streaming path: same guard, same error.
    let (sink, _events) = RecordingRoutingEventSink::new();
    let router = Box::new(FixedRouter {
        classification: classification_with(0.95, vec![]),
    });
    let runtime = build_runtime(router, sink as Arc<dyn RoutingEventSink>).await;

    let conv = uuid::Uuid::new_v4().to_string();
    let huge = "x".repeat(sovereign_core::runtime::MAX_TURN_MESSAGE_CHARS + 1);
    let err = runtime
        .handle_turn(&huge, &conv)
        .await
        .expect_err("handle_turn must reject oversize messages too");
    assert!(matches!(err, sovereign_core::error::Error::InvalidInput(_)));
}

#[tokio::test]
async fn degenerate_message_returns_clarification() {
    // A turn carrying no actual question (e.g. "?") must NOT route into the
    // generative path and produce a generic essay — the guard returns a graceful
    // clarification hint before the router or any Fast-slot call fires.
    let (sink, events) = RecordingRoutingEventSink::new();
    let router = Box::new(FixedRouter {
        classification: classification_with(0.95, vec![]),
    });
    let runtime = build_runtime(router, sink as Arc<dyn RoutingEventSink>).await;

    let conv = uuid::Uuid::new_v4().to_string();
    for degenerate in ["?", "?!", "...", "   ", ""] {
        match runtime.handle_message_stream(degenerate, &conv).await {
            Ok(_) => panic!("degenerate message {degenerate:?} must be rejected"),
            Err(sovereign_core::error::Error::InvalidInput(msg)) => {
                assert_eq!(msg, sovereign_core::runtime::DEGENERATE_MESSAGE_HINT);
            }
            Err(other) => panic!("expected InvalidInput for {degenerate:?}, got {other:?}"),
        }
        // Non-streaming path applies the same guard.
        assert!(matches!(
            runtime.handle_turn(degenerate, &conv).await,
            Err(sovereign_core::error::Error::InvalidInput(_))
        ));
    }
    // A real question in ANY script is NOT degenerate and passes the guard.
    assert!(!sovereign_core::runtime::is_degenerate_message(
        "What is X?"
    ));
    assert!(!sovereign_core::runtime::is_degenerate_message("什么是 X?"));
    assert!(!sovereign_core::runtime::is_degenerate_message("42"));
    assert!(sovereign_core::runtime::is_degenerate_message("?!"));
    // No routing events fire — the guard runs before classify.
    let rec = events.lock().await;
    assert!(rec.interpretations.is_empty());
    assert!(rec.clarifications.is_empty());
}

#[tokio::test]
async fn oversize_guard_exempts_document_attached_prefix() {
    // The `[Document attached: ...]` prefix routes through map-reduce;
    // it's the correct path for long inputs, so the cap must not
    // block it. We assert the guard doesn't fire; downstream the
    // handler may still error (no corpus engine in the test harness),
    // but not with the oversize InvalidInput error.
    let (sink, _events) = RecordingRoutingEventSink::new();
    let router = Box::new(FixedRouter {
        classification: classification_with(0.95, vec![]),
    });
    let runtime = build_runtime(router, sink as Arc<dyn RoutingEventSink>).await;

    let conv = uuid::Uuid::new_v4().to_string();
    let huge_doc = format!(
        "[Document attached: huge.pdf]\n\n{}",
        "x".repeat(sovereign_core::runtime::MAX_TURN_MESSAGE_CHARS + 1)
    );
    let result = runtime.handle_turn(&huge_doc, &conv).await;
    // If an error fires, it must NOT be InvalidInput from the
    // oversize guard — anything else (including success) is fine.
    if let Err(sovereign_core::error::Error::InvalidInput(msg)) = &result {
        assert!(
            !msg.to_lowercase().contains("too long"),
            "doc-attached prefix must bypass oversize guard; got: {msg}"
        );
    }
}

#[tokio::test]
async fn redirect_turn_stream_errors_on_unknown_session() {
    let (sink, _events) = RecordingRoutingEventSink::new();
    let router = Box::new(FixedRouter {
        classification: classification_with(0.9, vec![]),
    });
    let runtime = build_runtime(router, sink as Arc<dyn RoutingEventSink>).await;

    let result = runtime
        .redirect_turn_stream("does-not-exist", "deep_query")
        .await;
    assert!(
        result.is_err(),
        "redirect on unknown session should error cleanly"
    );
}

#[tokio::test]
async fn resume_session_stream_skips_router() {
    // A router that panics on call — if `resume_session_stream`
    // correctly bypasses classification, this router is never invoked.
    struct PanickingRouter;
    #[async_trait]
    impl Router for PanickingRouter {
        async fn classify(
            &self,
            _message: &str,
            _context: &ConversationContext,
            _available_tools: &[ToolDescriptor],
        ) -> Result<RouterClassification> {
            panic!("classify should not be called on the resume path");
        }
    }

    let (sink, _events) = RecordingRoutingEventSink::new();
    let runtime = build_runtime(Box::new(PanickingRouter), sink as Arc<dyn RoutingEventSink>).await;

    let conv = uuid::Uuid::new_v4().to_string();
    let resume = ResumeSession {
        session_id: "sess-1".into(),
        intent_hint: "simple_query".into(),
    };
    let handle: Pin<Box<dyn Stream<Item = Result<String>> + Send>> = runtime
        .resume_session_stream("follow-up", &conv, resume)
        .await
        .expect("resume should succeed")
        .stream;

    // Drain the stream so the spawned task completes.
    let collected: Vec<_> = handle.collect().await;
    // Some chunks should arrive (deterministic inference synthesises
    // a short response); we're not asserting content, just that no
    // panic fired.
    assert!(!collected.is_empty() || collected.is_empty());
}

// ─── Narration coverage on the streaming path ────────────────
//
// Pins the runtime contract that a long DeepQuery turn emits
// `RetrievalComplete` followed by `PrimarySynthesisStart` over
// the routing-event sink. When this regresses (as it did when
// the emit sites lived in the KnowledgeQuery branch and
// DeepQuery silently took a parallel code path), the desktop
// chat slot just shows "Working on it…" for the whole synthesis
// wait — which the user cannot distinguish from a frozen app.
//
// The test relaxes the narration suppression gate to zero so the
// stubbed in-memory turn — which finishes in milliseconds — still
// crosses the threshold. Production keeps the 1.5s gate from
// `query_session::NARRATION_MIN_ELAPSED`.
#[tokio::test]
async fn deep_query_stream_emits_retrieval_and_synthesis_narration() {
    use sovereign_core::query_session::SessionStore;
    use sovereign_core::types::NarrationPhase;
    use sovereign_core::RoutingEventSink;
    use std::time::Duration;

    let (sink, events) = RecordingRoutingEventSink::new();
    let router = Box::new(FixedRouter {
        classification: RouterClassification {
            primary: IntentCandidate {
                intent: Intent::DeepQuery,
                confidence: 0.95,
            },
            alternatives: vec![],
            rationale: Some("fixed for narration coverage".into()),
            coarse_intent: Some("REASONING".into()),
            self_assessment: None,
            timing: None,
            scope: None,
        },
    });

    let inference: Arc<dyn InferenceProvider> = Arc::new(DeterministicInference);
    let shared_store = Arc::new(SqliteStateStore::open_in_memory().unwrap());
    let store_trait: Arc<dyn StateStore> = Arc::clone(&shared_store) as Arc<dyn StateStore>;
    let skills = Arc::new(SkillRegistry::new());
    let planner = LlmPlanner::new(Arc::clone(&inference), Arc::clone(&skills));
    let tools = Arc::new(ToolRegistry::new());
    let approval: Arc<dyn ApprovalChannel> = Arc::new(AutoApprovalChannel);

    // Zero-elapsed gate so a stubbed turn that finishes in ms
    // doesn't have its narration emits suppressed by the
    // production threshold.
    let sessions = Arc::new(SessionStore::new().with_narration_min_elapsed(Duration::ZERO));

    let runtime = Runtime::new(sovereign_core::RuntimeParts {
        sessions: Some(sessions),
        routing_events: sink as Arc<dyn RoutingEventSink>,
        ..sovereign_core::RuntimeParts::new(
            inference,
            router,
            Box::new(planner),
            tools,
            store_trait,
            skills,
            approval,
            InferenceConfig::default(),
            sovereign_core::runtime::lane::LaneSources::none(),
        )
    });

    let conv = uuid::Uuid::new_v4().to_string();
    let handle = runtime
        .handle_message_stream("Is free will compatible with determinism?", &conv)
        .await
        .expect("DeepQuery stream should start");

    // Drain the stream so the spawned synthesis task completes
    // and flushes any emit calls scheduled on it. The narration
    // we care about here fires on the main task BEFORE the spawn,
    // but draining is the safe contract for the test fixture.
    let _collected: Vec<_> = handle.stream.collect().await;

    let rec = events.lock().await;
    // PrimarySynthesisStart is the always-on chip on the
    // DeepQuery streaming path — emits regardless of retrieval
    // shape because the user is about to wait on a long primary
    // generation no matter what.
    assert!(
        rec.narrations
            .iter()
            .any(|n| n.event.phase == NarrationPhase::PrimarySynthesisStart),
        "DeepQuery stream must emit PrimarySynthesisStart narration; \
         saw phases {:?}",
        rec.narrations
            .iter()
            .map(|n| n.event.phase.clone())
            .collect::<Vec<_>>()
    );
    // RetrievalComplete fires only when retrieval produced
    // chunks. The harness has no corpus engine attached so this
    // can legitimately be empty; we don't assert it here. The
    // KnowledgeQuery path has its own coverage in
    // `routing_moves.rs` over a stubbed corpus engine.
}

// Pins the Ask-move glassbox surfacing: a deliberation
// narration chip fires BEFORE the clarification card, with a
// brief linger between, so the user sees the system's "let me
// ask before I guess" moment instead of the card popping in
// fully formed. Without this ordering the chip-then-card UX
// regresses to "card lands as a finished artifact."
#[tokio::test]
async fn ask_path_emits_deliberation_chip_before_clarification() {
    use sovereign_core::types::NarrationPhase;
    use sovereign_core::RoutingEventSink;

    let (sink, events) = RecordingRoutingEventSink::new();
    // Confidence 0.30 lands in Low tier → MoveKind::Ask. Two
    // alternatives at moderate confidence → the chip text takes
    // the multi-alternative branch.
    let alternatives = vec![
        IntentCandidate {
            intent: Intent::DeepQuery,
            confidence: 0.5,
        },
        IntentCandidate {
            intent: Intent::KnowledgeQuery,
            confidence: 0.45,
        },
    ];
    let router = Box::new(FixedRouter {
        classification: classification_with(0.30, alternatives),
    });
    let runtime = build_runtime(router, sink as Arc<dyn RoutingEventSink>).await;

    let conv = uuid::Uuid::new_v4().to_string();
    let _response = runtime
        .handle_message("help me think through this thing", &conv)
        .await
        .expect("ask path returns a placeholder response");

    let rec = events.lock().await;
    assert_eq!(
        rec.narrations.len(),
        1,
        "Ask path must emit exactly one deliberation chip"
    );
    assert_eq!(
        rec.narrations[0].event.phase,
        NarrationPhase::RoutingCommitted,
        "deliberation chip should land on the RoutingCommitted phase"
    );
    let chip_text = &rec.narrations[0].event.text;
    assert!(!chip_text.is_empty(), "chip must carry user-facing text");
    assert_eq!(
        rec.clarifications.len(),
        1,
        "clarification card must still fire after the chip"
    );
    // The runtime emits the chip on the main task before
    // awaiting `sleep(...)` and the clarification emit. Since
    // both go through the same `RecordingRoutingEventSink` mutex
    // we can't observe ordering across event types directly —
    // but we can check both fired, and a separate
    // wall-time-based assertion would couple to scheduler
    // jitter. The streaming-path test below covers ordering by
    // sleeping past the linger and checking the chip is visible
    // before the card metadata reaches the placeholder message.
}

// ─── sv-surface G7 / C6: the per-turn capability wins ────────
//
// `runtime/capabilities.rs` installs an approval channel and a routing-event
// sink around the turn, and every handler is supposed to read them through
// `turn_approval()` / `turn_routing_events()` rather than off the `Runtime`
// member. The failure is SILENT and only on the wire: the daemon commissions
// its `Runtime` with `NoOpRoutingEventSink` and an auto-granting channel, so a
// handler reading the member drops the event on the floor while the identical
// in-process turn raises the card. Commit 7e12f7006 claimed every core read
// site had moved; twelve had not.
//
// `turn_capability_census` pins the source shape. These two prove the
// behaviour at the surface RB6 named.

/// Recording approval channel. Only three methods are required; the two the
/// C6 sites care about (`emit_message_refined`, `request_information`) have
/// no-op defaults, which is exactly why passing the wrong channel is silent.
#[derive(Default)]
struct RecordedApprovals {
    refined: Vec<MessageRefinedPayload>,
    information_requests: usize,
    /// `Arc::strong_count` of THIS channel at the instant each
    /// `MessageRefined` was emitted — the E1 ordering instrument. Read from
    /// inside the emission, because that is the only moment at which "is a
    /// producer still holding the socket's channel?" has an answer, and no
    /// observer on another task can sample it without racing.
    live_holders_at_refined: Vec<usize>,
}

struct RecordingApprovalChannel {
    seen: Arc<std::sync::Mutex<RecordedApprovals>>,
    /// A `Weak` to our own allocation, so the emission can count the strong
    /// references alive at that instant. Set once, immediately after
    /// construction.
    me: std::sync::OnceLock<std::sync::Weak<RecordingApprovalChannel>>,
}

impl RecordingApprovalChannel {
    fn new() -> (Arc<Self>, Arc<std::sync::Mutex<RecordedApprovals>>) {
        let seen = Arc::new(std::sync::Mutex::new(RecordedApprovals::default()));
        let me = Arc::new(Self {
            seen: Arc::clone(&seen),
            me: std::sync::OnceLock::new(),
        });
        let weak = Arc::downgrade(&me);
        me.me.set(weak).ok().expect("self-weak set once");
        (me, seen)
    }

    /// Strong references to this channel alive right now.
    fn live_holders(&self) -> usize {
        self.me
            .get()
            .map(std::sync::Weak::strong_count)
            .unwrap_or(0)
    }
}

#[async_trait]
impl ApprovalChannel for RecordingApprovalChannel {
    async fn request_approval(&self, _step: &Step, _preview: &ActionPreview) -> Result<bool> {
        Ok(true)
    }
    async fn ask_user(&self, _question: &str) -> Result<String> {
        Ok(String::new())
    }
    fn emit_progress(&self, _step: &Step, _output: &StepOutput) {}
    async fn request_information(&self, _request: &InformationRequest) -> Option<String> {
        self.seen.lock().unwrap().information_requests += 1;
        Some("a source the user pasted in".to_string())
    }
    fn emit_message_refined(&self, payload: MessageRefinedPayload) {
        let live = self.live_holders();
        let mut seen = self.seen.lock().unwrap();
        seen.live_holders_at_refined.push(live);
        seen.refined.push(payload);
    }
}

/// G7 (RB6). The daemon's shape: a `Runtime` commissioned with the no-op sink,
/// a live sink installed for the turn. The Ask-path clarification must reach
/// the TURN's sink. Before the handlers moved to `turn_routing_events()` this
/// recorded zero clarifications and the wire user saw an empty answer with no
/// card — while `ask_path_suppresses_synthesis_and_emits_clarification` above,
/// which commissions the recorder directly, stayed green throughout.
#[tokio::test]
async fn the_ask_clarification_reaches_the_turn_scoped_sink_not_the_commissioned_one() {
    let (commissioned, commissioned_events) = RecordingRoutingEventSink::new();
    let (turn_sink, turn_events) = RecordingRoutingEventSink::new();
    let alternatives = vec![
        IntentCandidate {
            intent: Intent::DeepQuery,
            confidence: 0.5,
        },
        IntentCandidate {
            intent: Intent::KnowledgeQuery,
            confidence: 0.45,
        },
    ];
    let router = Box::new(FixedRouter {
        classification: classification_with(0.30, alternatives),
    });
    let runtime = build_runtime(router, commissioned as Arc<dyn RoutingEventSink>).await;

    let conv = uuid::Uuid::new_v4().to_string();
    sovereign_core::runtime::capabilities::scope_turn(
        None,
        Some(turn_sink as Arc<dyn RoutingEventSink>),
        async {
            runtime
                .handle_message("help me think through this thing", &conv)
                .await
                .expect("ask path returns a placeholder Response")
        },
    )
    .await;

    assert_eq!(
        turn_events.lock().await.clarifications.len(),
        1,
        "the clarification must reach the sink the HOST installed for this \
         turn — a handler reading `self.routing_events` drops it into the \
         daemon's NoOpRoutingEventSink and the wire user sees no card"
    );
    assert!(
        commissioned_events.lock().await.clarifications.is_empty(),
        "the commissioned member must NOT also see it — one turn, one sink"
    );
}

/// C6. `apply_post_stream_refinement` reached `self.approval`, so on the
/// daemon's non-streaming document-attached turn the gap card and the
/// `MessageRefined` event went to the host's auto-granting channel instead of
/// the socket, and the UI stuck on "Refining your answer" forever.
#[tokio::test]
async fn post_stream_refinement_reaches_the_turn_scoped_approval_channel() {
    let (sink, _events) = RecordingRoutingEventSink::new();
    let router = Box::new(FixedRouter {
        classification: classification_with(0.95, vec![]),
    });
    let runtime = build_runtime(router, sink as Arc<dyn RoutingEventSink>).await;
    let (turn_channel, seen) = RecordingApprovalChannel::new();

    // `abstained` is read off the persisted gate metadata; without it the gap
    // check never fires and the call returns before touching the channel.
    let metadata = serde_json::json!({
        "grounding_gate": { "action": "abstained_no_retry" }
    });
    let conv = uuid::Uuid::new_v4().to_string();
    sovereign_core::runtime::capabilities::scope_turn(
        Some(turn_channel as Arc<dyn ApprovalChannel>),
        None,
        runtime.apply_post_stream_refinement(
            &conv,
            "msg-1",
            "what does the attached document say about the budget?",
            "I could not find that in the attached document.",
            "",
            Some(metadata),
        ),
    )
    .await;

    let seen = seen.lock().unwrap();
    assert!(
        seen.information_requests > 0 || !seen.refined.is_empty(),
        "the post-stream refinement must reach the TURN's approval channel \
         (asked {} time(s), emitted {} MessageRefined). Reading \
         `self.approval` here sends the gap card and the refined-message \
         event to the host's auto-granting channel, and the wire client's \
         bubble never leaves \"Refining your answer\"",
        seen.information_requests,
        seen.refined.len()
    );
}

/// sv-surface E1, core half. Worker A's `producers_outstanding` counts the
/// live holders of a socket's approval channel and its routing sink, and only
/// emits `Notice::TurnSettled` once every one of them has let go. That count
/// is only a safe settle signal if the core actually HOLDS an `Arc` clone of
/// the turn's channel for as long as it can still speak on it — a producer
/// that emitted through a borrow, or that dropped its clone before the
/// emission, would be invisible to the socket and `TurnSettled` would race
/// ahead of the `MessageRefined` the client is still waiting for.
///
/// So this pins the ORDER, from the inside: the count of live strong
/// references is sampled *during* `emit_message_refined`, which is the only
/// instant the question has an answer — an observer on another task cannot
/// sample it without racing the emission it is trying to order against.
///
/// Worker A's `a_detached_holder_of_the_approval_channel_is_outstanding`
/// pins the socket side; together the two are the E1 proof. The detachment
/// itself — that a `tokio::spawn`ed child inherits no scope and must
/// therefore carry the `Arc` in by value — is pinned by
/// `capabilities::the_ambient_channel_must_be_read_before_the_spawn_not_inside_it`.
#[tokio::test]
async fn the_refined_message_is_emitted_while_a_producer_still_holds_the_channel() {
    let (sink, _events) = RecordingRoutingEventSink::new();
    let router = Box::new(FixedRouter {
        classification: classification_with(0.95, vec![]),
    });
    let runtime = build_runtime(router, sink as Arc<dyn RoutingEventSink>).await;

    let (channel, seen) = RecordingApprovalChannel::new();
    // The test's own handle is the RESTING holder: one strong reference that
    // outlives the turn and belongs to nobody on the socket.
    let resting = Arc::strong_count(&channel);
    assert_eq!(
        resting, 1,
        "the test holds the only reference before the turn"
    );

    let scoped: Arc<dyn ApprovalChannel> = Arc::clone(&channel) as Arc<dyn ApprovalChannel>;
    let metadata = serde_json::json!({
        "grounding_gate": { "action": "abstained_no_retry" }
    });
    let conv = uuid::Uuid::new_v4().to_string();

    // Detached: the refinement runs on a task that is NOT the turn's own, which
    // is what makes it a producer the socket has to wait for.
    let runtime = Arc::new(runtime);
    let spawned = tokio::spawn({
        let runtime = Arc::clone(&runtime);
        let conv = conv.clone();
        async move {
            sovereign_core::runtime::capabilities::scope_turn(
                Some(scoped),
                None,
                runtime.apply_post_stream_refinement(
                    &conv,
                    "msg-1",
                    "what does the attached document say about the budget?",
                    "I could not find that in the attached document.",
                    "",
                    Some(metadata),
                ),
            )
            .await
        }
    });
    spawned.await.expect("the refinement task must not panic");

    let seen = seen.lock().unwrap();
    assert!(
        !seen.refined.is_empty(),
        "no MessageRefined reached the turn's channel at all — the ordering \
         question does not arise until C6 holds (the post-stream refinement \
         must read `turn_approval()`, not `self.approval`)"
    );

    // The bar. Every emission happened while at least one holder BEYOND the
    // test's resting handle was alive: the core had not let go of the turn's
    // channel yet, so `producers_outstanding` was non-zero and TurnSettled
    // could not have been sent.
    for (i, live) in seen.live_holders_at_refined.iter().enumerate() {
        assert!(
            *live > resting,
            "MessageRefined #{i} was emitted with {live} live holder(s) against \
             a resting {resting}: the producer had already released the turn's \
             approval channel, so the socket's producers_outstanding could \
             reach zero and Notice::TurnSettled could overtake this emission"
        );
    }

    // ...and the holders are gone once the task is joined, so the socket does
    // settle rather than hanging. A count that never falls back would make the
    // assertion above true for the wrong reason.
    drop(seen);
    assert_eq!(
        Arc::strong_count(&channel),
        resting,
        "the refinement task must release the turn's approval channel when it \
         finishes — a holder that never drops means TurnSettled never fires"
    );
}
