// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use corpus_engine::index::{EnrichmentChunkRow, InsertChunk, InsertCodeMeta};

fn ts(s: &str) -> NaiveDateTime {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
}

fn turn(ts_str: &str, is_user: bool, words: u64, chunk_id: u64, first_line: &str) -> Turn {
    Turn {
        ts: Some(ts(ts_str)),
        is_user,
        words,
        chunk_id,
        first_line: first_line.to_string(),
    }
}

fn doc(uuid: &str, turns: Vec<Turn>) -> ConvDoc {
    ConvDoc {
        conv_uuid: uuid.to_string(),
        title: Some(format!("title-{uuid}")),
        turns,
    }
}

// ─── fold_scale ──────────────────────────────────────────────────────

#[test]
fn scale_counts_words_months_and_dates() {
    let docs = vec![
        doc(
            "a",
            vec![
                turn("2025-01-10 09:00", true, 10, 1, "q"),
                turn("2025-01-10 09:01", false, 40, 1, "a"),
            ],
        ),
        doc("b", vec![turn("2025-03-02 22:00", true, 5, 2, "q")]),
    ];
    let c = fold_scale(&docs).unwrap();
    assert_eq!(c.conversations, 2);
    assert_eq!(c.months_active, 2); // 2025-01, 2025-03
    assert_eq!(c.words_user, 15);
    assert_eq!(c.words_assistant, 40);
    assert_eq!(c.words_total, 55);
    assert_eq!(c.first_date, "2025-01-10");
    assert_eq!(c.last_date, "2025-03-02");
    assert!(!c.derivation.is_empty());
}

#[test]
fn scale_absent_for_empty_corpus() {
    assert!(fold_scale(&[]).is_none());
}

// ─── fold_rhythm ─────────────────────────────────────────────────────

#[test]
fn rhythm_heatmap_buckets_by_utc_weekday_and_hour() {
    // 2025-01-06 is a Monday.
    let docs = vec![doc(
        "a",
        vec![
            turn("2025-01-06 02:10", true, 1, 1, "x"),
            turn("2025-01-06 02:30", false, 1, 1, "y"),
            turn("2025-01-12 23:59", true, 1, 2, "z"), // Sunday
        ],
    )];
    let c = fold_rhythm(&docs).unwrap();
    assert_eq!(c.heatmap[0][2], 2); // Monday 02:xx
    assert_eq!(c.heatmap[6][23], 1); // Sunday 23:xx
    assert_eq!(c.total_turns, 3);
}

#[test]
fn rhythm_absent_when_no_turn_is_timestamped() {
    let docs = vec![doc(
        "a",
        vec![Turn {
            ts: None,
            is_user: true,
            words: 3,
            chunk_id: 1,
            first_line: "hello".into(),
        }],
    )];
    assert!(fold_rhythm(&docs).is_none());
}

#[test]
fn longest_session_splits_on_gap_and_prefers_duration() {
    let docs = vec![
        // One conversation, two sessions: a 20-minute run, then (after a
        // 5h gap) a 61-minute run that should win.
        doc(
            "rabbit",
            vec![
                turn("2025-03-09 10:00", true, 1, 10, "short run start"),
                turn("2025-03-09 10:20", false, 1, 10, "short run end"),
                turn("2025-03-09 15:00", true, 1, 11, "long run start"),
                turn("2025-03-09 15:25", false, 1, 11, "mid"),
                turn("2025-03-09 15:50", true, 1, 12, "mid 2"),
                turn("2025-03-09 16:01", false, 1, 12, "long run end"),
            ],
        ),
        doc(
            "other",
            vec![
                turn("2025-04-01 08:00", true, 1, 20, "elsewhere"),
                turn("2025-04-01 08:30", false, 1, 20, "elsewhere end"),
            ],
        ),
    ];
    let s = fold_rhythm(&docs).unwrap().longest_session.unwrap();
    assert_eq!(s.conv_uuid, "rabbit");
    assert_eq!(s.date, "2025-03-09");
    assert_eq!(s.duration_minutes, 61);
    assert_eq!(s.turns, 4);
    assert_eq!(s.chunk_ids, vec![11, 12]);
    let e = s.excerpt.unwrap();
    assert_eq!(e.text, "long run start");
    assert_eq!(e.chunk_id, 11);
}

#[test]
fn longest_session_tie_breaks_on_turn_count() {
    let docs = vec![
        doc(
            "two-turns",
            vec![
                turn("2025-05-01 10:00", true, 1, 1, "a"),
                turn("2025-05-01 10:30", false, 1, 1, "b"),
            ],
        ),
        doc(
            "three-turns",
            vec![
                turn("2025-05-02 10:00", true, 1, 2, "a"),
                turn("2025-05-02 10:15", false, 1, 2, "b"),
                turn("2025-05-02 10:30", true, 1, 2, "c"),
            ],
        ),
    ];
    let s = fold_rhythm(&docs).unwrap().longest_session.unwrap();
    assert_eq!(s.conv_uuid, "three-turns");
}

#[test]
fn excerpt_prefix_respects_char_boundaries() {
    let line = "é".repeat(300); // 2 bytes per char; 240 is mid-char
    let out = excerpt_prefix(&line);
    assert!(out.len() <= 240);
    assert!(line.starts_with(&out)); // still a verbatim prefix
}

// ─── entity folds ────────────────────────────────────────────────────

fn erow(chunk_id: u64, text: &str, label: &str, score: f64, conv: &str) -> EntityRow {
    EntityRow {
        chunk_id,
        text: text.to_string(),
        label: label.to_string(),
        char_start: 0,
        char_end: text.len(),
        score,
        conv_uuid: Some(conv.to_string()),
    }
}

fn content_map(pairs: &[(u64, &str)]) -> HashMap<u64, String> {
    pairs.iter().map(|(id, c)| (*id, c.to_string())).collect()
}

fn no_generics() -> HashSet<String> {
    HashSet::new()
}

#[test]
fn obsessions_groups_by_quarter_and_ranks_by_conversations() {
    let rows = vec![
        erow(1, "Rust", "Work", 0.9, "c1"),
        erow(2, "Rust", "Work", 0.8, "c2"),
        erow(3, "Rust", "Work", 0.7, "c4"),
        erow(3, "Tokio", "Work", 0.7, "c1"), // 1 conv — below MIN_TOPIC_CONVS
        erow(4, "Berlin", "Location", 0.9, "c3"), // Q2 chunk
        erow(5, "Berlin", "Location", 0.8, "c5"),
        erow(6, "Berlin", "Location", 0.7, "c6"),
    ];
    let quarters: HashMap<u64, String> = [
        (1, "2025-Q1"),
        (2, "2025-Q1"),
        (3, "2025-Q1"),
        (4, "2025-Q2"),
        (5, "2025-Q2"),
        (6, "2025-Q2"),
    ]
    .into_iter()
    .map(|(k, v)| (k, v.to_string()))
    .collect();
    let content = content_map(&[
        (1, "I love Rust a lot"),
        (2, "more Rust here"),
        (3, "Rust and Tokio runtime question"),
        (4, "trip to Berlin"),
        (5, "Berlin again"),
        (6, "still Berlin"),
    ]);
    let c = fold_obsessions(&rows, &quarters, &content, &no_generics()).unwrap();
    assert_eq!(c.quarters.len(), 2);
    let q1 = &c.quarters[0];
    assert_eq!(q1.quarter, "2025-Q1");
    // Tokio (1 conversation) is no hook — only Rust (3 convs) survives.
    assert_eq!(q1.topics.len(), 1);
    assert_eq!(q1.topics[0].text, "Rust");
    assert_eq!(q1.topics[0].conversations, 3);
    // Sample cites the highest-score row and is verbatim.
    assert_eq!(q1.topics[0].sample.chunk_id, 1);
    assert_eq!(c.quarters[1].quarter, "2025-Q2");
    assert_eq!(c.quarters[1].topics[0].text, "Berlin");
}

#[test]
fn obsessions_drops_stoplist_low_score_and_non_verbatim_rows() {
    let rows = vec![
        erow(1, "you", "Person", 0.99, "c1"),  // stoplist floor
        erow(1, "Rust", "Work", 0.3, "c1"),    // below score floor
        erow(1, "Phantom", "Work", 0.9, "c1"), // not verbatim in chunk
        erow(1, "ok", "Work", 0.9, "c1"),      // too short
    ];
    let quarters: HashMap<u64, String> = [(1u64, "2025-Q1".to_string())].into_iter().collect();
    let content = content_map(&[(1, "talking about Rust with you, ok")]);
    assert!(fold_obsessions(&rows, &quarters, &content, &no_generics()).is_none());
}

#[test]
fn obsessions_drops_case_profile_generics() {
    // "workers" recurs across 3 convs but the generic set kills it.
    let rows = vec![
        erow(1, "Workers", "Person", 0.9, "c1"),
        erow(2, "Workers", "Person", 0.9, "c2"),
        erow(3, "Workers", "Person", 0.9, "c3"),
    ];
    let quarters: HashMap<u64, String> = [(1, "2025-Q1"), (2, "2025-Q1"), (3, "2025-Q1")]
        .into_iter()
        .map(|(k, v)| (k, v.to_string()))
        .collect();
    let content = content_map(&[
        (1, "Workers unite"),
        (2, "Workers again"),
        (3, "Workers third"),
    ]);
    let generic: HashSet<String> = ["workers".to_string()].into_iter().collect();
    assert!(fold_obsessions(&rows, &quarters, &content, &generic).is_none());
}

#[test]
fn cast_builds_cooccurrence_graph_with_degree() {
    let mut rows = Vec::new();
    // Alice + Acme co-occur in c1 and c2; Bob appears alone in c3.
    for conv in ["c1", "c2"] {
        rows.push(erow(1, "Alice", "Person", 0.9, conv));
        rows.push(erow(1, "Acme", "Organization", 0.9, conv));
    }
    rows.push(erow(1, "Bob", "Person", 0.9, "c3"));
    rows.push(erow(1, "Berlin", "Location", 0.9, "c1")); // label excluded
    let content = content_map(&[(1, "Alice from Acme met Bob in Berlin")]);
    let c = fold_cast(&rows, &content, &no_generics()).unwrap();
    assert_eq!(c.nodes.len(), 3); // Alice, Acme, Bob — no Location
    assert_eq!(c.edges.len(), 1);
    assert_eq!(c.edges[0].co_conversations, 2);
    let alice = c
        .nodes
        .iter()
        .find(|n| n.canonical_name == "Alice")
        .unwrap();
    assert_eq!(alice.conversations, 2);
    assert_eq!(alice.degree, 1);
    let bob = c.nodes.iter().find(|n| n.canonical_name == "Bob").unwrap();
    assert_eq!(bob.degree, 0);
}

#[test]
fn cast_absent_without_entity_rows() {
    assert!(fold_cast(&[], &HashMap::new(), &no_generics()).is_none());
}

// ─── case profile ────────────────────────────────────────────────────

fn texts(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn case_profile_counts_lowercase_and_mid_sentence_capitals() {
    let t = texts(&[
        "The workers met today. Workers were tired.",
        "I asked the workers about the workers union.",
    ]);
    let p = case_profile("workers", &t);
    // 3 lowercase; "Workers" follows ". " (sentence start) → neither side.
    assert_eq!(
        p,
        CaseProfile {
            lowercase: 3,
            capitalized_mid: 0
        }
    );
    assert!(p.is_generic());
}

#[test]
fn case_profile_keeps_proper_nouns_and_collision_names() {
    let t = texts(&[
        "I met Alice yesterday. Alice agreed.",
        "We fed the data in, then asked the Fed about rates; the Fed declined. It fed back.",
    ]);
    // "Alice": never lowercase → kept.
    assert!(!case_profile("alice", &t).is_generic());
    // "Fed"/"fed" collision: 2 lowercase (verb) vs 2 capitalized-mid →
    // share 0.5... lowercase=2 < MIN_EVIDENCE(3) → kept.
    let fed = case_profile("fed", &t);
    assert_eq!(fed.lowercase, 2);
    assert_eq!(fed.capitalized_mid, 2);
    assert!(!fed.is_generic());
}

#[test]
fn case_profile_respects_word_boundaries() {
    let t = texts(&["the userspace tools and the user model; a user, every user"]);
    let p = case_profile("user", &t);
    assert_eq!(p.lowercase, 3); // "userspace" must not count
}

#[test]
fn generic_keys_verdict_set() {
    let t = texts(&[
        "the banks failed and more banks failed; banks everywhere. Apple shipped; we like Apple and Apple again.",
    ]);
    let candidates: HashSet<String> = ["banks".to_string(), "apple".to_string()]
        .into_iter()
        .collect();
    let generic = generic_keys_by_case_profile(&candidates, &t);
    assert!(generic.contains("banks"));
    assert!(!generic.contains("apple"));
}

// ─── read_chunk_entities ─────────────────────────────────────────────

#[test]
fn chunk_entities_missing_db_degrades_to_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let rows = read_chunk_entities(&tmp.path().join("nope.db"), "x").unwrap();
    assert!(rows.is_empty());
}

#[test]
fn chunk_entities_missing_table_degrades_to_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("state.db");
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute("CREATE TABLE unrelated (x INTEGER)", [])
        .unwrap();
    let rows = read_chunk_entities(&db, "x").unwrap();
    assert!(rows.is_empty());
}

fn write_entities_fixture(db: &Path, corpus_id: &str, rows: &[(u64, &str, &str, f64, &str)]) {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute(
        "CREATE TABLE IF NOT EXISTS chunk_entities (
            corpus_id TEXT NOT NULL, chunk_id INTEGER NOT NULL,
            text TEXT NOT NULL, label TEXT NOT NULL,
            char_start INTEGER NOT NULL, char_end INTEGER NOT NULL,
            score REAL NOT NULL, conv_uuid TEXT, extracted_at INTEGER NOT NULL,
            PRIMARY KEY (corpus_id, chunk_id, text, label))",
        [],
    )
    .unwrap();
    for (chunk_id, text, label, score, conv) in rows {
        conn.execute(
            "INSERT INTO chunk_entities VALUES (?1, ?2, ?3, ?4, 0, 0, ?5, ?6, 0)",
            rusqlite::params![corpus_id, *chunk_id as i64, text, label, score, conv],
        )
        .unwrap();
    }
}

#[test]
fn chunk_entities_reads_only_the_requested_corpus() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("state.db");
    write_entities_fixture(&db, "mine", &[(7, "Rust", "Work", 0.9, "c1")]);
    write_entities_fixture(&db, "other", &[(8, "Go", "Work", 0.9, "c9")]);
    let rows = read_chunk_entities(&db, "mine").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].chunk_id, 7);
    assert_eq!(rows[0].text, "Rust");
    assert_eq!(rows[0].conv_uuid.as_deref(), Some("c1"));
}

// ─── build_conv_docs ─────────────────────────────────────────────────

fn chunk_row(
    id: u64,
    source_doc_id: &str,
    content: &str,
    summary: Option<&str>,
) -> EnrichmentChunkRow {
    EnrichmentChunkRow {
        id,
        content: content.to_string(),
        title: None,
        url: None,
        metadata_raw: summary.map(|s| format!(r#"{{"summary":{}}}"#, serde_json::json!(s))),
        source_doc_id: Some(source_doc_id.to_string()),
    }
}

const CONV_A: &str = "### [2025-01-06 02:10] user\n\nHow do lifetimes work in Rust?\n\n### [2025-01-06 02:12] assistant\n\nThey bound how long references live.";

#[test]
fn conv_docs_parse_turns_titles_and_word_counts() {
    let rows = vec![
        chunk_row(5, "conv-a", CONV_A, Some("Lifetimes deep dive")),
        chunk_row(
            6,
            "conv-a",
            "### [2025-01-06 02:40] user\n\nAnd borrows?",
            Some("Lifetimes deep dive"),
        ),
    ];
    let docs = build_conv_docs(&rows);
    assert_eq!(docs.len(), 1);
    let d = &docs[0];
    assert_eq!(d.conv_uuid, "conv-a");
    assert_eq!(d.title.as_deref(), Some("Lifetimes deep dive"));
    assert_eq!(d.turns.len(), 3);
    assert!(d.turns[0].is_user);
    assert_eq!(d.turns[0].words, 6); // "How do lifetimes work in Rust?"
    assert_eq!(d.turns[0].first_line, "How do lifetimes work in Rust?");
    assert_eq!(d.turns[2].chunk_id, 6);
}

// ─── end-to-end: build + audit + staleness over a real index ─────────

const EMBED_DIM: usize = 8;

fn insert_chunk(content: &str, source_doc_id: &str, summary: &str) -> InsertChunk {
    InsertChunk {
        content: content.into(),
        title: Some(summary.into()),
        url: None,
        metadata: Some(format!(r#"{{"summary":{}}}"#, serde_json::json!(summary))),
        content_hash: None,
        source_doc_id: Some(source_doc_id.into()),
        source_file: None,
        code: InsertCodeMeta::default(),
        unit_id: None,
    }
}

async fn build_fixture_index(path: &Path, corpus_id: &str) -> Vec<u64> {
    let index = CorpusIndex::create(
        path,
        corpus_id,
        "Wrapped Test",
        "test-model",
        EMBED_DIM,
        false,
        "MIT",
    )
    .await
    .expect("create index");
    let payload: Vec<_> = [
        (CONV_A, "conv-a", "Lifetimes deep dive"),
        (
            "### [2025-04-09 15:00] user\n\nPlan my Berlin trip with Alice.\n\n### [2025-04-09 15:20] assistant\n\nStart with the museum island.",
            "conv-b",
            "Berlin trip",
        ),
        // Same quarter as conv-a (2025-Q1): MIN_TOPIC_CONVS counts
        // distinct conversations PER QUARTER.
        (
            "### [2025-02-02 09:00] user\n\nMore Rust questions.\n\n### [2025-02-02 09:01] assistant\n\nRust rewards patience.",
            "conv-c",
            "Rust again",
        ),
        (
            "### [2025-03-03 09:00] user\n\nIs Rust right for this?\n\n### [2025-03-03 09:02] assistant\n\nFor this workload, Rust fits.",
            "conv-d",
            "Rust fit check",
        ),
    ]
    .iter()
    .enumerate()
    .map(|(i, (content, doc_id, summary))| {
        (
            insert_chunk(content, doc_id, summary),
            (0..EMBED_DIM).map(|j| i as f32 + j as f32 * 0.1).collect::<Vec<f32>>(),
        )
    })
    .collect();
    index.insert_batch(&payload).await.expect("insert_batch");
    let mut ids: Vec<u64> = index
        .all_chunks_full()
        .await
        .unwrap()
        .iter()
        .map(|r| r.id)
        .collect();
    ids.sort_unstable();
    ids
}

#[tokio::test]
async fn build_audit_cache_and_staleness_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let idx_dir = tmp.path().join("idx");
    let corpus_id = "wrapped-e2e";
    let ids = build_fixture_index(&idx_dir, corpus_id).await;

    // Entity rows cite real chunks with verbatim spans (+ one phantom
    // that the fold-time verbatim filter must drop). "Rust" spans three
    // conversations so it clears MIN_TOPIC_CONVS on the Obsessions card.
    let db = tmp.path().join("state.db");
    write_entities_fixture(
        &db,
        corpus_id,
        &[
            (ids[0], "Rust", "Work", 0.9, "conv-a"),
            (ids[2], "Rust", "Work", 0.85, "conv-c"),
            (ids[3], "Rust", "Work", 0.8, "conv-d"),
            (ids[1], "Alice", "Person", 0.9, "conv-b"),
            (ids[1], "Berlin", "Location", 0.9, "conv-b"),
            (ids[1], "Phantom", "Person", 0.9, "conv-b"),
        ],
    );

    let artifact = build_wrapped_artifact(&idx_dir, Some(&db)).await.unwrap();
    assert_eq!(artifact.schema_version, WRAPPED_SCHEMA_VERSION);
    assert_eq!(artifact.corpus_id, corpus_id);

    // Deck: scale + rhythm + obsessions + cast + door.
    let types: Vec<&str> = artifact
        .cards
        .iter()
        .map(|c| match c {
            WrappedCard::Scale(_) => "scale",
            WrappedCard::Rhythm(_) => "rhythm",
            WrappedCard::Obsessions(_) => "obsessions",
            WrappedCard::Cast(_) => "cast",
            WrappedCard::Door(_) => "door",
        })
        .collect();
    assert_eq!(types, vec!["scale", "rhythm", "obsessions", "cast", "door"]);

    // The audit passes against the live index.
    verify_wrapped_artifact(&artifact, &idx_dir).await.unwrap();

    // Cache landed and a fresh load returns it (same built_at).
    assert!(cache_path(&idx_dir).exists());
    let again = wrapped_artifact(&idx_dir, Some(&db)).await.unwrap();
    assert_eq!(again.built_at_unix, artifact.built_at_unix);
    assert_eq!(again.corpus_last_updated, artifact.corpus_last_updated);

    // Tampered excerpt → audit refuses.
    let mut tampered = artifact.clone();
    for card in &mut tampered.cards {
        if let WrappedCard::Rhythm(r) = card {
            if let Some(s) = &mut r.longest_session {
                s.excerpt = Some(Excerpt {
                    chunk_id: ids[0],
                    text: "never said this".into(),
                });
            }
        }
    }
    assert!(verify_wrapped_artifact(&tampered, &idx_dir).await.is_err());

    // Bogus citation → audit refuses.
    let mut bogus = artifact.clone();
    for card in &mut bogus.cards {
        if let WrappedCard::Cast(c) = card {
            c.nodes[0].sample.chunk_id = 999_999;
        }
    }
    assert!(verify_wrapped_artifact(&bogus, &idx_dir).await.is_err());

    // Staleness: bump `last_updated` in _corpus_meta.json → rebuild.
    let meta_path = idx_dir.join("_corpus_meta.json");
    let mut meta: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&meta_path).unwrap()).unwrap();
    let bumped = artifact.corpus_last_updated + 17;
    meta["last_updated"] = serde_json::json!(bumped);
    std::fs::write(&meta_path, serde_json::to_vec(&meta).unwrap()).unwrap();
    let rebuilt = wrapped_artifact(&idx_dir, Some(&db)).await.unwrap();
    assert_eq!(rebuilt.corpus_last_updated, bumped);
}

#[tokio::test]
async fn missing_state_db_yields_deck_without_entity_cards() {
    let tmp = tempfile::tempdir().unwrap();
    let idx_dir = tmp.path().join("idx");
    build_fixture_index(&idx_dir, "wrapped-nodb").await;
    let artifact = build_wrapped_artifact(&idx_dir, Some(&tmp.path().join("absent.db")))
        .await
        .unwrap();
    let has = |t: &str| {
        artifact.cards.iter().any(|c| match (c, t) {
            (WrappedCard::Obsessions(_), "obsessions") | (WrappedCard::Cast(_), "cast") => true,
            _ => false,
        })
    };
    assert!(!has("obsessions"));
    assert!(!has("cast"));
    assert!(matches!(artifact.cards.last(), Some(WrappedCard::Door(_))));
}
