// SPDX-License-Identifier: AGPL-3.0-or-later
//! The text store's own laws: one writer, named refusals, a set of records,
//! a digest that moves only with texts, sources and extractors.

use super::*;
use crate::index::InsertChunk;

const DIM: usize = 4;

async fn fresh(dir: &Path, id: &str) -> CorpusIndex {
    CorpusIndex::create(&dir.join(id), id, id, "m", DIM, true, "MIT")
        .await
        .unwrap()
}

fn chunk(content: &str, text: Option<Sha256Hash>) -> (InsertChunk, Vec<f32>) {
    (
        InsertChunk {
            content: content.into(),
            title: None,
            url: None,
            metadata: None,
            content_hash: Some(kernel_types::ContentHash::of_str(content).to_hex()),
            source_doc_id: Some("doc".into()),
            source_file: None,
            code: Default::default(),
            unit_id: None,
            text_sha256: text,
        },
        vec![0.1; DIM],
    )
}

fn input<'a>(
    text: &'a str,
    source_id: &'a str,
    ordinal: u32,
    source: &'a DocSource,
) -> DocumentInput<'a> {
    DocumentInput {
        text,
        source_id,
        ordinal,
        source,
        metadata: None,
    }
}

#[tokio::test]
async fn a_stored_text_reads_back_by_its_name_with_its_record() {
    let dir = tempfile::tempdir().unwrap();
    let idx = fresh(dir.path(), "c").await;
    let file = dir.path().join("a.txt");
    std::fs::write(&file, b"raw source bytes").unwrap();
    let mut w = TextWriter::open(&idx, "plaintext@0.8.0", true)
        .await
        .unwrap();
    assert!(w.is_active(), "an empty index begins a store");

    let meta = serde_json::json!({"k": "v"});
    let source = DocSource::File(file.clone());
    let mut doc = input("the text", "a.txt", 0, &source);
    doc.metadata = Some(&meta);
    let name = w.store_document(doc).unwrap().expect("named");
    assert_eq!(name, Sha256Hash::of_str("the text"));
    assert_eq!(
        std::fs::read_to_string(Corpus::texts_in(idx.path()).join(name.to_hex())).unwrap(),
        "the text",
        "texts/<sha256> holds the bytes"
    );
    assert_eq!(w.flush(&idx).await.unwrap(), 1);

    let got = idx.text(&name).await.unwrap().expect("held");
    assert_eq!(got.text, "the text");
    assert_eq!(got.documents.len(), 1);
    let rec = &got.documents[0];
    assert_eq!(rec.extractor, "plaintext@0.8.0");
    assert_eq!(rec.source_id, "a.txt");
    assert_eq!(rec.source_sha256, Some(Sha256Hash::of(b"raw source bytes")));
    assert_eq!(rec.metadata.as_deref(), Some(r#"{"k":"v"}"#));
    assert!(rec.text_stored);
}

#[tokio::test]
async fn a_hashed_source_states_its_own_extractor_and_a_record_has_no_source_hash() {
    let dir = tempfile::tempdir().unwrap();
    let idx = fresh(dir.path(), "c").await;
    let mut w = TextWriter::open(&idx, "jsonl@0.8.0", true).await.unwrap();
    let stated = DocSource::Hashed {
        sha256: Sha256Hash::of(b"pdf bytes"),
        extractor: "local-stage:pdf@0.8.0".into(),
    };
    let a = w
        .store_document(input("from a pdf", "p.pdf", 0, &stated))
        .unwrap()
        .unwrap();
    let b = w
        .store_document(input("a row", "row-7", 0, &DocSource::Record))
        .unwrap()
        .unwrap();
    w.flush(&idx).await.unwrap();
    let by = idx.documents_for(&[a, b]).await.unwrap().unwrap();
    assert_eq!(by[&a][0].extractor, "local-stage:pdf@0.8.0");
    assert_eq!(by[&a][0].source_sha256, Some(Sha256Hash::of(b"pdf bytes")));
    assert_eq!(by[&b][0].extractor, "jsonl@0.8.0");
    assert_eq!(by[&b][0].source_sha256, None, "a record is said by name");
}

#[tokio::test]
async fn every_refusal_is_named() {
    let dir = tempfile::tempdir().unwrap();

    // Held nowhere.
    let idx = fresh(dir.path(), "held").await;
    let mut w = TextWriter::open(&idx, "x@1", true).await.unwrap();
    w.store_document(input("t", "s", 0, &DocSource::Record))
        .unwrap();
    w.flush(&idx).await.unwrap();
    let other = Sha256Hash::of_str("never ingested");
    assert_eq!(idx.text(&other).await.unwrap(), Err(TextAbsence::NotHeld));

    // store_texts = false: named, recorded, not stored.
    let off = fresh(dir.path(), "off").await;
    let mut w = TextWriter::open(&off, "x@1", false).await.unwrap();
    let name = w
        .store_document(input("t", "s", 0, &DocSource::Record))
        .unwrap()
        .unwrap();
    w.flush(&off).await.unwrap();
    assert_eq!(
        off.text(&name).await.unwrap(),
        Err(TextAbsence::TextNotStored)
    );
    assert!(!Corpus::texts_in(off.path()).join(name.to_hex()).exists());

    // An index that already held chunks before any writer: texts not stored,
    // and a writer opened on it stores nothing rather than a partial store.
    let old = fresh(dir.path(), "old").await;
    old.insert_batch(&[chunk("legacy", None)]).await.unwrap();
    let mut w = TextWriter::open(&old, "x@1", true).await.unwrap();
    assert!(!w.is_active());
    assert_eq!(
        w.store_document(input("t", "s", 0, &DocSource::Record))
            .unwrap(),
        None
    );
    assert_eq!(
        old.text(&name).await.unwrap(),
        Err(TextAbsence::TextsNotStored)
    );
    assert_eq!(
        old.documents().await.unwrap().map(|(_, rows)| rows.len()),
        Err(TextAbsence::TextsNotStored)
    );
    assert_eq!(
        old.documents_for(&[name]).await.unwrap().map(|m| m.len()),
        Err(TextAbsence::TextsNotStored)
    );
}

#[tokio::test]
async fn a_text_that_does_not_match_its_name_is_an_error_never_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let idx = fresh(dir.path(), "c").await;
    let mut w = TextWriter::open(&idx, "x@1", true).await.unwrap();
    let name = w
        .store_document(input("true words", "s", 0, &DocSource::Record))
        .unwrap()
        .unwrap();
    w.flush(&idx).await.unwrap();
    std::fs::write(
        Corpus::texts_in(idx.path()).join(name.to_hex()),
        "altered words",
    )
    .unwrap();
    assert!(idx.text(&name).await.is_err());
}

#[tokio::test]
async fn replacing_a_source_swaps_its_records_and_drops_texts_no_one_names() {
    let dir = tempfile::tempdir().unwrap();
    let idx = fresh(dir.path(), "c").await;
    let mut w = TextWriter::open(&idx, "x@1", true).await.unwrap();
    let v1 = w
        .store_document(input("version one", "f.md", 0, &DocSource::Record))
        .unwrap()
        .unwrap();
    let shared = w
        .store_document(input("boilerplate", "f.md", 1, &DocSource::Record))
        .unwrap()
        .unwrap();
    w.store_document(input("boilerplate", "g.md", 0, &DocSource::Record))
        .unwrap();
    w.flush(&idx).await.unwrap();

    let v2 = w
        .store_document(input("version two", "f.md", 0, &DocSource::Record))
        .unwrap()
        .unwrap();
    w.replace_source(&idx, "f.md").await.unwrap();

    let texts = Corpus::texts_in(idx.path());
    assert!(
        !texts.join(v1.to_hex()).exists(),
        "a superseded text leaves the store"
    );
    assert!(texts.join(shared.to_hex()).exists(), "g.md still names it");
    assert_eq!(idx.text(&v1).await.unwrap(), Err(TextAbsence::NotHeld));
    assert_eq!(
        idx.text(&v2).await.unwrap().unwrap().documents[0].source_id,
        "f.md"
    );
    let shared_docs = idx.text(&shared).await.unwrap().unwrap().documents;
    assert_eq!(shared_docs.len(), 1, "f.md's record of it was replaced");
    assert_eq!(shared_docs[0].source_id, "g.md");
}

/// The record table is a set: a record a resumed ingest re-flushes is one
/// record, and the version moves with every write.
#[tokio::test]
async fn a_repeated_record_is_one_record() {
    let dir = tempfile::tempdir().unwrap();
    let idx = fresh(dir.path(), "c").await;
    let mut w = TextWriter::open(&idx, "x@1", true).await.unwrap();
    assert_eq!(idx.documents().await.unwrap().unwrap(), (None, Vec::new()));
    w.store_document(input("beta", "b", 0, &DocSource::Record))
        .unwrap();
    w.flush(&idx).await.unwrap();
    let (v1, rows) = idx.documents().await.unwrap().unwrap();
    assert_eq!(rows.len(), 1);
    w.store_document(input("beta", "b", 0, &DocSource::Record))
        .unwrap();
    w.flush(&idx).await.unwrap();
    let (v2, rows) = idx.documents().await.unwrap().unwrap();
    assert_eq!(rows.len(), 1, "identical rows collapse");
    assert_ne!(v1, v2, "a write moves the version a digest memo keys on");
}

#[tokio::test]
async fn a_union_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let src = fresh(dir.path(), "src").await;
    let mut w = TextWriter::open(&src, "x@1", true).await.unwrap();
    let name = w
        .store_document(input("one text", "s", 0, &DocSource::Record))
        .unwrap()
        .unwrap();
    w.flush(&src).await.unwrap();

    let dst = fresh(dir.path(), "dst").await;
    dst.set_text_store(true).unwrap();
    assert_eq!(dst.union_texts_from(&src.path()).await.unwrap(), 1);
    assert_eq!(
        dst.union_texts_from(&src.path()).await.unwrap(),
        0,
        "names are content hashes"
    );
    let got = dst.text(&name).await.unwrap().unwrap();
    assert_eq!((got.text.as_str(), got.documents.len()), ("one text", 1));
}

#[tokio::test]
async fn chunks_carry_their_text_name_through_schema_v4() {
    let dir = tempfile::tempdir().unwrap();
    let idx = fresh(dir.path(), "c").await;
    let name = Sha256Hash::of_str("the text");
    idx.insert_batch(&[chunk("cut from it", Some(name)), chunk("no text", None)])
        .await
        .unwrap();
    let names = idx.chunk_text_sha256s(&[1, 2]).await.unwrap();
    assert_eq!(names.get(&1), Some(&name));
    assert_eq!(names.get(&2), None);
}

/// A v3 index (no `text_sha256` column) migrates additively on open.
#[tokio::test]
async fn a_v3_index_gains_the_text_name_column_on_open() {
    let dir = tempfile::tempdir().unwrap();
    let idx = fresh(dir.path(), "c").await;
    idx.insert_batch(&[chunk("old row", None)]).await.unwrap();
    idx.table().drop_columns(&["text_sha256"]).await.unwrap();
    let path = idx.path();
    drop(idx);
    let meta_path = Corpus::meta_in(&path);
    let mut meta: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&meta_path).unwrap()).unwrap();
    meta["schema_version"] = 3.into();
    std::fs::write(&meta_path, meta.to_string()).unwrap();

    let idx = CorpusIndex::open(&path).await.unwrap();
    let name = Sha256Hash::of_str("t");
    idx.insert_batch(&[chunk("new row", Some(name))])
        .await
        .unwrap();
    assert_eq!(
        idx.chunk_text_sha256s(&[1, 2]).await.unwrap().get(&2),
        Some(&name)
    );
    assert_eq!(read_meta(&path).unwrap().schema_version, 4);
}
