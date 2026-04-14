mod harness;

use harness::{GapScript, InfoResponseScript, RefineScript, TestHarness};
use sovereign_core::skills::parse_skill_toml;
use sovereign_core::traits::*;
use sovereign_core::types::*;

// ─── Knowledge Base Search Integration ───────────────────────

#[tokio::test]
async fn deep_query_searches_local_knowledge() {
    let h = TestHarness::new();
    h.ingest_test_corpus(
        "sep",
        vec![
            (
                "epistemology",
                "Epistemology is the study of knowledge and justified belief.",
            ),
            (
                "bergson",
                "Henri Bergson wrote an essay on laughter examining the mechanical encrusted on the living.",
            ),
        ],
    )
    .await;

    let resp = h.send("What did Bergson write about humor?").await;
    let prov = h.provenance(&resp);

    // The system searched local knowledge
    assert!(
        prov.search_method.is_some(),
        "search_method should be set when corpora are installed"
    );
    // Found results from the SEP corpus
    assert!(
        prov.sources.iter().any(|s| s.origin == "sep" && s.count > 0),
        "Should find SEP chunks for Bergson query. Sources: {:?}",
        prov.sources
    );
}

#[tokio::test]
async fn query_with_no_corpus_reports_no_sources() {
    let h = TestHarness::new();
    // No corpus ingested

    let resp = h.send("What is epistemology?").await;
    let prov = h.provenance(&resp);

    // No sources found (no corpora installed)
    assert!(
        prov.sources.is_empty() || prov.sources.iter().all(|s| s.count == 0),
        "Should have no source results without corpora. Sources: {:?}",
        prov.sources
    );
}

#[tokio::test]
async fn corpus_installed_but_no_match_reports_zero_count() {
    let h = TestHarness::new();
    h.ingest_test_corpus(
        "wiki",
        vec![(
            "rust-lang",
            "Rust is a systems programming language focused on safety.",
        )],
    )
    .await;

    // Query about something not in the corpus
    let resp = h.send("Tell me about medieval architecture").await;
    let prov = h.provenance(&resp);

    // Search was attempted
    assert!(prov.search_method.is_some());
    // Wiki corpus was searched but found nothing relevant
    let wiki_source = prov.sources.iter().find(|s| s.origin == "wiki");
    assert!(
        wiki_source.map_or(true, |s| s.count == 0),
        "Wiki should have 0 matches for unrelated query"
    );
}

#[tokio::test]
async fn fts5_handles_natural_language_with_punctuation() {
    let h = TestHarness::new();
    h.ingest_test_corpus(
        "wiki",
        vec![(
            "rust-lang",
            "Rust is a systems programming language focused on safety and performance.",
        )],
    )
    .await;

    // Natural language with apostrophes and question marks
    let resp = h.send("What's Rust? Is it good for systems programming?").await;
    let prov = h.provenance(&resp);

    assert!(
        prov.sources.iter().any(|s| s.origin == "wiki" && s.count > 0),
        "FTS5 should match despite punctuation in query. Sources: {:?}",
        prov.sources
    );
}

// ─── Provenance Completeness ─────────────────────────────────

#[tokio::test]
async fn every_response_has_provenance() {
    let h = TestHarness::new();
    let resp = h.send("Hello, how are you?").await;
    let prov = h.provenance(&resp);

    assert!(!prov.intent.is_empty(), "Intent should be set");
    assert!(
        !prov.inference_backend.is_empty(),
        "Inference backend should be recorded"
    );
    assert_eq!(
        prov.inference_backend, "deterministic",
        "Should use DeterministicInference"
    );
}

#[tokio::test]
async fn provenance_persisted_in_store() {
    let h = TestHarness::new();
    let _resp = h.send_in("test message", "c1").await;

    // Retrieve from store and verify provenance survived round-trip
    let conv = h.store.get_conversation("c1").await.unwrap();
    let assistant_msg = conv
        .messages
        .iter()
        .find(|m| m.role == Role::Assistant)
        .expect("Should have an assistant message");
    let metadata = assistant_msg
        .metadata
        .as_ref()
        .expect("Assistant message should have metadata");
    assert!(
        metadata.get("provenance").is_some(),
        "Provenance should be persisted in message metadata"
    );
}

#[tokio::test]
async fn provenance_records_token_count() {
    let h = TestHarness::new();
    let resp = h.send("Simple question").await;
    let prov = h.provenance(&resp);

    assert!(
        prov.tokens_used > 0,
        "Token count should be recorded in provenance"
    );
}

// ─── Multi-Turn Conversation State ───────────────────────────

#[tokio::test]
async fn multi_turn_preserves_messages() {
    let h = TestHarness::new();
    h.send_in("First message", "c1").await;
    h.send_in("Second message", "c1").await;
    h.send_in("Third message", "c1").await;

    // 3 user messages + 3 assistant responses = 6
    assert_eq!(h.conversation_length("c1").await, 6);
}

#[tokio::test]
async fn separate_conversations_isolated() {
    let h = TestHarness::new();
    h.send_in("Topic A", "c1").await;
    h.send_in("Topic B", "c2").await;

    assert_eq!(h.conversation_length("c1").await, 2);
    assert_eq!(h.conversation_length("c2").await, 2);
}

#[tokio::test]
async fn response_is_always_nonempty() {
    let h = TestHarness::new();
    let resp = h.send("Any question at all").await;
    assert!(
        !resp.message.content.is_empty(),
        "Response should never be empty"
    );
}

// ─── Soft Delete / Sync Readiness ────────────────────────────

#[tokio::test]
async fn deleted_conversation_excluded_from_list() {
    let h = TestHarness::new();
    h.send_in("test", "c1").await;
    h.send_in("test", "c2").await;

    h.store.delete_conversation("c1").await.unwrap();

    let convos = h.store.list_conversations(10, 0).await.unwrap();
    assert_eq!(convos.len(), 1);
    assert_eq!(convos[0].id, "c2");
}

#[tokio::test]
async fn deleted_memory_excluded_from_retrieval() {
    let h = TestHarness::new();
    let mem = Memory {
        id: "m1".to_string(),
        content: "test fact about Rust programming".to_string(),
        source: "test".to_string(),
        confidence: 1.0,
        created_at: 0,
        last_used: 0,
        version: 0,
        deleted_at: None,
    };
    h.store.save_memory(&mem).await.unwrap();
    assert_eq!(h.store.get_all_memories().await.unwrap().len(), 1);

    h.store.delete_memory("m1").await.unwrap();
    assert!(h.store.get_all_memories().await.unwrap().is_empty());
}

#[tokio::test]
async fn version_field_set_on_response() {
    let h = TestHarness::new();
    let resp = h.send_in("test", "c1").await;

    // The runtime sets version = now() when creating the message.
    assert!(
        resp.message.version > 0,
        "Response message version should be set (got {})",
        resp.message.version
    );
}

// ─── Trust Levels ────────────────────────────────────────────

#[test]
fn unsigned_skill_gets_unsigned_trust() {
    let skill = parse_skill_toml(
        r#"
        [skill]
        id = "test"
        name = "Test"
        version = "0.1.0"
    "#,
    )
    .unwrap();
    assert_eq!(skill.trust_level, TrustLevel::Unsigned);
}

#[test]
fn signed_skill_gets_author_signed_trust() {
    let skill = parse_skill_toml(
        r#"
        [skill]
        id = "test"
        name = "Test"
        version = "0.1.0"
        signature = "abc123"
        signed_by = "jane@example.com"
    "#,
    )
    .unwrap();
    assert_eq!(skill.trust_level, TrustLevel::AuthorSigned);
}

#[test]
fn community_signed_skill_gets_community_trust() {
    let skill = parse_skill_toml(
        r#"
        [skill]
        id = "test"
        name = "Test"
        version = "0.1.0"
        signature = "abc123"
        signed_by = "sovereign-community"
    "#,
    )
    .unwrap();
    assert_eq!(skill.trust_level, TrustLevel::CommunityReviewed);
}

// ─── Corpus Ingestion ────────────────────────────────────────

#[tokio::test]
async fn ingested_corpus_searchable_via_fts5() {
    let h = TestHarness::new();
    h.ingest_test_corpus(
        "test",
        vec![
            ("doc1", "Quantum mechanics describes the behavior of particles at atomic scales."),
            ("doc2", "Classical mechanics was developed by Newton and Lagrange."),
        ],
    )
    .await;

    // Search via the store directly
    let results = h
        .store
        .search_documents(&[], "quantum particles", 5)
        .await
        .unwrap();

    assert!(
        !results.is_empty(),
        "FTS5 should find 'quantum particles' in ingested corpus"
    );
    assert!(results[0].content.contains("Quantum"));
}

#[tokio::test]
async fn corpus_state_tracks_chunk_count() {
    let h = TestHarness::new();
    h.ingest_test_corpus(
        "wiki",
        vec![
            ("a", "Article A content"),
            ("b", "Article B content"),
            ("c", "Article C content"),
        ],
    )
    .await;

    let state = h.store.get_corpus_state("wiki").await.unwrap();
    assert_eq!(state.chunks_count, 3);
}

// ─── ReasonWithTools ─────────────────────────────────────────

// ─── Layered Confidence ─────────────────────────────────────

#[tokio::test]
async fn layered_confidence_no_unverified_tags() {
    let h = TestHarness::new();
    h.ingest_test_corpus(
        "physics",
        vec![(
            "schrodinger",
            "Schrödinger proposed the cat thought experiment in 1935 to illustrate quantum superposition.",
        )],
    )
    .await;

    let resp = h.send("What is quantum superposition and how does it relate to consciousness?").await;

    // The response should not contain [unverified] tags — the layered
    // confidence system should present general knowledge naturally.
    assert!(
        !resp.message.content.contains("[unverified]"),
        "Response should not contain [unverified] tags. Got: {}",
        resp.message.content
    );

    // Should not refuse to answer.
    assert!(
        !resp.message.content.to_lowercase().contains("i cannot find"),
        "Should not refuse to answer. Got: {}",
        resp.message.content
    );
    assert!(
        !resp.message.content.to_lowercase().contains("i cannot provide"),
        "Should not refuse to answer. Got: {}",
        resp.message.content
    );
}

#[tokio::test]
async fn empty_corpus_produces_response_not_refusal() {
    let h = TestHarness::new();
    h.ingest_test_corpus("empty", vec![("stub", "Unrelated stub content about cooking recipes.")]).await;

    // Ask about something not in the corpus at all.
    let resp = h.send("What are the core differences between Theravada and Zen Buddhism?").await;

    // Should produce a response, not an empty string.
    assert!(
        !resp.message.content.is_empty(),
        "Response should not be empty for general knowledge question"
    );

    // Should not contain [unverified] tags.
    assert!(
        !resp.message.content.contains("[unverified]"),
        "Empty-corpus response should not use [unverified]. Got: {}",
        resp.message.content
    );
}

// ─── Conversation Topic Context ─────────────────────────────

#[tokio::test]
async fn topic_context_tracks_across_turns() {
    let h = TestHarness::new();
    let conv_id = "topic-test";

    // Turn 1: establish a topic.
    let r1 = h.send_in("Tell me about Schrödinger's cat experiment", conv_id).await;
    assert!(!r1.message.content.is_empty());

    // Turn 2: follow up in the same domain.
    let r2 = h.send_in("How does this relate to quantum decoherence?", conv_id).await;
    assert!(!r2.message.content.is_empty());

    // Turn 3: a third turn.
    let r3 = h.send_in("What about the many-worlds interpretation?", conv_id).await;
    assert!(!r3.message.content.is_empty());

    // All three turns should have produced responses.
    assert_eq!(h.conversation_length(conv_id).await, 6); // 3 user + 3 assistant
}

// ─── Thinking Block Filter (TypeScript unit test as Rust doc) ─

// Note: The administrative thinking filter is tested via the TypeScript
// test suite for parse-message.ts. The following documents the expected
// behavior for integration verification:
//
// Input think block:
//   **Source Analysis:**
//   [saantarak-sita] — no substantive content on consciousness
//   [vasubandhu] — Methodological information only
//   Critical Problem: I cannot fabricate detailed Buddhist positions
//
// Expected: block is SUPPRESSED entirely (>60% administrative lines)
//
// Input think block:
//   The question asks about Schrödinger's monism vs Buddhist philosophy.
//   Schrödinger's position: consciousness is singular.
//   What the retrieved sources give me: [religion-science] notes Buddhism
//   rejects belief in substantive souls.
//
// Expected: block is PRESERVED (substantive philosophical reasoning)

// ─── Rename & Auto-Title ────────────────────────────────────

#[tokio::test]
async fn rename_updates_title_and_persists() {
    let h = TestHarness::new();
    let conv_id = "rename-test";

    // Seed a conversation by sending any message (this creates the row via
    // save_message upsert).
    h.send_in("Hello", conv_id).await;

    // Rename it.
    h.store
        .update_conversation_title(conv_id, "My custom title")
        .await
        .expect("rename should succeed");

    let reloaded = h.store.get_conversation(conv_id).await.unwrap();
    assert_eq!(reloaded.title.as_deref(), Some("My custom title"));
}

#[tokio::test]
async fn rename_nonexistent_returns_not_found() {
    let h = TestHarness::new();
    let result = h
        .store
        .update_conversation_title("does-not-exist", "anything")
        .await;
    assert!(
        result.is_err(),
        "renaming a missing conversation should return NotFound"
    );
}

#[tokio::test]
async fn auto_title_no_op_when_conversation_missing() {
    let h = TestHarness::new();
    // Conversation doesn't exist — try_auto_title should fail cleanly (NotFound).
    let result = sovereign_core::title::try_auto_title(
        std::sync::Arc::new(harness::DeterministicInference).as_ref(),
        h.store.as_ref() as &dyn sovereign_core::traits::StateStore,
        "ghost-convo",
    )
    .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn auto_title_gated_by_message_count() {
    let h = TestHarness::new();
    let conv_id = "gate-test";
    let inference: std::sync::Arc<dyn sovereign_core::traits::InferenceProvider> =
        std::sync::Arc::new(harness::DeterministicInference);
    let store_ref: &dyn sovereign_core::traits::StateStore = h.store.as_ref();

    // Zero messages: conversation row doesn't exist — skip this branch, we
    // test the gate from one-message state upward.

    // Save just a user message — no assistant yet.
    let user_only = Message {
        id: "m1".to_string(),
        conversation_id: conv_id.to_string(),
        role: Role::User,
        content: "Hi".to_string(),
        created_at: 0,
        metadata: None,
        version: 0,
    };
    h.store.save_message(&user_only).await.unwrap();

    // Only 1 message: should return None (no full exchange yet).
    let result = sovereign_core::title::try_auto_title(inference.as_ref(), store_ref, conv_id)
        .await
        .expect("gate check should not error");
    assert!(
        result.is_none(),
        "auto-title should skip with only a user message"
    );

    // Confirm no title was saved.
    let convo = h.store.get_conversation(conv_id).await.unwrap();
    assert!(convo.title.is_none());

    // Add the assistant response.
    let assistant = Message {
        id: "m2".to_string(),
        conversation_id: conv_id.to_string(),
        role: Role::Assistant,
        content: "Hello! How can I help?".to_string(),
        created_at: 1,
        metadata: None,
        version: 0,
    };
    h.store.save_message(&assistant).await.unwrap();

    // Now auto-title should run and persist.
    let result = sovereign_core::title::try_auto_title(inference.as_ref(), store_ref, conv_id)
        .await
        .expect("generation should succeed");
    assert_eq!(
        result.as_deref(),
        Some("Test conversation title"),
        "auto-title should produce the deterministic harness output"
    );

    let convo = h.store.get_conversation(conv_id).await.unwrap();
    assert_eq!(convo.title.as_deref(), Some("Test conversation title"));
}

#[tokio::test]
async fn auto_title_skips_when_title_already_set() {
    let h = TestHarness::new();
    let conv_id = "already-titled";
    let inference: std::sync::Arc<dyn sovereign_core::traits::InferenceProvider> =
        std::sync::Arc::new(harness::DeterministicInference);
    let store_ref: &dyn sovereign_core::traits::StateStore = h.store.as_ref();

    // Seed with an exchange and a user-set title.
    h.send_in("hello", conv_id).await;
    h.store
        .update_conversation_title(conv_id, "User picked this name")
        .await
        .unwrap();

    let result = sovereign_core::title::try_auto_title(inference.as_ref(), store_ref, conv_id)
        .await
        .expect("should not error");
    assert!(
        result.is_none(),
        "auto-title must not overwrite an existing title"
    );

    let convo = h.store.get_conversation(conv_id).await.unwrap();
    assert_eq!(convo.title.as_deref(), Some("User picked this name"));
}

#[tokio::test]
async fn auto_title_skips_when_only_assistant_messages() {
    // Edge case: somehow a conversation has only assistant messages (shouldn't
    // happen in practice, but the gate should guard against it).
    let h = TestHarness::new();
    let conv_id = "assistant-only";
    let inference: std::sync::Arc<dyn sovereign_core::traits::InferenceProvider> =
        std::sync::Arc::new(harness::DeterministicInference);

    for i in 0..3 {
        let msg = Message {
            id: format!("a{i}"),
            conversation_id: conv_id.to_string(),
            role: Role::Assistant,
            content: format!("assistant {i}"),
            created_at: i as i64,
            metadata: None,
            version: 0,
        };
        h.store.save_message(&msg).await.unwrap();
    }

    let result = sovereign_core::title::try_auto_title(
        inference.as_ref(),
        h.store.as_ref() as &dyn sovereign_core::traits::StateStore,
        conv_id,
    )
    .await
    .expect("should not error");
    assert!(result.is_none(), "need both a user and assistant message");
}

// ─── Document Skeleton Self-Heal ───────────────────────────

#[tokio::test]
async fn rebuild_skeleton_from_stored_chunks() {
    use sovereign_core::types::{
        AssetState, DocumentAsset, DocumentChunk, DocumentTypeTag, SourceType,
    };
    use sovereign_core::traits::DocumentAssetStore;

    let h = TestHarness::new();

    // Seed a document asset that has no skeleton — simulating an ingest
    // that was interrupted before save_asset_skeleton could run.
    let asset_id = "test-asset-id".to_string();
    let source_id = format!("asset:{asset_id}");
    let asset = DocumentAsset {
        id: asset_id.clone(),
        title: "Test Document".to_string(),
        filename: "test.pdf".to_string(),
        file_size_mb: 1.0,
        word_count: 200,
        chunk_count: 4,
        document_type: DocumentTypeTag::Unknown,
        ingested_at: chrono::Utc::now(),
        index_id: format!("doc-{asset_id}"),
        skeleton: None,
        state: AssetState::PartiallyReady,
    };
    h.store.save_document_asset(&asset).await.unwrap();

    // Seed the chunks that the rebuild will process.
    let chunks: Vec<DocumentChunk> = (0..4)
        .map(|i| DocumentChunk {
            id: format!("{source_id}:{i}"),
            source: source_id.clone(),
            content: format!("Chunk {i} — introduces the central concept with test content."),
            chunk_index: i,
            embedding: None,
            created_at: 0,
            source_type: SourceType::UserDocument,
            version: 0,
            deleted_at: None,
        })
        .collect();
    h.store.store_chunks(&chunks).await.unwrap();

    // Drive the rebuild.
    let inference: std::sync::Arc<dyn sovereign_core::traits::InferenceProvider> =
        std::sync::Arc::new(harness::DeterministicInference);
    let store_arc: std::sync::Arc<dyn sovereign_core::traits::StateStore> =
        std::sync::Arc::clone(&h.store)
            as std::sync::Arc<dyn sovereign_core::traits::StateStore>;
    let manager =
        sovereign_tools::document_asset::DocumentAssetManager::new(inference, store_arc);

    let skeleton = manager
        .rebuild_skeleton(&asset_id)
        .await
        .expect("rebuild should succeed on an asset with stored chunks");

    // The deterministic harness returns one SectionAnnotation per batch
    // with a "Test Entity". Assert the skeleton reflects that.
    assert!(
        !skeleton.sections.is_empty(),
        "rebuilt skeleton should have at least one section annotation"
    );
    assert!(
        skeleton.main_entities.iter().any(|e| e.name == "Test Entity"),
        "rebuilt skeleton should include 'Test Entity' from the harness"
    );

    // And the asset in the store should reflect the updated skeleton +
    // document_type atomically (document_type was Unknown, should now be
    // Argument per the harness's detect_document_type response).
    let reloaded = h
        .store
        .get_document_asset(&asset_id)
        .await
        .unwrap()
        .expect("asset should still exist");
    assert!(
        reloaded.skeleton.is_some(),
        "asset should have a persisted skeleton after rebuild"
    );
    assert_eq!(
        reloaded.document_type,
        DocumentTypeTag::Argument,
        "document_type should be updated atomically with the skeleton"
    );
    assert!(
        matches!(reloaded.state, AssetState::Ready),
        "asset state should transition to Ready after rebuild"
    );
}

#[tokio::test]
async fn rebuild_skeleton_missing_chunks_returns_not_found() {
    use sovereign_core::types::{
        AssetState, DocumentAsset, DocumentTypeTag,
    };
    use sovereign_core::traits::DocumentAssetStore;

    let h = TestHarness::new();

    // Asset exists but no chunks were ever stored.
    let asset_id = "orphan-asset".to_string();
    let asset = DocumentAsset {
        id: asset_id.clone(),
        title: "Orphan".to_string(),
        filename: "orphan.pdf".to_string(),
        file_size_mb: 1.0,
        word_count: 0,
        chunk_count: 0,
        document_type: DocumentTypeTag::Unknown,
        ingested_at: chrono::Utc::now(),
        index_id: format!("doc-{asset_id}"),
        skeleton: None,
        state: AssetState::Pending,
    };
    h.store.save_document_asset(&asset).await.unwrap();

    let inference: std::sync::Arc<dyn sovereign_core::traits::InferenceProvider> =
        std::sync::Arc::new(harness::DeterministicInference);
    let store_arc: std::sync::Arc<dyn sovereign_core::traits::StateStore> =
        std::sync::Arc::clone(&h.store)
            as std::sync::Arc<dyn sovereign_core::traits::StateStore>;
    let manager =
        sovereign_tools::document_asset::DocumentAssetManager::new(inference, store_arc);

    let result = manager.rebuild_skeleton(&asset_id).await;
    assert!(
        result.is_err(),
        "rebuild should fail when no chunks are available"
    );
}

// ─── ReasonWithTools ─────────────────────────────────────────

use sovereign_core::executor::{AutoApprovalChannel, Executor, TaskContext};
use sovereign_core::ToolRegistry;
use sovereign_core::SkillRegistry;

#[tokio::test]
async fn reason_with_tools_searches_then_synthesizes() {
    let h = TestHarness::new();
    h.ingest_test_corpus(
        "sep",
        vec![
            ("bergson", "Henri Bergson wrote Laughter examining comedy as social corrective."),
            ("epistemology", "Epistemology studies the nature and scope of knowledge."),
        ],
    )
    .await;

    // Build an executor with the real store (which has corpus data).
    let inference = std::sync::Arc::new(harness::DeterministicInference);
    let store: std::sync::Arc<dyn sovereign_core::traits::StateStore> =
        std::sync::Arc::clone(&h.store) as std::sync::Arc<dyn sovereign_core::traits::StateStore>;

    // Register the search tool so ReasonWithTools can call it.
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(sovereign_tools::search::SearchTool::new(
        std::sync::Arc::clone(&store),
        std::sync::Arc::clone(&inference) as std::sync::Arc<dyn sovereign_core::traits::InferenceProvider>,
    )));

    let executor = Executor::new(
        inference,
        std::sync::Arc::new(tools),
        store,
        std::sync::Arc::new(AutoApprovalChannel),
        std::sync::Arc::new(SkillRegistry::new()),
    );

    let plan = Plan {
        id: "rwt-test".to_string(),
        goal: "research Bergson".to_string(),
        steps: vec![Step {
            id: 0,
            description: "Research Bergson's theory of humor".to_string(),
            kind: StepKind::ReasonWithTools {
                prompt_template: "What did Bergson write about humor and laughter?".to_string(),
                speed: Speed::Slow,
                available_tools: vec!["search".to_string()],
                max_iterations: 4,
            },
            requires_approval: false,
            inputs: vec![],
            sampling: None,
            evaluation: None,
        }],
        edges: vec![],
    };

    let task = Task {
        id: "rwt-task".to_string(),
        conversation_id: "rwt-conv".to_string(),
        goal: "test".to_string(),
        plan: plan.clone(),
        status: TaskStatus::Running,
        completed_steps: Vec::new(),
        created_at: 0,
        updated_at: 0,
        version: 0,
    };

    let mut ctx = TaskContext {
        task,
        completed: std::collections::HashMap::new(),
    };

    let result = executor.run(&plan, &mut ctx).await.unwrap();
    assert!(result.error.is_none(), "Execution should succeed");

    // Verify the output is a ReasonWithToolsResult.
    let output = result.completed.get(&0).expect("Step 0 should have output");
    match output {
        StepOutput::ReasonWithToolsResult {
            text,
            search_log,
            iterations,
            capped,
        } => {
            assert!(!text.is_empty(), "Synthesis text should not be empty");
            assert!(
                *iterations >= 1,
                "Should have at least 1 search iteration, got {iterations}"
            );
            assert!(
                !search_log.is_empty(),
                "Search log should have at least 1 entry"
            );
            assert!(
                !capped,
                "Should not have hit the iteration cap with max_iterations=4"
            );
            // The search log should show what was queried.
            assert!(
                !search_log[0].query.is_empty(),
                "Search log entry should have a query"
            );
        }
        other => panic!(
            "Expected ReasonWithToolsResult, got {:?}",
            std::mem::discriminant(other)
        ),
    }
}

#[tokio::test]
async fn reason_with_tools_caps_at_max_iterations() {
    let h = TestHarness::new();
    h.ingest_test_corpus("sep", vec![("test", "Some content.")]).await;

    let inference = std::sync::Arc::new(harness::AlwaysSearchInference);
    let store: std::sync::Arc<dyn sovereign_core::traits::StateStore> =
        std::sync::Arc::clone(&h.store) as std::sync::Arc<dyn sovereign_core::traits::StateStore>;

    let mut tools = ToolRegistry::new();
    tools.register(Box::new(sovereign_tools::search::SearchTool::new(
        std::sync::Arc::clone(&store),
        std::sync::Arc::clone(&inference) as std::sync::Arc<dyn sovereign_core::traits::InferenceProvider>,
    )));

    let executor = Executor::new(
        inference,
        std::sync::Arc::new(tools),
        store,
        std::sync::Arc::new(AutoApprovalChannel),
        std::sync::Arc::new(SkillRegistry::new()),
    );

    let plan = Plan {
        id: "cap-test".to_string(),
        goal: "test cap".to_string(),
        steps: vec![Step {
            id: 0,
            description: "Search repeatedly".to_string(),
            kind: StepKind::ReasonWithTools {
                prompt_template: "Keep searching".to_string(),
                speed: Speed::Fast,
                available_tools: vec!["search".to_string()],
                max_iterations: 2,
            },
            requires_approval: false,
            inputs: vec![],
            sampling: None,
            evaluation: None,
        }],
        edges: vec![],
    };

    let task = Task {
        id: "cap-task".to_string(),
        conversation_id: "cap-conv".to_string(),
        goal: "test".to_string(),
        plan: plan.clone(),
        status: TaskStatus::Running,
        completed_steps: Vec::new(),
        created_at: 0,
        updated_at: 0,
        version: 0,
    };

    let mut ctx = TaskContext {
        task,
        completed: std::collections::HashMap::new(),
    };

    let result = executor.run(&plan, &mut ctx).await.unwrap();
    let output = result.completed.get(&0).unwrap();

    match output {
        StepOutput::ReasonWithToolsResult { iterations, capped, .. } => {
            assert_eq!(*iterations, 2, "Should hit the cap at 2 iterations");
            assert!(*capped, "Should be capped");
        }
        other => panic!("Expected ReasonWithToolsResult, got {:?}", std::mem::discriminant(other)),
    }
}

// ─── Auto-Collaborate (Phase 2) ──────────────────────────────
//
// These tests exercise `Runtime::maybe_collaborate` via the SimpleQuery
// path (PassthroughRouter sends everything to SimpleQuery). The
// ScriptableInference + ScriptedApprovalChannel doubles control the
// gap-check output, the refinement output, and the user's choice.

#[tokio::test]
async fn auto_collaborate_off_passes_through_unchanged() {
    // With auto_collaborate disabled (TestHarness::new uses default config),
    // the SimpleQuery path must never call the gap checker or approval
    // channel. `RefineScript::Unused` / `InfoResponseScript::Unused` would
    // panic if touched — we use the regular harness here to keep things
    // explicit about "this path doesn't even reach the scriptable code".
    let h = TestHarness::new();
    let resp = h.send("What is epistemology?").await;

    // Message is the deterministic echo; no refinement happened.
    assert!(
        !resp.message.content.contains("refined"),
        "auto_collaborate=false must not alter the answer"
    );
}

#[tokio::test]
async fn auto_collaborate_on_with_no_gap_returns_original_answer() {
    let h = TestHarness::new_with_collaborate(
        GapScript::NoGap,
        RefineScript::Unused,
        InfoResponseScript::Unused,
    );
    let resp = h.send("What is epistemology?").await;

    // Gap check ran, reported no gap → no info request, no refinement.
    // The message should be whatever the deterministic synthesis produced
    // (an echo of the prompt). The absence of panic is the main signal —
    // Unused scripts panic if invoked.
    assert!(
        !resp.message.content.is_empty(),
        "synthesis still produces a message when no gap is identified"
    );
}

#[tokio::test]
async fn auto_collaborate_on_with_gap_and_user_content_returns_refined_answer() {
    let h = TestHarness::new_with_collaborate(
        GapScript::Gap {
            gap: "Need a 2024 pharmaceutical R&D statistic".to_string(),
        },
        RefineScript::Text(
            "REFINED: integrates user source on pharma R&D post-IRA.".to_string(),
        ),
        InfoResponseScript::Pasted(
            "Per NEJM 2024: post-IRA R&D investment fell 12%.".to_string(),
        ),
    );
    let resp = h.send("What is the evidence on IRA's innovation effects?").await;

    assert!(
        resp.message.content.starts_with("REFINED:"),
        "expected refined answer, got: {}",
        resp.message.content
    );
}

#[tokio::test]
async fn auto_collaborate_on_with_user_skip_returns_original_answer() {
    let h = TestHarness::new_with_collaborate(
        GapScript::Gap {
            gap: "Need a primary source".to_string(),
        },
        // User pressed Skip → refinement must not be called.
        RefineScript::Unused,
        InfoResponseScript::Skip,
    );
    let resp = h.send("What is the evidence on IRA's innovation effects?").await;

    // The original corpus-only answer stays put; no panic from Unused script.
    assert!(
        !resp.message.content.starts_with("REFINED:"),
        "skip must preserve the original answer; got: {}",
        resp.message.content
    );
}

#[tokio::test]
async fn auto_collaborate_on_with_inference_error_returns_original_answer() {
    let h = TestHarness::new_with_collaborate(
        // Gap-check inference fails — the helper is documented to fall back
        // to the original response rather than propagate the error.
        GapScript::Error,
        RefineScript::Unused,
        InfoResponseScript::Unused,
    );
    let resp = h.send("What is epistemology?").await;

    // Turn must still succeed — the gap-check error is swallowed.
    assert!(
        !resp.message.content.is_empty(),
        "gap-check error must not fail the turn"
    );
    assert!(
        !resp.message.content.starts_with("REFINED:"),
        "no refinement should happen when gap check errored"
    );
}

// ─── Epistemic Humility: default-on invariants (Phase 3) ─────

#[test]
fn default_inference_config_has_epistemic_humility_on() {
    // If someone accidentally flips this back to false, every new
    // install loses the feature silently. This test stands guard.
    let cfg = sovereign_core::types::InferenceConfig::default();
    assert!(
        cfg.auto_collaborate,
        "InferenceConfig::default().auto_collaborate must be true"
    );
}

// ─── Epistemic Humility: streaming path (Phase 3) ─────────────
//
// `handle_message_stream` would be cumbersome to drive end-to-end from
// a pure unit test (needs a streaming inference provider, a running
// tokio task consuming the Receiver, and a way to await the post-
// stream spawn). Instead we exercise `apply_post_stream_refinement`
// directly — it's the exact function the streaming spawn calls, so
// covering it covers the behaviour users feel.

#[tokio::test]
async fn post_stream_refinement_rewrites_message_and_emits_event() {
    let h = TestHarness::new_with_collaborate(
        GapScript::Gap {
            gap: "Need a 2024 primary source".to_string(),
        },
        RefineScript::Text("REFINED: streamed answer with user source.".to_string()),
        InfoResponseScript::Pasted("Paragraph from a relevant paper.".to_string()),
    );

    // Persist an initial "streamed" assistant message. The real
    // streaming spawn does this exact shape before calling the
    // refinement hook.
    let conv_id = uuid::Uuid::new_v4().to_string();
    let msg_id = uuid::Uuid::new_v4().to_string();
    let original = "Initial streamed answer from the corpus.";
    let meta = serde_json::json!({
        "streamed": true,
        "provenance": {},
    });
    let initial = Message {
        id: msg_id.clone(),
        conversation_id: conv_id.clone(),
        role: Role::Assistant,
        content: original.to_string(),
        created_at: 0,
        metadata: Some(meta.clone()),
        version: 0,
    };
    h.store.save_message(&initial).await.unwrap();

    let refined = h
        .runtime
        .apply_post_stream_refinement(
            &conv_id,
            &msg_id,
            "What's the current evidence?",
            original,
            "evidence text",
            Some(meta),
        )
        .await;

    assert!(refined.is_some(), "refinement should have fired");
    assert!(
        refined.as_deref().unwrap().starts_with("REFINED:"),
        "refined text should come from the scripted refine response"
    );

    // The persisted message should now carry the refined content.
    let conv = h.store.get_conversation(&conv_id).await.unwrap();
    let msg = conv
        .messages
        .iter()
        .find(|m| m.id == msg_id)
        .expect("message should still exist");
    assert!(
        msg.content.starts_with("REFINED:"),
        "store should reflect the refined content, got: {}",
        msg.content
    );

    // And the approval channel should have emitted exactly one
    // message-refined event with the matching id.
    let events = h
        .scripted_approval
        .as_ref()
        .expect("collaborate harness sets scripted_approval")
        .refined_emissions();
    assert_eq!(events.len(), 1, "exactly one emission expected");
    assert_eq!(events[0].message_id, msg_id);
    assert_eq!(events[0].conversation_id, conv_id);
    assert!(events[0].new_content.starts_with("REFINED:"));
}

#[tokio::test]
async fn post_stream_refinement_noops_when_no_gap() {
    let h = TestHarness::new_with_collaborate(
        GapScript::NoGap,
        // Refine must not be called when there's no gap.
        RefineScript::Unused,
        InfoResponseScript::Unused,
    );

    let conv_id = uuid::Uuid::new_v4().to_string();
    let msg_id = uuid::Uuid::new_v4().to_string();
    let original = "Initial streamed answer with sufficient evidence.";
    let initial = Message {
        id: msg_id.clone(),
        conversation_id: conv_id.clone(),
        role: Role::Assistant,
        content: original.to_string(),
        created_at: 0,
        metadata: None,
        version: 0,
    };
    h.store.save_message(&initial).await.unwrap();

    let refined = h
        .runtime
        .apply_post_stream_refinement(&conv_id, &msg_id, "Q?", original, "E", None)
        .await;

    assert!(refined.is_none(), "should no-op when gap check says no gap");

    let events = h.scripted_approval.as_ref().unwrap().refined_emissions();
    assert!(events.is_empty(), "no refinement event should be emitted");
}

#[tokio::test]
async fn post_stream_refinement_noops_when_user_skips() {
    let h = TestHarness::new_with_collaborate(
        GapScript::Gap {
            gap: "something".to_string(),
        },
        RefineScript::Unused,
        InfoResponseScript::Skip,
    );

    let conv_id = uuid::Uuid::new_v4().to_string();
    let msg_id = uuid::Uuid::new_v4().to_string();
    let original = "Initial streamed answer.";
    let initial = Message {
        id: msg_id.clone(),
        conversation_id: conv_id.clone(),
        role: Role::Assistant,
        content: original.to_string(),
        created_at: 0,
        metadata: None,
        version: 0,
    };
    h.store.save_message(&initial).await.unwrap();

    let refined = h
        .runtime
        .apply_post_stream_refinement(&conv_id, &msg_id, "Q?", original, "E", None)
        .await;

    assert!(refined.is_none(), "skip → no-op");
    assert!(h
        .scripted_approval
        .as_ref()
        .unwrap()
        .refined_emissions()
        .is_empty());
}

