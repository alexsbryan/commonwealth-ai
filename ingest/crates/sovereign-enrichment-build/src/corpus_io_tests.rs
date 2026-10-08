use super::*;
use corpus_engine::enrichment::pipeline::{ChapterEntry, ChapterManifest};

#[test]
fn corpus_hydration_keeps_same_title_text_documents_and_typed_context_distinct() {
    let body = "Issue 842 was discussed by a maintainer.";
    let rows = vec![
        corpus_index::index::EnrichmentChunkRow {
            id: 41,
            content: body.into(),
            title: Some("Same thread title".into()),
            url: Some("https://example.test/doc-a".into()),
            metadata_raw: Some(
                r#"{"author":"Alice","role":"member","date":"2026-02-03","kind":"comment","id":"raw-a","ordinal":3}"#.into(),
            ),
            source_doc_id: Some("doc-a".into()),
        },
        corpus_index::index::EnrichmentChunkRow {
            id: 42,
            content: body.into(),
            title: Some("Same thread title".into()),
            url: Some("https://example.test/doc-b".into()),
            metadata_raw: Some(
                r#"{"author":"Bob","role":"none","date":"2026-02-04","kind":"event","id":"raw-b","ordinal":4}"#.into(),
            ),
            source_doc_id: Some("doc-b".into()),
        },
    ];
    let manifest = ChapterManifest {
        corpus_id: "cases".into(),
        schema_version: ChapterManifest::SCHEMA_VERSION,
        chapters: vec![ChapterEntry {
            id: "sec_0001".into(),
            title: "Same thread title".into(),
            part: None,
            chapter: None,
            first_line: "Same thread title".into(),
            word_count: 12,
            chunk_ids: vec![41, 42],
            characters_present: Vec::new(),
            metadata: Default::default(),
        }],
    };

    let inputs = hydrate_corpus_chapters_from_rows(&manifest, &rows);
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0].text, format!("{body}\n\n{body}"));
    assert_eq!(inputs[0].source_documents.len(), 2);
    let [alice, bob] = inputs[0].source_documents.as_slice() else {
        panic!("each distinct source_doc_id must remain a separate document")
    };
    assert_eq!(alice.key(), "doc-a");
    assert_eq!(bob.key(), "doc-b");
    assert_eq!(alice.title(), Some("Same thread title"));
    assert_eq!(bob.title(), Some("Same thread title"));
    assert_eq!(alice.url(), Some("https://example.test/doc-a"));
    assert_eq!(bob.url(), Some("https://example.test/doc-b"));
    assert_eq!(alice.metadata()["author"], "Alice");
    assert_eq!(bob.metadata()["author"], "Bob");
    assert_eq!(alice.metadata()["role"], "member");
    assert_eq!(bob.metadata()["role"], "none");
    assert_eq!(alice.metadata()["date"], "2026-02-03");
    assert_eq!(bob.metadata()["kind"], "event");
    assert_eq!(alice.metadata()["ordinal"], 3);
    assert_eq!(alice.raw_body(), body);
    assert_eq!(bob.raw_body(), body);
}
