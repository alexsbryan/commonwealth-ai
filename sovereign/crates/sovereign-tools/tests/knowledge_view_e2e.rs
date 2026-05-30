//! End-to-end tests for the `KnowledgeView` pipeline.
//!
//! Covers the full round-trip:
//!
//!   SqliteStateStore ← memories/conversations  (write path)
//!        │
//!        ▼
//!   SqliteAcquirer → rows.jsonl  (ingest)
//!        │
//!        ▼
//!   Jsonl extractor → Passthrough chunker → LanceDB  (index)
//!        │
//!        ▼
//!   FieldSkeleton planted on disk  (skips inference stub)
//!        │
//!        ▼
//!   KnowledgeViewManager::splice_into  (assemble digest)
//!        │
//!        ▼
//!   ConversationContext.knowledge_view_digests  (consumed by prompt)
//!
//! Inference is stubbed so enrichment prompts return deterministic
//! canned JSON when the tests exercise the enrichment path. Embedding
//! is a fixed-size zero vector — LanceDB stores it as opaque bytes,
//! and the splice path doesn't re-embed.

use std::path::PathBuf;
use std::sync::Arc;

use corpus_engine::enrichment::clustering::FieldModelStats;
use corpus_engine::enrichment::skeleton::{
    CanonicalQuestion, FieldSkeleton, SkeletonFaultLine, SkeletonOpenQuestion, SkeletonPosition,
};
use corpus_engine::recipe::AcquirerConfig;
use corpus_engine::{CorpusEngine, EmbedFn};
use sovereign_core::observer::SharedStateStoreObserver;
use sovereign_core::traits::{ConversationStore, MemoryStore};
use sovereign_core::types::{
    Conversation, ConversationContext, Memory, Message, Role,
};
use sovereign_store::sqlite::SqliteStateStore;
use sovereign_tools::knowledge_view::{
    conversation_history_recipe, KnowledgeViewManager, VIEW_PERSONAL_KNOWLEDGE,
};
use tempfile::TempDir;

/// Test-file-local mutex serialising the ingest-heavy tests in this
/// file. LanceDB's table writer exhibits intermittent errors when
/// two tests in the same process drive concurrent multi-view ingest
/// pipelines — even with separate TempDirs and CorpusEngines. The
/// race doesn't reproduce under `-- --test-threads=1` or when any
/// individual test is run in isolation; serialising here is a
/// pragmatic workaround that keeps the E2E coverage without
/// requiring a global test-runner flag. Held across the entire
/// test body, including the splice.
static INGEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

const EMBED_DIMS: usize = 768;

/// Deterministic embed stub: every call returns a fixed zero vector.
/// Adequate for the splice path, which never re-embeds.
fn stub_embed() -> EmbedFn {
    Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.0f32; EMBED_DIMS]) }))
}

/// Canned inference stub: inspects the prompt and returns JSON matching
/// the expected shape for each enrichment phase. Kept minimal — each
/// prompt method in `PersonalDomain` / `ConversationalDomain` has a
/// distinctive marker phrase.
fn stub_inference() -> corpus_engine::InferenceFn {
    Arc::new(|prompt: &str, _schema: Option<&serde_json::Value>| {
        let p = prompt.to_string();
        Box::pin(async move {
            if p.contains("semantically similar") || p.contains("cluster together") {
                Ok(r#"{"topic":"meaningful work","position_name":"Purpose-driven","is_argumentative":true,"is_objection":false,"is_open_question":false,"is_coherent":true}"#
                    .to_string())
            } else if p.contains("in tension") || p.contains("in dialogue") {
                Ok(r#"{"crux":"stability vs. autonomy","confidence":0.8,"resolution_condition":"lived clarity"}"#
                    .to_string())
            } else if p.contains("unresolved inquiry") || p.contains("returning to") {
                Ok(r#"{"question":"what kind of life do I want","why_unresolved":"value tensions"}"#
                    .to_string())
            } else if p.contains("[Memory 1]") || p.contains("[Conversation 1") {
                // Skeleton extraction — array of passage records.
                Ok(r#"[{"passage_index":0,"canonical_question":"what does meaningful work look like","question_type":"normative","positions":[{"name":"Purpose-driven","claim":"meaningful work serves others","status":"held","proponents":[]}]}]"#
                    .to_string())
            } else {
                Ok("{}".to_string())
            }
        })
    })
}

fn now() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn mem(id: &str, content: &str, conv_id: Option<&str>) -> Memory {
    Memory {
        id: id.to_string(),
        content: content.to_string(),
        source: "test".to_string(),
        confidence: 0.9,
        created_at: now(),
        last_used: now(),
        version: 0,
        deleted_at: None,
        source_conversation_id: conv_id.map(|s| s.to_string()),
        source_skill_id: None,
        ..Default::default()
    }
}

fn empty_context(conv_id: &str) -> ConversationContext {
    ConversationContext {
        conversation: Conversation {
            id: conv_id.to_string(),
            title: None,
            messages: vec![],
            created_at: now(),
            updated_at: now(),
            version: 0,
            deleted_at: None,
            skill_id: None,
            enabled_corpora: None,
        searched_sources: None,
        },
        memories: vec![],
        working_memory: None,
        installed_corpora: vec![],
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

/// Build a (CorpusEngine, temp dir, SQLite db path) suitable for
/// driving a full ingest pipeline with zero side effects on $HOME.
async fn boot_engine() -> (Arc<CorpusEngine>, TempDir, PathBuf) {
    let tmp = TempDir::new().expect("tempdir");
    let indexes_dir = tmp.path().join("indexes");
    let recipes_dir = tmp.path().join("recipes");
    let db_path = tmp.path().join("sovereign.db");
    std::fs::create_dir_all(&indexes_dir).unwrap();
    std::fs::create_dir_all(&recipes_dir).unwrap();

    let engine = Arc::new(
        CorpusEngine::new(recipes_dir, indexes_dir, stub_embed())
            .with_embedding_model("test-mock")
            .with_inference_fn(stub_inference()),
    );
    (engine, tmp, db_path)
}

/// Plant a canned FieldSkeleton on disk so `splice_into` can read it
/// without running the full 5-phase enrichment. The enrichment pipeline
/// has been verified separately in corpus-engine's own tests; here we
/// test the *splice* path specifically.
fn plant_skeleton(
    engine: &CorpusEngine,
    view_id: &str,
    domain_id: &str,
) -> FieldSkeleton {
    let skeleton = FieldSkeleton {
        schema_version: 1,
        corpus_id: view_id.to_string(),
        generated_at: "2026-04-20T00:00:00Z".into(),
        extraction_method: "e2e_fixture".into(),
        prompt_version: "v1".into(),
        domain_id: domain_id.to_string(),
        canonical_questions: vec![CanonicalQuestion {
            id: "q1".into(),
            question: "What does meaningful work look like for me?".into(),
            status: "contested".into(),
            question_type: "normative".into(),
            primary_entries: vec![],
            positions: vec![SkeletonPosition {
                id: "p1".into(),
                name: "Purpose-driven".into(),
                claim: "meaningful work serves others".into(),
                status: "held".into(),
                proponents: vec![],
                source: "skeleton".into(),
                cluster_ids: vec![],
                centroid_chunk_ids: vec![],
                discovery_confidence: None,
            }],
            fault_lines: vec![SkeletonFaultLine {
                id: "f1".into(),
                between_positions: vec!["p1".into(), "p2".into()],
                crux: "stability vs. autonomy".into(),
                key_chunk_ids: vec![],
                confidence: 0.8,
                source: "detected".into(),
                resolution_condition: None,
            }],
        }],
        open_questions: vec![SkeletonOpenQuestion {
            id: "oq1".into(),
            question: "what kind of life do I actually want".into(),
            status: "open".into(),
            related_question_id: Some("q1".into()),
            representative_chunk_ids: vec![],
        }],
        field_stats: FieldModelStats::default(),
    };

    // Open the index from within the current tokio runtime so
    // LanceDB's async calls can drive to completion. The helper is
    // invoked from an async test context, so `block_in_place` +
    // `handle.block_on` is the documented tokio idiom for re-
    // entering the runtime from a sync-looking helper.
    let view = view_id.to_string();
    let skeleton_clone = skeleton.clone();
    let engine_ref = engine;
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async move {
            let index = engine_ref
                .open_index_for_corpus(&view)
                .await
                .expect("open index after ingest");
            index
                .write_field_skeleton(&skeleton_clone)
                .expect("write skeleton");
        });
    });
    skeleton
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn personal_view_ingest_plus_planted_skeleton_splices_into_context() {
    let _serialise = INGEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (engine, _tmp, db_path) = boot_engine().await;

    // Seed a few memories so the acquirer has rows to emit.
    let store = Arc::new(SqliteStateStore::open(&db_path).expect("open store"));
    store.save_memory(&mem("m1", "I keep coming back to the question of meaningful work.", None))
        .await
        .unwrap();
    store.save_memory(&mem("m2", "I value simplicity — but I keep designing complex systems.", None))
        .await
        .unwrap();
    store.save_memory(&mem("m3", "My work matters when it serves others.", None))
        .await
        .unwrap();

    // Manager registers the SQLite acquirer on the engine and builds
    // its recipes. `inner-work` is excluded from the conversational
    // view but doesn't affect this personal-knowledge test.
    let manager = Arc::new(
        KnowledgeViewManager::new(
            engine.clone(),
            stub_inference(),
            db_path.clone(),
            vec!["inner-work".into()],
        )
        .await,
    );
    // Wire the manager as the store's observer for parity with
    // production; we don't actually need debounced enrichment here.
    store.set_observer(manager.clone() as SharedStateStoreObserver);

    // Run initial ingest — acquirer materialises memories → JSONL,
    // extractor parses, passthrough chunker emits one chunk per
    // memory, embedder runs the zero-vector stub, LanceDB writes.
    manager.init().await.expect("init ingests empty views");

    // Plant a skeleton directly on disk so the splice path has
    // something to read. (Full enrichment with the inference stub is
    // validated elsewhere; here we are testing the splice.)
    plant_skeleton(&engine, VIEW_PERSONAL_KNOWLEDGE, "personal");

    // Assemble a context and splice.
    let mut ctx = empty_context("c1");
    assert!(ctx.knowledge_view_digests.is_none());
    manager.splice_into(&mut ctx, None).await;

    // Invariant: splice always sets Some(_), per the plan.
    let digests = ctx
        .knowledge_view_digests
        .as_ref()
        .expect("splice populates the field");
    let personal = digests
        .iter()
        .find(|d| d.view_id == VIEW_PERSONAL_KNOWLEDGE)
        .expect("personal-knowledge digest present");

    assert!(
        personal.body.contains("What does meaningful work"),
        "canonical question surfaced: body={}",
        personal.body
    );
    assert!(
        personal.body.contains("stability vs. autonomy"),
        "fault-line crux surfaced"
    );
    assert!(
        personal.body.contains("what kind of life"),
        "open question surfaced"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conversational_view_excludes_local_only_skill_conversations() {
    // Tests the privacy-separation invariant at its most direct
    // surface: the SQL query the conversation-history recipe
    // actually emits, run against a DB that contains one
    // inner-work conversation and one research-analyst
    // conversation. Whatever the rest of the pipeline does, the
    // query itself must filter inner-work out.
    //
    // Verifying via SQL rather than the materialised JSONL path
    // avoids coupling to acquirer implementation details (tmp-dir
    // layout, cache cleanup, etc.) — those are covered by the
    // acquirer's own tests in `acquirers::sqlite::tests`.
    let (_engine, _tmp, db_path) = boot_engine().await;
    let store = Arc::new(SqliteStateStore::open(&db_path).expect("open store"));

    let inner_work_id = "conv-private";
    let research_id = "conv-public";

    store
        .save_message(&Message {
            id: "msg-iw-1".into(),
            conversation_id: inner_work_id.to_string(),
            role: Role::User,
            content: "private inner-work thoughts".into(),
            created_at: now(),
            metadata: None,
            version: 0,
        })
        .await
        .unwrap();
    store
        .save_message(&Message {
            id: "msg-ra-1".into(),
            conversation_id: research_id.to_string(),
            role: Role::User,
            content: "public research analysis of oil markets".into(),
            created_at: now(),
            metadata: None,
            version: 0,
        })
        .await
        .unwrap();

    // Tag each conversation with its skill. `save_message` doesn't
    // set `skill_id` today — that's a Tier 2 follow-up. For the
    // privacy filter to be testable, we set it directly.
    {
        let conn = store.connection();
        let conn = conn.lock().await;
        conn.execute(
            "UPDATE conversations SET skill_id = 'inner-work' WHERE id = ?1",
            rusqlite::params![inner_work_id],
        )
        .unwrap();
        conn.execute(
            "UPDATE conversations SET skill_id = 'research-analyst' WHERE id = ?1",
            rusqlite::params![research_id],
        )
        .unwrap();
    }

    // Pull the exact SQL the recipe emits and execute it. This is
    // the query `SqliteAcquirer` will run at ingest time — testing
    // it in isolation pins the filter's behaviour regardless of
    // the pipeline around it.
    let recipe = conversation_history_recipe(&db_path, &["inner-work"]);
    let query = match recipe.acquire {
        AcquirerConfig::Custom { ref params, .. } => params["query"]
            .as_str()
            .expect("recipe query is a string")
            .to_string(),
        _ => panic!("recipe unexpectedly not a Custom acquirer"),
    };

    let mut rows_contents: Vec<String> = Vec::new();
    {
        let conn = store.connection();
        let conn = conn.lock().await;
        let mut stmt = conn.prepare(&query).expect("prepare recipe query");
        let mapped = stmt
            .query_map([], |row| {
                let content: String = row.get("content")?;
                Ok(content)
            })
            .expect("execute recipe query");
        for row in mapped {
            rows_contents.push(row.unwrap());
        }
    }

    let combined = rows_contents.join("\n");
    assert!(
        combined.contains("public research analysis"),
        "research-analyst conversation must appear: {combined}"
    );
    assert!(
        !combined.contains("private inner-work"),
        "inner-work conversation must be excluded: {combined}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn splice_without_any_enrichment_still_sets_some_empty_vec() {
    // Invariant from the plan: splice_into always sets the field to
    // `Some(_)` so downstream callers can rely on it being non-None,
    // even if no view has produced a digest yet. This guards
    // against the silent "unrouted context" privacy bug the plan
    // called out as the biggest risk.
    let (engine, _tmp, db_path) = boot_engine().await;
    let manager = Arc::new(
        KnowledgeViewManager::new(
            engine,
            stub_inference(),
            db_path,
            vec!["inner-work".into()],
        )
        .await,
    );
    // Deliberately skip init() — no ingest, no enrichment, no
    // skeleton. splice_into must still produce `Some(_)`.
    let mut ctx = empty_context("c1");
    manager.splice_into(&mut ctx, None).await;
    let digests = ctx
        .knowledge_view_digests
        .expect("splice guarantees Some(_) even with no enriched views");
    // Bodies may be empty strings (the fallback) — the invariant is
    // only that the outer Option is not None.
    assert!(digests.len() <= 2);
}

/// Semantically-aware embed stub for the cross-view E2E: maps
/// specific texts to vectors that encode their "topic". Identical
/// or near-identical topics produce vectors with cosine similarity
/// well above the 0.75 match threshold; unrelated topics embed
/// orthogonally.
///
/// Keep the mapping small + explicit — the test's whole point is
/// that semantically-similar items match across views.
fn semantic_embed_stub() -> EmbedFn {
    Arc::new(|text: &str| {
        let t = text.to_lowercase();
        let v = if t.contains("meaningful work") || t.contains("purpose") {
            // "axis 0" — the work/purpose cluster.
            vec![1.0f32, 0.05, 0.05]
        } else if t.contains("autonomy") || t.contains("governance") {
            // "axis 1" — the autonomy/governance cluster.
            vec![0.05f32, 1.0, 0.05]
        } else if t.contains("weather") || t.contains("cooking") {
            // "axis 2" — an unrelated topic that must not match.
            vec![0.05f32, 0.05, 1.0]
        } else {
            // Default: small off-axis vector so unmatched queries
            // don't accidentally land on a cluster.
            vec![0.3f32, 0.3, 0.3]
        };
        Box::pin(async move { Ok(v) })
    })
}

/// Plant a field_skeleton.json for `view_id`. Resilient to
/// partition-vs-canonical layout: the CorpusEngine's solo-ingest
/// finalise typically promotes `<view>-partition-local` to
/// `<view>`, but if another corpus is promoting concurrently the
/// finalise may defer and leave the partition path in place.
/// Plant into whichever directory actually holds committed data.
fn plant_skeleton_into(engine: &CorpusEngine, view_id: &str, skeleton: &FieldSkeleton) {
    let canonical = engine.index_dir().join(view_id);
    let partition = engine.partition_path(view_id);
    let target = if canonical.exists() {
        canonical
    } else if partition.exists() {
        partition
    } else {
        panic!(
            "no index directory for view '{view_id}' — canonical={} partition={}",
            engine.index_dir().join(view_id).display(),
            engine.partition_path(view_id).display()
        );
    };
    let view = view_id.to_string();
    let target = target.clone();
    let skeleton_clone = skeleton.clone();
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async move {
            let index = engine.open_index(&target).await.unwrap_or_else(|e| {
                panic!("open index at {}: {e}", target.display())
            });
            index
                .write_field_skeleton(&skeleton_clone)
                .unwrap_or_else(|e| panic!("write skeleton for {view}: {e}"));
        });
    });
}

fn plant_personal_skeleton(engine: &CorpusEngine) {
    let skeleton = FieldSkeleton {
        schema_version: 1,
        corpus_id: VIEW_PERSONAL_KNOWLEDGE.into(),
        generated_at: "2026-04-20T00:00:00Z".into(),
        extraction_method: "cross-view-e2e".into(),
        prompt_version: "v1".into(),
        domain_id: "personal".into(),
        canonical_questions: vec![
            CanonicalQuestion {
                id: "p-q1".into(),
                question: "What does meaningful work look like for me?".into(),
                status: "contested".into(),
                question_type: "normative".into(),
                primary_entries: vec![],
                positions: vec![],
                fault_lines: vec![],
            },
            CanonicalQuestion {
                id: "p-q2".into(),
                question: "My relationship to autonomy".into(),
                status: "held".into(),
                question_type: "normative".into(),
                primary_entries: vec![],
                positions: vec![],
                fault_lines: vec![],
            },
        ],
        open_questions: vec![],
        field_stats: FieldModelStats::default(),
    };
    plant_skeleton_into(engine, VIEW_PERSONAL_KNOWLEDGE, &skeleton);
}

fn plant_conversation_skeleton(engine: &CorpusEngine) {
    let skeleton = FieldSkeleton {
        schema_version: 1,
        corpus_id: "conversation-history".into(),
        generated_at: "2026-04-20T00:00:00Z".into(),
        extraction_method: "cross-view-e2e".into(),
        prompt_version: "v1".into(),
        domain_id: "conversational".into(),
        canonical_questions: vec![CanonicalQuestion {
            id: "c-q1".into(),
            question: "What's the purpose of this project?".into(),
            status: "held".into(),
            question_type: "conceptual".into(),
            primary_entries: vec![],
            positions: vec![],
            fault_lines: vec![],
        }],
        open_questions: vec![SkeletonOpenQuestion {
            id: "c-oq1".into(),
            question: "How should governance handle user autonomy?".into(),
            status: "open".into(),
            related_question_id: None,
            representative_chunk_ids: vec![],
        }],
        field_stats: FieldModelStats::default(),
    };
    plant_skeleton_into(engine, "conversation-history", &skeleton);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cross_view_digest_surfaces_resonance_across_personal_and_conversational() {
    let _serialise = INGEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    use sovereign_tools::knowledge_view::VIEW_CROSS_VIEW;

    // Builds two views with planted skeletons that share a
    // semantic axis ("meaningful work" ↔ "purpose of this project"
    // + autonomy), runs `splice_into`, and asserts the cross-view
    // digest surfaces at least one tentative match with the right
    // framing.
    let tmp = TempDir::new().unwrap();
    let indexes_dir = tmp.path().join("indexes");
    let recipes_dir = tmp.path().join("recipes");
    let db_path = tmp.path().join("sovereign.db");
    let notes_path = tmp.path().join("notes.db");
    std::fs::create_dir_all(&indexes_dir).unwrap();
    std::fs::create_dir_all(&recipes_dir).unwrap();
    // Touch the notes DB so institutional ingest doesn't error on
    // startup — it'll still produce no results and be skipped from
    // the cross-view computation.
    let _ = std::fs::File::create(&notes_path).unwrap();

    let engine = Arc::new(
        CorpusEngine::new(recipes_dir, indexes_dir, semantic_embed_stub())
            .with_embedding_model("test-mock")
            .with_inference_fn(stub_inference()),
    );

    // Seed memories + a conversation so personal + conversational
    // both ingest non-empty content.
    let store = Arc::new(SqliteStateStore::open(&db_path).unwrap());
    store
        .save_memory(&mem("m1", "I've been reflecting on meaningful work", None))
        .await
        .unwrap();
    store
        .save_message(&Message {
            id: "msg1".into(),
            conversation_id: "c-seed".into(),
            role: Role::User,
            content: "thinking about the purpose of this project".into(),
            created_at: now(),
            metadata: None,
            version: 0,
        })
        .await
        .unwrap();

    let manager = Arc::new(
        KnowledgeViewManager::new_with_notes_path(
            engine.clone(),
            stub_inference(),
            db_path.clone(),
            notes_path.clone(),
            vec![],
        )
        .await,
    );
    manager.init().await.expect("ingest views");

    // Plant skeletons that share a "purpose / meaningful work"
    // axis and an "autonomy" axis across two different views.
    plant_personal_skeleton(&engine);
    plant_conversation_skeleton(&engine);

    let mut ctx = empty_context("c-cross-view");
    manager.splice_into(&mut ctx, None).await;
    let digests = ctx.knowledge_view_digests.expect("splice populates");
    let cross = digests
        .iter()
        .find(|d| d.view_id == VIEW_CROSS_VIEW)
        .unwrap_or_else(|| {
            panic!(
                "cross-view digest missing; digests present: {:?}",
                digests.iter().map(|d| &d.view_id).collect::<Vec<_>>()
            )
        });
    assert!(
        cross.body.contains("Cross-view connections"),
        "body has section header: {}",
        cross.body
    );
    assert!(
        cross.body.contains("may resonate with"),
        "body uses tentative framing: {}",
        cross.body
    );
    // The match that must be present: "meaningful work" in
    // personal resonates with "purpose" in conversations, OR the
    // autonomy theme matches across both views.
    let has_work_purpose = cross.body.contains("meaningful work")
        && cross.body.contains("purpose of this project");
    let has_autonomy = cross.body.contains("autonomy");
    assert!(
        has_work_purpose || has_autonomy,
        "expected at least one semantic match to surface: {}",
        cross.body
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cross_view_digest_suppressed_under_local_only_skill() {
    use sovereign_tools::knowledge_view::VIEW_CROSS_VIEW;

    // When `inner-work` is the active skill, conversational and
    // institutional are suppressed → cross-view has no peers to
    // match against → the cross-view digest must not appear at all.
    let tmp = TempDir::new().unwrap();
    let indexes_dir = tmp.path().join("indexes");
    let recipes_dir = tmp.path().join("recipes");
    let db_path = tmp.path().join("sovereign.db");
    let notes_path = tmp.path().join("notes.db");
    std::fs::create_dir_all(&indexes_dir).unwrap();
    std::fs::create_dir_all(&recipes_dir).unwrap();
    let _ = std::fs::File::create(&db_path).unwrap();
    let _ = std::fs::File::create(&notes_path).unwrap();

    let engine = Arc::new(
        CorpusEngine::new(recipes_dir, indexes_dir, semantic_embed_stub())
            .with_embedding_model("test-mock")
            .with_inference_fn(stub_inference()),
    );
    let manager = Arc::new(
        KnowledgeViewManager::new_with_notes_path(
            engine,
            stub_inference(),
            db_path,
            notes_path,
            vec!["inner-work".into()],
        )
        .await,
    );

    let mut ctx = empty_context("c-private");
    manager.splice_into(&mut ctx, Some("inner-work")).await;
    let digests = ctx.knowledge_view_digests.unwrap();
    assert!(
        !digests.iter().any(|d| d.view_id == VIEW_CROSS_VIEW),
        "cross-view digest must be absent under local_only active skill"
    );
}
