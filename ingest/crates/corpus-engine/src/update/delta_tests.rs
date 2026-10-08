// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for `delta.rs`, moved out of it unchanged.

use super::*;

#[test]
fn stamp_doc_identity_sets_doc_id_and_stem_title() {
    let mut chunks = vec![
        crate::index::InsertChunk {
            content: "body".into(),
            title: None,
            url: None,
            metadata: None,
            content_hash: None,
            source_doc_id: None,
            source_file: None,
            code: crate::index::InsertCodeMeta::default(),
            unit_id: None,
            text_sha256: None,
        },
        crate::index::InsertChunk {
            content: "body2".into(),
            // A chunker-provided title must survive the stamp.
            title: Some("Existing".into()),
            url: None,
            metadata: None,
            content_hash: None,
            source_doc_id: None,
            source_file: None,
            code: crate::index::InsertCodeMeta::default(),
            unit_id: None,
            text_sha256: None,
        },
    ];
    stamp_doc_identity(&mut chunks, "notes/daily/2026-06-10.md");
    for c in &chunks {
        assert_eq!(
            c.source_doc_id.as_deref(),
            Some("notes/daily/2026-06-10.md")
        );
    }
    assert_eq!(chunks[0].title.as_deref(), Some("2026-06-10"));
    assert_eq!(chunks[1].title.as_deref(), Some("Existing"));
}

fn make_manifest(corpus_id: &str, version: &str, entries: &[(&str, &str)]) -> VersionManifest {
    VersionManifest {
        corpus_id: corpus_id.into(),
        version: version.into(),
        entries: entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    }
}

#[test]
fn manifest_diff_compute_all_buckets() {
    let old = make_manifest(
        "sep",
        "v1",
        &[("doc-a", "hash1"), ("doc-b", "hash2"), ("doc-c", "hash3")],
    );
    let new = make_manifest(
        "sep",
        "v2",
        &[
            ("doc-b", "hash2-updated"),
            ("doc-c", "hash3"), // unchanged
            ("doc-d", "hash4"), // new
        ],
    );
    let diff = ManifestDiff::compute(&old, &new);
    assert_eq!(diff.new_documents, vec!["doc-d"]);
    assert_eq!(diff.updated_documents, vec!["doc-b"]);
    assert_eq!(diff.deleted_documents, vec!["doc-a"]);
}

#[test]
fn manifest_diff_empty_when_identical() {
    let m = make_manifest("sep", "v1", &[("doc-a", "hash1")]);
    let diff = ManifestDiff::compute(&m, &m);
    assert!(diff.is_empty());
}

#[test]
fn manifest_diff_all_new() {
    let old = make_manifest("sep", "v1", &[]);
    let new = make_manifest("sep", "v2", &[("doc-a", "h1"), ("doc-b", "h2")]);
    let diff = ManifestDiff::compute(&old, &new);
    assert_eq!(diff.new_documents.len(), 2);
    assert!(diff.updated_documents.is_empty());
    assert!(diff.deleted_documents.is_empty());
}

#[test]
fn manifest_diff_all_deleted() {
    let old = make_manifest("sep", "v1", &[("doc-a", "h1")]);
    let new = make_manifest("sep", "v2", &[]);
    let diff = ManifestDiff::compute(&old, &new);
    assert!(diff.new_documents.is_empty());
    assert!(diff.updated_documents.is_empty());
    assert_eq!(diff.deleted_documents.len(), 1);
}
