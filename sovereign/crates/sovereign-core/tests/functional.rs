// SPDX-License-Identifier: AGPL-3.0-or-later
mod harness;

use harness::{InfoResponseScript, PhraseScript, RefineScript, TestHarness};
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
        prov.sources
            .iter()
            .any(|s| s.origin == "sep" && s.count > 0),
        "Should find SEP chunks for Bergson query. Sources: {:?}",
        prov.sources
    );
}

/// I2-A / invariant I1: the simple-answer surface persists the typed
/// epistemic ledger on the assistant message when the flag is on (default).
/// Guards the ledger-persistence contract the KQ/streaming/complex-task/
/// attached-doc surfaces all share.
#[tokio::test]
async fn simple_surface_persists_epistemic_ledger() {
    let h = TestHarness::new();
    let resp = h.send("Say hello.").await;
    let meta = resp
        .message
        .metadata
        .as_ref()
        .expect("assistant message should carry metadata");
    let ledger = meta
        .get("epistemic_state")
        .expect("simple surface must carry an epistemic_state key");
    assert!(
        !ledger.is_null(),
        "ledger must be populated when SOVEREIGN_EPISTEMIC_STATE is on (default): {meta}"
    );
    assert!(
        ledger.get("verdict").is_some(),
        "epistemic_state must carry a derived verdict: {ledger}"
    );
}

/// I2-A: the attached-doc surface persists the epistemic ledger. A bare
/// `DocumentSession` on the conversation routes the turn to the attached-doc
/// handler; the handler degrades gracefully with no asset/chunks (empty
/// briefing, zero retrieved) and finalizes through
/// `package_attached_doc_response`, which assembles the ledger regardless of
/// whether the (default-off) grounding gate ran. Before I2-A this surface
/// threw away its gate claims and wrote no ledger.
#[tokio::test]
async fn attached_doc_surface_persists_epistemic_ledger() {
    let h = TestHarness::new();
    let conv = "attached-doc-epistemic";
    let session = DocumentSession {
        id: "sess-epistemic-1".into(),
        conversation_id: conv.into(),
        filename: "notes.txt".into(),
        source: "asset:missing".into(),
        word_count: 0,
        chunk_count: 0,
        created_at: 0,
        operation: "answer questions about the document".into(),
        map_prompt: String::new(),
        reduce_prompt: String::new(),
        last_output: None,
        history: Vec::new(),
    };
    h.store
        .create_document_session(&session)
        .await
        .expect("create document session");

    let resp = h
        .send_in("What does the document say about the topic?", conv)
        .await;
    let meta = resp
        .message
        .metadata
        .as_ref()
        .expect("assistant message should carry metadata");
    // Confirm the turn actually took the attached-doc surface.
    assert_eq!(
        meta.get("intent").and_then(|v| v.as_str()),
        Some("AttachedDoc"),
        "turn should route to the attached-doc surface: {meta}"
    );
    let ledger = meta
        .get("epistemic_state")
        .expect("attached-doc must carry an epistemic_state key");
    assert!(
        !ledger.is_null(),
        "ledger must be populated when SOVEREIGN_EPISTEMIC_STATE is on (default): {meta}"
    );
    assert!(
        ledger.get("verdict").is_some(),
        "epistemic_state must carry a derived verdict: {ledger}"
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
        wiki_source.is_none_or(|s| s.count == 0),
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
    let resp = h
        .send("What's Rust? Is it good for systems programming?")
        .await;
    let prov = h.provenance(&resp);

    assert!(
        prov.sources
            .iter()
            .any(|s| s.origin == "wiki" && s.count > 0),
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
        source_conversation_id: None,
        source_skill_id: None,
        ..Default::default()
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
            (
                "doc1",
                "Quantum mechanics describes the behavior of particles at atomic scales.",
            ),
            (
                "doc2",
                "Classical mechanics was developed by Newton and Lagrange.",
            ),
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

    let resp = h
        .send("What is quantum superposition and how does it relate to consciousness?")
        .await;

    // The response should not contain [unverified] tags — the layered
    // confidence system should present general knowledge naturally.
    assert!(
        !resp.message.content.contains("[unverified]"),
        "Response should not contain [unverified] tags. Got: {}",
        resp.message.content
    );

    // Should not refuse to answer.
    assert!(
        !resp
            .message
            .content
            .to_lowercase()
            .contains("i cannot find"),
        "Should not refuse to answer. Got: {}",
        resp.message.content
    );
    assert!(
        !resp
            .message
            .content
            .to_lowercase()
            .contains("i cannot provide"),
        "Should not refuse to answer. Got: {}",
        resp.message.content
    );
}

#[tokio::test]
async fn empty_corpus_produces_response_not_refusal() {
    let h = TestHarness::new();
    h.ingest_test_corpus(
        "empty",
        vec![("stub", "Unrelated stub content about cooking recipes.")],
    )
    .await;

    // Ask about something not in the corpus at all.
    let resp = h
        .send("What are the core differences between Theravada and Zen Buddhism?")
        .await;

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
    let r1 = h
        .send_in("Tell me about Schrödinger's cat experiment", conv_id)
        .await;
    assert!(!r1.message.content.is_empty());

    // Turn 2: follow up in the same domain.
    let r2 = h
        .send_in("How does this relate to quantum decoherence?", conv_id)
        .await;
    assert!(!r2.message.content.is_empty());

    // Turn 3: a third turn.
    let r3 = h
        .send_in("What about the many-worlds interpretation?", conv_id)
        .await;
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
    use sovereign_core::traits::DocumentAssetStore;
    use sovereign_core::types::{
        AssetState, DocumentAsset, DocumentChunk, DocumentTypeTag, SourceType,
    };

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
        owner: None,
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
        std::sync::Arc::clone(&h.store) as std::sync::Arc<dyn sovereign_core::traits::StateStore>;
    let manager = sovereign_tools::document_asset::DocumentAssetManager::new(inference, store_arc);

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
        skeleton
            .main_entities
            .iter()
            .any(|e| e.name == "Test Entity"),
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

/// Regression — the id the UI receives from `prepare` must be the SAME
/// id every `IngestProgress` fires under. The desktop upload command
/// once minted one asset id, returned it to the UI, then let `ingest`
/// mint a *second* id internally: the banner subscribed to the first
/// while events fired under the second, so it sat on "Queued…" for the
/// entire ingest while a duplicate record quietly progressed to Ready.
#[tokio::test]
async fn ingest_emits_progress_under_the_prepared_asset_id() {
    use sovereign_core::traits::DocumentAssetStore;
    use sovereign_core::types::AssetState;
    use sovereign_tools::document_asset::IngestProgress;
    use std::sync::{Arc, Mutex};

    let h = TestHarness::new();

    // A small text document — one chunk, so the pipeline finishes fast.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("regression_doc.txt");
    std::fs::write(
        &path,
        "Ada Lovelace wrote the first algorithm intended for a machine. \
         She collaborated with Charles Babbage on the Analytical Engine.",
    )
    .unwrap();

    let inference: Arc<dyn sovereign_core::traits::InferenceProvider> =
        Arc::new(harness::DeterministicInference);
    let store_arc: Arc<dyn sovereign_core::traits::StateStore> =
        Arc::clone(&h.store) as Arc<dyn sovereign_core::traits::StateStore>;
    let manager = sovereign_tools::document_asset::DocumentAssetManager::new(inference, store_arc);

    // prepare() persists a Pending asset and hands back its id.
    let prepared = manager
        .prepare(&path)
        .await
        .expect("prepare should succeed");
    let expected_id = prepared.asset.id.clone();
    assert!(
        matches!(prepared.asset.state, AssetState::Pending),
        "prepared asset should start Pending"
    );
    let seeded = h
        .store
        .get_document_asset(&expected_id)
        .await
        .unwrap()
        .expect("prepare should persist the asset");
    assert!(matches!(seeded.state, AssetState::Pending));

    // run_ingest() must emit every id-bearing progress event under the
    // SAME id prepare returned — never a freshly-minted one.
    let seen_ids: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen_ids);
    let completed = manager
        .run_ingest(prepared, move |p| {
            let id = match p {
                IngestProgress::Started { asset_id, .. } => Some(asset_id),
                IngestProgress::RagAvailable { asset_id } => Some(asset_id),
                IngestProgress::MultiHopReady { asset_id } => Some(asset_id),
                IngestProgress::Ready { asset_id, .. } => Some(asset_id),
                _ => None,
            };
            if let Some(id) = id {
                sink.lock().unwrap().push(id);
            }
        })
        .await
        .expect("run_ingest should succeed");

    assert_eq!(
        completed.id, expected_id,
        "completed asset must keep the id prepare returned"
    );
    assert!(matches!(completed.state, AssetState::Ready));

    // Block-scoped: the guard must not (even lexically) span the await
    // below — clippy::await_holding_lock is scope-based, not drop-based.
    {
        let ids = seen_ids.lock().unwrap();
        assert!(
            !ids.is_empty(),
            "at least the Started + Ready events should have fired"
        );
        for id in ids.iter() {
            assert_eq!(
                id, &expected_id,
                "every progress event must carry the prepared asset id — \
                 a different id is the dual-asset bug regressing"
            );
        }
    }

    // Exactly one record for this id; run_ingest minted no duplicate.
    let reloaded = h
        .store
        .get_document_asset(&expected_id)
        .await
        .unwrap()
        .expect("the single asset record should still exist");
    assert!(matches!(reloaded.state, AssetState::Ready));
}

#[tokio::test]
async fn rebuild_skeleton_missing_chunks_returns_not_found() {
    use sovereign_core::traits::DocumentAssetStore;
    use sovereign_core::types::{AssetState, DocumentAsset, DocumentTypeTag};

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
        owner: None,
    };
    h.store.save_document_asset(&asset).await.unwrap();

    let inference: std::sync::Arc<dyn sovereign_core::traits::InferenceProvider> =
        std::sync::Arc::new(harness::DeterministicInference);
    let store_arc: std::sync::Arc<dyn sovereign_core::traits::StateStore> =
        std::sync::Arc::clone(&h.store) as std::sync::Arc<dyn sovereign_core::traits::StateStore>;
    let manager = sovereign_tools::document_asset::DocumentAssetManager::new(inference, store_arc);

    let result = manager.rebuild_skeleton(&asset_id).await;
    assert!(
        result.is_err(),
        "rebuild should fail when no chunks are available"
    );
}

// ─── ReasonWithTools ─────────────────────────────────────────

use sovereign_core::executor::{AutoApprovalChannel, Executor, TaskContext};
use sovereign_core::SkillRegistry;
use sovereign_core::ToolRegistry;

#[tokio::test]
async fn reason_with_tools_searches_then_synthesizes() {
    let h = TestHarness::new();
    h.ingest_test_corpus(
        "sep",
        vec![
            (
                "bergson",
                "Henri Bergson wrote Laughter examining comedy as social corrective.",
            ),
            (
                "epistemology",
                "Epistemology studies the nature and scope of knowledge.",
            ),
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
        std::sync::Arc::clone(&inference)
            as std::sync::Arc<dyn sovereign_core::traits::InferenceProvider>,
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
    h.ingest_test_corpus("sep", vec![("test", "Some content.")])
        .await;

    let inference = std::sync::Arc::new(harness::AlwaysSearchInference);
    let store: std::sync::Arc<dyn sovereign_core::traits::StateStore> =
        std::sync::Arc::clone(&h.store) as std::sync::Arc<dyn sovereign_core::traits::StateStore>;

    let mut tools = ToolRegistry::new();
    tools.register(Box::new(sovereign_tools::search::SearchTool::new(
        std::sync::Arc::clone(&store),
        std::sync::Arc::clone(&inference)
            as std::sync::Arc<dyn sovereign_core::traits::InferenceProvider>,
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
        StepOutput::ReasonWithToolsResult {
            iterations, capped, ..
        } => {
            assert_eq!(*iterations, 2, "Should hit the cap at 2 iterations");
            assert!(*capped, "Should be capped");
        }
        other => panic!(
            "Expected ReasonWithToolsResult, got {:?}",
            std::mem::discriminant(other)
        ),
    }
}

// ─── Auto-Collaborate (Phase 2 → I4-C structural detection) ──
//
// These tests exercise `Runtime::maybe_collaborate` directly. Since the
// I4-C retirement of gap.rs, DETECTION is structural — the card fires
// iff the caller passes `abstained: true` (the gate signal the ledger's
// CannotKnowFromHere verdict derives from) — so the scripted inference
// only shapes the card's phrased ask and the refinement output; the
// ScriptedApprovalChannel controls the user's choice.

#[tokio::test]
async fn auto_collaborate_off_passes_through_unchanged() {
    // With auto_collaborate disabled (TestHarness::new uses default config),
    // the SimpleQuery path must never reach the phrasing or approval
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
async fn auto_collaborate_answered_turn_passes_through_instantly() {
    let h = TestHarness::new_with_collaborate(
        // An answered turn (abstained=false) must touch NO script:
        // detection is structural, no phrasing call, no card.
        PhraseScript::Unused,
        RefineScript::Unused,
        InfoResponseScript::Unused,
    );
    let original = "Epistemology is the study of knowledge.";
    let out = h
        .runtime
        .maybe_collaborate("conv-1", "What is epistemology?", original, false)
        .await;
    assert_eq!(out, original, "answered turn must pass through unchanged");
}

#[tokio::test]
async fn auto_collaborate_abstained_with_user_content_returns_refined_answer() {
    let h = TestHarness::new_with_collaborate(
        PhraseScript::Text("A 2024 primary source on post-IRA R&D investment.".to_string()),
        RefineScript::Text("REFINED: integrates user source on pharma R&D post-IRA.".to_string()),
        InfoResponseScript::Pasted("Per NEJM 2024: post-IRA R&D investment fell 12%.".to_string()),
    );
    let out = h
        .runtime
        .maybe_collaborate(
            "conv-1",
            "What is the evidence on IRA's innovation effects?",
            "I couldn't confirm an answer to this from your sources.",
            true,
        )
        .await;
    assert!(
        out.starts_with("REFINED:"),
        "expected refined answer, got: {out}"
    );
}

#[tokio::test]
async fn auto_collaborate_abstained_with_user_skip_returns_original_answer() {
    let h = TestHarness::new_with_collaborate(
        PhraseScript::Text("A primary source.".to_string()),
        // User pressed Skip → refinement must not be called.
        RefineScript::Unused,
        InfoResponseScript::Skip,
    );
    let original = "I couldn't confirm an answer to this from your sources.";
    let out = h
        .runtime
        .maybe_collaborate(
            "conv-1",
            "What is the evidence on IRA's innovation effects?",
            original,
            true,
        )
        .await;
    // The original abstention stays put; no panic from Unused script.
    assert_eq!(out, original, "skip must preserve the original answer");
}

#[tokio::test]
async fn auto_collaborate_phrasing_error_still_fires_card_with_raw_question() {
    // D4: phrasing may phrase, never gate. A phrasing failure must not
    // suppress the card — the flow continues with the user's question
    // verbatim as the ask, and a pasted source still refines.
    let h = TestHarness::new_with_collaborate(
        PhraseScript::Error,
        RefineScript::Text("REFINED: with the pasted source.".to_string()),
        InfoResponseScript::Pasted("pasted source text".to_string()),
    );
    let out = h
        .runtime
        .maybe_collaborate(
            "conv-1",
            "What is epistemology?",
            "I couldn't confirm an answer to this from your sources.",
            true,
        )
        .await;
    assert!(
        out.starts_with("REFINED:"),
        "phrasing error must not suppress the card/refinement flow; got: {out}"
    );
}

// ─── Epistemic Humility: default-on invariants (Phase 3) ─────

/// Invariant I1, runtime half: a real turn through an answer surface
/// persists a ledger with a derived verdict — default-on, no env setup.
/// (The closed-surface compile-time pin lives in runtime/epistemic.rs;
/// this drives the SimpleQuery path end-to-end as its runtime witness.)
#[tokio::test]
async fn simple_turn_persists_epistemic_ledger_with_verdict() {
    let h = TestHarness::new();
    let resp = h.send("What is epistemology?").await;
    let ledger = resp
        .message
        .metadata
        .as_ref()
        .and_then(|m| m.get("epistemic_state"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    assert!(
        ledger.get("verdict").is_some_and(|v| v.is_string()),
        "I1: every answer turn must persist an epistemic_state with a derived verdict; got: {ledger}"
    );
}

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
        PhraseScript::Text("A 2024 primary source.".to_string()),
        RefineScript::Text("REFINED: streamed answer with user source.".to_string()),
        InfoResponseScript::Pasted("Paragraph from a relevant paper.".to_string()),
    );

    // Persist an initial "streamed" assistant message. The real
    // streaming spawn does this exact shape before calling the
    // refinement hook. The abstained gate action is the card's
    // detection signal (I4-C structural detection).
    let conv_id = uuid::Uuid::new_v4().to_string();
    let msg_id = uuid::Uuid::new_v4().to_string();
    let original = "Initial streamed answer from the corpus.";
    let meta = serde_json::json!({
        "streamed": true,
        "provenance": {},
        "grounding_gate": { "action": "abstained" },
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
async fn post_stream_refinement_noops_when_turn_answered() {
    let h = TestHarness::new_with_collaborate(
        // Answered turn (no abstained gate action in the metadata) →
        // structural detection says no gap; nothing may be invoked.
        PhraseScript::Unused,
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
async fn post_stream_refinement_emits_fallback_when_inference_errors() {
    // Repro for the 2026-05-25 "Refining your answer" stall: the
    // user clicked Search on the InformationRequestCard (so the
    // desktop set `m.refining = true`), the daemon ran the gap
    // check + web search, then refinement inference errored with
    // `Decode Error -3`. Backend logged "falling back to original"
    // and returned without emitting anything → desktop stuck on
    // the refining overlay forever.
    //
    // Invariant under test: when refinement INFERENCE errors after
    // the user has provided content, `run_post_stream_refinement`
    // MUST emit `message-refined` with the original content so the
    // frontend clears `m.refining`. The persisted message stays
    // unchanged (no rewrite). Stale-write to the dossier does NOT
    // fire (`refined.is_none()`).
    let h = TestHarness::new_with_collaborate(
        PhraseScript::Text("A primary source.".to_string()),
        RefineScript::Error,
        InfoResponseScript::Pasted("user-supplied source text".to_string()),
    );

    let conv_id = uuid::Uuid::new_v4().to_string();
    let msg_id = uuid::Uuid::new_v4().to_string();
    let original = "Initial streamed answer from the corpus.";
    let abstained_meta = serde_json::json!({ "grounding_gate": { "action": "abstained" } });
    let initial = Message {
        id: msg_id.clone(),
        conversation_id: conv_id.clone(),
        role: Role::Assistant,
        content: original.to_string(),
        created_at: 0,
        metadata: Some(abstained_meta.clone()),
        version: 0,
    };
    h.store.save_message(&initial).await.unwrap();

    let refined = h
        .runtime
        .apply_post_stream_refinement(&conv_id, &msg_id, "Q?", original, "E", Some(abstained_meta))
        .await;

    // Refinement errored → caller should see `None` so the
    // stale-write dossier path doesn't trigger.
    assert!(
        refined.is_none(),
        "inference error must not return a refined string"
    );

    // Persisted message must stay unchanged — we didn't rewrite it.
    let conv = h.store.get_conversation(&conv_id).await.unwrap();
    let msg = conv
        .messages
        .iter()
        .find(|m| m.id == msg_id)
        .expect("message should still exist");
    assert_eq!(
        msg.content, original,
        "store must not change when refinement errors"
    );

    // CRITICAL: a `message-refined` event MUST still be emitted
    // (with the original content) so the desktop's `m.refining`
    // flag clears. Without this emit the UI sticks on "Refining
    // your answer" forever.
    let events = h
        .scripted_approval
        .as_ref()
        .expect("collaborate harness sets scripted_approval")
        .refined_emissions();
    assert_eq!(
        events.len(),
        1,
        "exactly one fallback emission expected to clear UI flag"
    );
    assert_eq!(events[0].message_id, msg_id);
    assert_eq!(events[0].conversation_id, conv_id);
    assert_eq!(
        events[0].new_content, original,
        "fallback emission carries the original (unchanged) content"
    );
}

#[tokio::test]
async fn post_stream_refinement_emits_fallback_when_output_equals_original() {
    // Sibling invariant to the Error case: if the refinement model
    // produces text byte-identical to the original answer (the
    // "model declined to revise" branch), the desktop still has
    // `m.refining = true` from the user's Submit/Search click —
    // we must emit `message-refined` (with the original) to clear
    // the flag.
    let h = TestHarness::new_with_collaborate(
        PhraseScript::Text("A primary source.".to_string()),
        RefineScript::Text("Initial streamed answer from the corpus.".to_string()),
        InfoResponseScript::Pasted("user-supplied source text".to_string()),
    );

    let conv_id = uuid::Uuid::new_v4().to_string();
    let msg_id = uuid::Uuid::new_v4().to_string();
    let original = "Initial streamed answer from the corpus.";
    let abstained_meta = serde_json::json!({ "grounding_gate": { "action": "abstained" } });
    let initial = Message {
        id: msg_id.clone(),
        conversation_id: conv_id.clone(),
        role: Role::Assistant,
        content: original.to_string(),
        created_at: 0,
        metadata: Some(abstained_meta.clone()),
        version: 0,
    };
    h.store.save_message(&initial).await.unwrap();

    let refined = h
        .runtime
        .apply_post_stream_refinement(&conv_id, &msg_id, "Q?", original, "E", Some(abstained_meta))
        .await;

    assert!(
        refined.is_none(),
        "no-change refinement must not advertise as a successful rewrite"
    );

    let events = h.scripted_approval.as_ref().unwrap().refined_emissions();
    assert_eq!(
        events.len(),
        1,
        "no-change still emits message-refined to clear UI flag"
    );
    assert_eq!(events[0].new_content, original);
}

#[tokio::test]
async fn post_stream_refinement_noops_when_user_skips() {
    let h = TestHarness::new_with_collaborate(
        PhraseScript::Text("something".to_string()),
        RefineScript::Unused,
        InfoResponseScript::Skip,
    );

    let conv_id = uuid::Uuid::new_v4().to_string();
    let msg_id = uuid::Uuid::new_v4().to_string();
    let original = "Initial streamed answer.";
    let abstained_meta = serde_json::json!({ "grounding_gate": { "action": "abstained" } });
    let initial = Message {
        id: msg_id.clone(),
        conversation_id: conv_id.clone(),
        role: Role::Assistant,
        content: original.to_string(),
        created_at: 0,
        metadata: Some(abstained_meta.clone()),
        version: 0,
    };
    h.store.save_message(&initial).await.unwrap();

    let refined = h
        .runtime
        .apply_post_stream_refinement(&conv_id, &msg_id, "Q?", original, "E", Some(abstained_meta))
        .await;

    assert!(refined.is_none(), "skip → no-op");
    assert!(h
        .scripted_approval
        .as_ref()
        .unwrap()
        .refined_emissions()
        .is_empty());
}

// ─── Tier 2: conversation skill_id tagging ───────────────────

#[tokio::test]
async fn handle_message_tags_conversation_with_active_local_only_skill() {
    // End-to-end: construct a harness with a LocalOnly skill
    // activated; send a message; verify the conversation row
    // picks up `skill_id` automatically.
    let mut skills = sovereign_core::SkillRegistry::new();
    let skill_toml = r#"
[skill]
id = "inner-work"
name = "Inner Work"
version = "0.1.0"

[inference]
privacy = "local_only"
"#;
    skills.register(parse_skill_toml(skill_toml).unwrap());
    skills.activate("inner-work");

    let h = TestHarness::with_skills(skills);
    let conv_id = uuid::Uuid::new_v4().to_string();
    let _ = h
        .send_in("what does meaningful work look like for me?", &conv_id)
        .await;

    let conv = h.store.get_conversation(&conv_id).await.unwrap();
    assert_eq!(
        conv.skill_id.as_deref(),
        Some("inner-work"),
        "inner-work was active → conversation must be tagged with it \
         so the conversational KnowledgeView filter keeps it private"
    );
}

#[tokio::test]
async fn handle_message_leaves_skill_id_none_when_no_skill_active() {
    // Regression: the default harness (empty SkillRegistry) must
    // not tag conversations, so existing behaviour is preserved for
    // any caller that doesn't wire up skills.
    let h = TestHarness::new();
    let conv_id = uuid::Uuid::new_v4().to_string();
    let _ = h.send_in("hello", &conv_id).await;
    let conv = h.store.get_conversation(&conv_id).await.unwrap();
    assert!(
        conv.skill_id.is_none(),
        "no active skill → conversation stays untagged"
    );
}

#[tokio::test]
async fn handle_message_skill_id_is_first_writer_wins_across_turns() {
    // Even if the active skill changes mid-conversation, the tag
    // from the first message sticks. This matches the spec:
    // "skill active when this conversation started".
    let mut skills = sovereign_core::SkillRegistry::new();
    let toml_research = r#"
[skill]
id = "research-analyst"
name = "Research"
version = "0.1.0"

[inference]
privacy = "mesh_allowed"
"#;
    skills.register(parse_skill_toml(toml_research).unwrap());
    skills.activate("research-analyst");

    let h = TestHarness::with_skills(skills);
    let conv_id = uuid::Uuid::new_v4().to_string();
    let _ = h.send_in("first message", &conv_id).await;
    let first_tag = h.store.get_conversation(&conv_id).await.unwrap().skill_id;
    assert_eq!(first_tag.as_deref(), Some("research-analyst"));

    // Second turn in the same conversation. skill_id must not be
    // rewritten even if we later mutated the registry; the
    // first-writer-wins UPDATE clause (`WHERE skill_id IS NULL`)
    // guarantees it. (We can't easily mutate the harness's Arc'd
    // registry post-construction — the invariant is enforced at
    // the SQL layer anyway.)
    let _ = h.send_in("follow-up", &conv_id).await;
    let second_tag = h.store.get_conversation(&conv_id).await.unwrap().skill_id;
    assert_eq!(
        second_tag.as_deref(),
        Some("research-analyst"),
        "the skill tag must persist unchanged across turns"
    );
}

// ─── Compaction privacy invariant (marathon-graceful) ────────
//
// `conv_frame::fold` folds dropped chat history into the conversation
// frame that the prompt then re-injects. The privacy contract: that
// fold call MUST use `Speed::Fast` because
// `MeshInferenceProvider` only forwards `Speed::Slow` over the mesh
// — Fast stays local. If a future refactor accidentally bumps this
// to `Speed::Slow` (e.g. "the summary needs more model power"),
// local-only chat content would leak to whichever mesh peer the
// daemon happens to be routing to.
//
// The test installs a capturing `InferenceProvider` that records the
// first `complete` request and asserts its `preferred_speed`. Pure
// unit-shape; no harness needed.

#[tokio::test]
async fn conv_frame_fold_uses_fast_slot_only() {
    use async_trait::async_trait;
    use futures::Stream;
    use sovereign_core::error::{Error as CoreError, Result as CoreResult};
    use std::pin::Pin;
    use std::sync::Mutex;

    struct CapturingProvider {
        captured: Mutex<Option<CompletionRequest>>,
    }

    #[async_trait]
    impl InferenceProvider for CapturingProvider {
        async fn complete(&self, request: &CompletionRequest) -> CoreResult<CompletionResponse> {
            *self.captured.lock().unwrap() = Some(request.clone());
            // Return a valid JSON envelope so the parse path
            // succeeds and the function returns Ok(Some(_)). The
            // test inspects the *captured request*, not the
            // response.
            Ok(CompletionResponse {
                text: r#"{"summary": "captured"}"#.to_string(),
                tokens_used: 1,
                prompt_tokens: 0,
                model_id: "capturing".to_string(),
                latency_ms: 0,
                oicp_meta: None,
                finish_reason: None,
                completion_tokens: None,
            })
        }

        async fn complete_stream(
            &self,
            _request: &CompletionRequest,
        ) -> CoreResult<Pin<Box<dyn Stream<Item = CoreResult<String>> + Send>>> {
            Err(CoreError::NotImplemented("capturing".to_string()))
        }

        async fn embed(&self, _text: &str) -> CoreResult<Vec<f32>> {
            Err(CoreError::NotImplemented("capturing".to_string()))
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 2048,
                supports_structured_output: true,
                relative_speed: Speed::Fast,
                relative_reasoning: Depth::Shallow,
            }
        }
    }

    let provider = CapturingProvider {
        captured: Mutex::new(None),
    };
    let dropped = vec![
        Message {
            id: "m1".to_string(),
            conversation_id: "c1".to_string(),
            role: Role::User,
            content: "What's the photoelectric effect?".to_string(),
            created_at: 0,
            metadata: None,
            version: 0,
        },
        Message {
            id: "m2".to_string(),
            conversation_id: "c1".to_string(),
            role: Role::Assistant,
            content: "Einstein explained it in 1905.".to_string(),
            created_at: 1,
            metadata: None,
            version: 0,
        },
    ];

    let _ = sovereign_core::conv_frame::fold(&provider, None, &dropped, 0, dropped.len())
        .await
        .expect("fold should complete with the captured response");

    let captured = provider
        .captured
        .lock()
        .unwrap()
        .clone()
        .expect("conv_frame::fold must call inference.complete exactly once");

    // Compression runs on the fast slot — it's a cheap background
    // summary and the small model suffices. NOTE (post-OICP-refactor):
    // the fast slot is NOT what keeps this local anymore. Privacy is
    // now carried by the LocalOnly envelope — `offload_eligible`
    // forwards a turn only when `sharding == MeshAllowed && latency !=
    // Fast` (see sovereign-mesh/oicp_select.rs), so a LocalOnly turn
    // never crosses the mesh regardless of speed. Both are asserted:
    // the slot for cost, the posture for the local-only guarantee
    // (ARCH §7.4 defence-in-depth).
    assert!(
        matches!(captured.preferred_speed, Speed::Fast),
        "conv_frame::fold must use the fast slot, got {:?}",
        captured.preferred_speed
    );
    assert_eq!(
        captured
            .oicp
            .as_ref()
            .expect("compression must carry a Workload envelope")
            .sharding(),
        sovereign_core::oicp::ShardingPrivacy::LocalOnly,
        "conv_frame::fold must stay LocalOnly — dropped chat content must never offload",
    );
    // Sanity: structured_output must be set so the parse path
    // remains deterministic. (If a future refactor swaps in a
    // free-form summary, the parse logic needs a parallel change.)
    assert!(
        captured.structured_output.is_some(),
        "conv_frame::fold must request structured output"
    );
}

/// The two durable-memory-integrity guards — temporal-tension
/// classification and contradiction detection — must run on the
/// PRIMARY slot (they defend the durable store from corruption and a
/// stronger model is worth it on a background task where latency buys
/// nothing) YET must never leave the node (they read user-derived
/// memory content). Post-OICP-refactor those two requirements are
/// independent knobs: the ExtractDurable envelope makes them
/// primary-class (`Speed::Slow` shadow) while its LocalOnly posture
/// keeps `offload_eligible` false. This pins BOTH — a regression that
/// reverted the class to Fast, OR one that opened the posture to
/// MeshAllowed (leaking memory content over the mesh), fails here.
/// Sibling of `conv_frame_fold_uses_fast_slot_only`, which
/// pins the *opposite* choice for the cheap compression path.
#[tokio::test]
async fn memory_integrity_guards_route_primary_and_stay_local() {
    use async_trait::async_trait;
    use futures::Stream;
    use sovereign_core::error::{Error as CoreError, Result as CoreResult};
    use sovereign_core::oicp::ShardingPrivacy;
    use std::pin::Pin;
    use std::sync::Mutex;

    struct CapturingProvider {
        captured: Mutex<Option<CompletionRequest>>,
    }

    #[async_trait]
    impl InferenceProvider for CapturingProvider {
        async fn complete(&self, request: &CompletionRequest) -> CoreResult<CompletionResponse> {
            *self.captured.lock().unwrap() = Some(request.clone());
            // Empty JSON array satisfies both guards' parse paths
            // (tension classifications / contradiction indices) → each
            // returns Ok with no findings. The test inspects the
            // captured request, not the response.
            Ok(CompletionResponse {
                text: "[]".to_string(),
                tokens_used: 1,
                prompt_tokens: 0,
                model_id: "capturing".to_string(),
                latency_ms: 0,
                oicp_meta: None,
                finish_reason: None,
                completion_tokens: None,
            })
        }

        async fn complete_stream(
            &self,
            _request: &CompletionRequest,
        ) -> CoreResult<Pin<Box<dyn Stream<Item = CoreResult<String>> + Send>>> {
            Err(CoreError::NotImplemented("capturing".to_string()))
        }

        async fn embed(&self, _text: &str) -> CoreResult<Vec<f32>> {
            Err(CoreError::NotImplemented("capturing".to_string()))
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 2048,
                supports_structured_output: true,
                relative_speed: Speed::Fast,
                relative_reasoning: Depth::Shallow,
            }
        }
    }

    fn assert_primary_and_local(captured: &Mutex<Option<CompletionRequest>>, which: &str) {
        let req = captured
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| panic!("{which} must call inference.complete exactly once"));
        // Primary slot: the ExtractDurable bundle maps Normal latency →
        // the Slow shadow, which the local slot picker routes to primary.
        assert!(
            matches!(req.preferred_speed, Speed::Slow),
            "{which} must route to the primary slot (Speed::Slow), got {:?}",
            req.preferred_speed
        );
        // Local-only: user-derived memory content must never offload.
        // Under `offload_eligible` this is the load-bearing guarantee —
        // Slow speed alone would offload if the posture were MeshAllowed.
        assert_eq!(
            req.oicp
                .as_ref()
                .unwrap_or_else(|| panic!("{which} must carry a Workload envelope"))
                .sharding(),
            ShardingPrivacy::LocalOnly,
            "{which} must stay LocalOnly — memory content must never cross the mesh",
        );
    }

    let existing = Memory {
        id: "m1".to_string(),
        content: "The user's cat is named Whiskers.".to_string(),
        source: "test".to_string(),
        confidence: 1.0, // ≥ RELATIONAL_DIRECT_THRESHOLD (0.85) so it's a candidate
        created_at: 0,
        last_used: 0,
        version: 0,
        deleted_at: None,
        source_conversation_id: None,
        source_skill_id: None,
        ..Default::default()
    };

    // Guard 1 — temporal-tension classification.
    let p1 = CapturingProvider {
        captured: Mutex::new(None),
    };
    sovereign_core::memory::detect_temporal_tensions(
        &p1,
        "The user's cat is named Mittens.",
        std::slice::from_ref(&existing),
    )
    .await
    .expect("detect_temporal_tensions should complete");
    assert_primary_and_local(&p1.captured, "detect_temporal_tensions");

    // Guard 2 — contradiction detection.
    let new_mem = Memory {
        id: "m2".to_string(),
        content: "The user's cat is named Mittens.".to_string(),
        source: "test".to_string(),
        confidence: 1.0,
        created_at: 1,
        last_used: 1,
        version: 0,
        deleted_at: None,
        source_conversation_id: None,
        source_skill_id: None,
        ..Default::default()
    };
    let p2 = CapturingProvider {
        captured: Mutex::new(None),
    };
    sovereign_core::memory::detect_contradictions(&p2, &new_mem, std::slice::from_ref(&existing))
        .await
        .expect("detect_contradictions should complete");
    assert_primary_and_local(&p2.captured, "detect_contradictions");
}
