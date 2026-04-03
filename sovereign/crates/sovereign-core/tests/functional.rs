mod harness;

use harness::TestHarness;
use sovereign_core::skills::parse_skill_toml;
use sovereign_core::traits::StateStore;
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
