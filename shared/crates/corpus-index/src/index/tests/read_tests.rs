// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for [`CorpusIndex::embeddings_for_chunk_ids`] (`index/read.rs`) — the
//! chunk-vector join the wiki seed-table migration borrows through.
//!
//! A child of `index::tests` rather than more lines in `index/mod.rs`: that
//! file is already past the size ceiling and these would have pushed it past
//! its slack (ARCH §3.1). Being a CHILD is what makes the carve free — the
//! fixtures in the parent (`create_test_index`, `sample_chunks`,
//! `read_meta`/`write_meta`) are reached by `use super::*` with nothing made
//! public for a test's sake.

use super::*;

#[tokio::test]
async fn embeddings_for_chunk_ids_borrows_the_stored_vector_and_nothing_else() {
    use std::collections::HashSet;

    // The seed-table migration's join: it must hand back the vector that is
    // ON DISK for a wanted id, bit for bit — a borrowed vector that is not
    // the chunk's own is not a borrow. Ids nobody asked for do not come
    // back, and an id with no row is ABSENT rather than a zero vector
    // (ARCH §18.3), so the caller can report the shortfall.
    let dir = tempdir().unwrap();
    let idx = create_test_index(dir.path()).await;
    idx.insert_batch(&sample_chunks()).await.unwrap();

    let all = idx.all_chunks_full().await.unwrap();
    let want: HashSet<u64> = all.iter().take(2).map(|c| c.id).collect();
    let absent = all.iter().map(|c| c.id).max().unwrap() + 1_000;
    let mut asked = want.clone();
    asked.insert(absent);

    let got = idx.embeddings_for_chunk_ids(&asked).await.unwrap();
    let got_ids: HashSet<u64> = got.keys().copied().collect();
    assert_eq!(got_ids, want, "only the wanted ids that exist come back");
    assert!(
        !got.contains_key(&absent),
        "an id with no row must be absent, never a zero vector"
    );

    // Byte-identical to what was written.
    let reference = idx.all_chunks_with_embeddings().await.unwrap();
    for (row, embedding) in &reference {
        if let Some(borrowed) = got.get(&row.id) {
            assert_eq!(
                borrowed, embedding,
                "chunk {} came back with a different vector than the one \
                 `all_chunks_with_embeddings` reads for it",
                row.id
            );
        }
    }
}

#[tokio::test]
async fn a_duplicate_chunk_id_resolves_to_the_lowest_row_position() {
    use std::collections::HashSet;

    // THE DEDUPE RULE, with the failing input that makes it necessary:
    // `chunks.lance` `id` is not unique (1 of 221 sampled wikipedia ids had
    // two rows). Forged here the way the real ones were made — by rewinding
    // the `next_chunk_id` high-water mark, which is exactly what
    // `allocate_chunk_ids` exists to prevent (see its comment on id reuse).
    //
    // Without a stated rule the winner is whichever row the scan happened
    // to reach last, so a rebuild could seed a different vector for the
    // same atom with nothing changed. First-seen — the lowest row position
    // — is deterministic.
    let dir = tempdir().unwrap();
    let idx = create_test_index(dir.path()).await;

    let first = vec![(
        InsertChunk {
            content: "the earlier row".into(),
            title: Some("Dup".into()),
            url: None,
            metadata: None,
            content_hash: None,
            source_doc_id: None,
            source_file: None,
            code: InsertCodeMeta::default(),
            unit_id: None,
        },
        make_embedding(&[1.0, 0.0, 0.0, 0.0]),
    )];
    idx.insert_batch(&first).await.unwrap();
    let dup_id = idx.all_chunks_full().await.unwrap()[0].id;

    // Rewind the high-water mark so the next insert re-issues `dup_id`.
    let index_dir = dir.path().join("test-corpus");
    let mut meta = read_meta(&index_dir).unwrap();
    meta.next_chunk_id = Some(dup_id);
    write_meta(&index_dir, &meta).unwrap();

    let second = vec![(
        InsertChunk {
            content: "the later row".into(),
            title: Some("Dup".into()),
            url: None,
            metadata: None,
            content_hash: None,
            source_doc_id: None,
            source_file: None,
            code: InsertCodeMeta::default(),
            unit_id: None,
        },
        make_embedding(&[0.0, 1.0, 0.0, 0.0]),
    )];
    idx.insert_batch(&second).await.unwrap();

    // The fixture must actually be the hazard, or the assertion below is
    // vacuous (ARCH §18.4 — validate the instrument before the result).
    let rows = idx.all_chunks_full().await.unwrap();
    assert_eq!(
        rows.iter().filter(|r| r.id == dup_id).count(),
        2,
        "the forged duplicate did not take; this test would pass for the \
         wrong reason"
    );

    let got = idx
        .embeddings_for_chunk_ids(&HashSet::from([dup_id]))
        .await
        .unwrap();
    assert_eq!(
        got.get(&dup_id).map(|v| v.as_slice()),
        Some([1.0_f32, 0.0, 0.0, 0.0].as_slice()),
        "first seen in the scan — the LOWEST row position — must win; \
         got the later row's vector instead"
    );
}
