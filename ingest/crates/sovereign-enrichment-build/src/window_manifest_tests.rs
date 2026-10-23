use super::{build_window_manifest, window_chars, WINDOW_CHARS};
use corpus_index::index::EnrichmentChunkRow;

fn row(id: u64, doc: &str, date: Option<&str>, chars: usize) -> EnrichmentChunkRow {
    EnrichmentChunkRow {
        id,
        content: "x".repeat(chars),
        title: Some(format!("subject of {doc}")),
        url: None,
        metadata_raw: date.map(|d| serde_json::json!({ "date": d }).to_string()),
        source_doc_id: Some(doc.to_string()),
    }
}

fn windows(m: &corpus_engine::enrichment::pipeline::ChapterManifest) -> Vec<Vec<u64>> {
    m.chapters.iter().map(|c| c.chunk_ids.clone()).collect()
}

/// Failing input, named: chunk order (ingestion order, which follows a
/// maildir's folders) puts `late` before `early`; the clock puts it after,
/// and the undated document after every dated one.
#[test]
fn documents_go_in_clock_order_with_undated_last() {
    let rows = vec![
        row(1, "late", Some("Tue, 15 May 2001 09:00:00 -0700"), 10),
        row(2, "undated", None, 10),
        row(
            3,
            "early",
            Some("Mon, 14 May 2001 16:39:00 -0700 (PDT)"),
            10,
        ),
    ];
    let m = build_window_manifest("c", rows, &Default::default(), Some("date"), 1_000);
    assert_eq!(windows(&m), vec![vec![3, 1, 2]]);
    assert_eq!(m.chapters[0].metadata["window_documents"], "3");
}

/// A window is cut only where a document ends. Failing input, named: cut by
/// chunk, `b`'s first chunk would join `a` (80 of 100) and its second open the
/// next window; cut by document, `b` opens it whole. A single chunk over the
/// cap stands alone.
#[test]
fn a_window_is_cut_only_between_documents() {
    let rows = vec![
        row(1, "a", Some("2001-05-01"), 40),
        row(2, "b", Some("2001-05-02"), 40),
        row(3, "b", None, 40),
        row(4, "c", Some("2001-05-03"), 300),
        row(5, "d", Some("2001-05-04"), 10),
    ];
    let m = build_window_manifest("c", rows, &Default::default(), Some("date"), 100);
    assert_eq!(windows(&m), vec![vec![1], vec![2, 3], vec![4], vec![5]]);
    let ids: Vec<&str> = m.chapters.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["sec_00001", "sec_00002", "sec_00003", "sec_00004"]);
}

/// With no date declared every document is undated, in chunk order.
#[test]
fn no_declared_date_keeps_chunk_order() {
    let rows = vec![
        row(2, "b", Some("2001-05-01"), 10),
        row(1, "a", Some("2001-05-02"), 10),
    ];
    let m = build_window_manifest("c", rows, &Default::default(), None, 1_000);
    assert_eq!(windows(&m), vec![vec![1, 2]]);
}

/// The reader reads the stored text, not the chunks: a document whose chunks
/// are short but whose stored text passes the cap opens a window of its own.
#[test]
fn a_window_is_sized_by_the_stored_text_the_reader_reads() {
    let rows = vec![
        row(1, "a", Some("2001-05-01"), 10),
        row(2, "b", Some("2001-05-02"), 10),
    ];
    let texts = [("b".to_string(), "y".repeat(500))].into_iter().collect();
    let m = build_window_manifest("c", rows.clone(), &Default::default(), Some("date"), 100);
    assert_eq!(
        windows(&m),
        vec![vec![1, 2]],
        "control: by chunks they fit one window"
    );
    let m = build_window_manifest("c", rows, &texts, Some("date"), 100);
    assert_eq!(windows(&m), vec![vec![1], vec![2]]);
}

/// A document longer than a window is cut where the recipe's chunker cut it:
/// consecutive parts of whole chunks, each placed as a document is. Failing
/// input, named: saltgrass, one 45k-character novel in 31 chunks, was one
/// window, so the window reader read it as a one-shot (2026-10-14).
#[test]
fn a_document_longer_than_a_window_is_cut_at_its_chunks() {
    let rows = vec![
        row(1, "a", None, 10),
        row(2, "novel", None, 40),
        row(3, "novel", None, 40),
        row(4, "novel", None, 40),
        row(5, "novel", None, 40),
        row(6, "novel", None, 40),
        row(7, "b", None, 10),
    ];
    let m = build_window_manifest("c", rows, &Default::default(), None, 100);
    assert_eq!(windows(&m), vec![vec![1, 2, 3], vec![4, 5], vec![6, 7]]);
}

/// The window size is the recipe's `window_chars` when it sets one, the
/// shipped [`WINDOW_CHARS`] when it does not.
#[test]
fn the_window_size_is_the_recipes_when_it_sets_one() {
    let mut p = corpus_engine::enrichment::ontology::OntologyPolicies::default();
    assert_eq!(window_chars(&p), WINDOW_CHARS);
    p.shape.window_chars = Some(30_000);
    assert_eq!(window_chars(&p), 30_000);
}
