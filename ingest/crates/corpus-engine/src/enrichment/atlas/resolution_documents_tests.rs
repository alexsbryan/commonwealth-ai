use super::*;

fn chunk(id: u64, doc: &str, content: &str) -> EnrichmentChunkRow {
    EnrichmentChunkRow {
        id,
        content: content.to_string(),
        title: None,
        url: None,
        metadata_raw: None,
        source_doc_id: Some(doc.to_string()),
    }
}

/// A document whose chunks run across two windows is read in parts: each
/// window's reader sees only its own chunks, and RESOLVE anchors a statement
/// of either part in the whole, the canonical text when the store holds it.
/// A document held whole by one window reads as before.
#[test]
fn a_document_across_windows_is_read_in_parts_and_anchored_whole() {
    let rows = [
        chunk(1, "novel", "Chapter one. The keeper lit the lamp."),
        chunk(2, "novel", "Chapter two. The ledger went missing."),
        chunk(3, "note", "A short note."),
    ];
    let ids1: &[u64] = &[1, 3];
    let ids2: &[u64] = &[2];
    let docs = SectionDocuments::from_chunk_rows([("w1", ids1), ("w2", ids2)], &rows);
    let first = docs
        .documents_for_section("w1")
        .iter()
        .find(|d| d.key() == "novel")
        .unwrap();
    let second = docs
        .documents_for_section("w2")
        .iter()
        .find(|d| d.key() == "novel")
        .unwrap();
    let note = docs
        .documents_for_section("w1")
        .iter()
        .find(|d| d.key() == "note")
        .unwrap();
    assert!(first.is_part() && second.is_part() && !note.is_part());
    assert_eq!(second.raw_body(), "Chapter two. The ledger went missing.");
    assert!(
        second.body().contains("The keeper lit the lamp.")
            && second.body().contains("The ledger went missing.")
    );

    let whole = "Chapter one. The keeper lit the lamp.\n\nChapter two. The ledger went missing.";
    let docs = docs.with_texts(&DocumentTexts::from([(
        "novel".to_string(),
        whole.to_string(),
    )]));
    let second = docs
        .documents_for_section("w2")
        .iter()
        .find(|d| d.key() == "novel")
        .unwrap();
    assert_eq!(
        second.raw_body(),
        "Chapter two. The ledger went missing.",
        "the reader reads its part"
    );
    assert_eq!(
        second.body(),
        fold_ws(whole),
        "RESOLVE anchors in the canonical text"
    );
    assert_eq!(second.stored_text(), Some(whole));
}
