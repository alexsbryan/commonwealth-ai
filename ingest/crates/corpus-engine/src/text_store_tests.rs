// SPDX-License-Identifier: AGPL-3.0-or-later
//! The engine half of the text store: the extractor tag, the library digest
//! through the one preimage, and the merge rule.

use super::*;
use crate::index::{DocSource, DocumentInput, InsertChunk, TextAbsence, TextWriter};

const DIM: usize = 8;

async fn shard(root: &Path, name: &str, texts: &[(&str, &str)], store: bool) -> PathBuf {
    let path = root.join(name);
    let idx = CorpusIndex::create(&path, "c", "C", "m", DIM, true, "MIT")
        .await
        .unwrap();
    let mut w = TextWriter::open(&idx, "x@1", true).await.unwrap();
    let mut rows = Vec::new();
    for (source, text) in texts {
        let name = if store {
            w.store_document(DocumentInput {
                text,
                source_id: source,
                ordinal: 0,
                source: &DocSource::Record,
                metadata: None,
            })
            .unwrap()
        } else {
            None
        };
        rows.push((
            InsertChunk {
                content: text.to_string(),
                title: None,
                url: None,
                metadata: None,
                content_hash: Some(kernel_types::ContentHash::of_str(text).to_hex()),
                source_doc_id: Some(source.to_string()),
                source_file: None,
                code: Default::default(),
                unit_id: None,
                text_sha256: name,
            },
            vec![0.5; DIM],
        ));
    }
    w.flush(&idx).await.unwrap();
    if !store {
        idx.set_text_store(false).unwrap();
    }
    idx.insert_batch(&rows).await.unwrap();
    path
}

#[test]
fn the_extractor_tag_is_the_recipe_type_at_this_version() {
    let v = env!("CARGO_PKG_VERSION");
    let jsonl: ExtractorConfig = toml::from_str("type = \"jsonl\"").unwrap();
    assert_eq!(extractor_tag(&jsonl), format!("jsonl@{v}"));
    let custom: ExtractorConfig =
        toml::from_str("type = \"custom\"\nkind = \"pdf\"\nextension = \"pdf\"").unwrap();
    assert_eq!(extractor_tag(&custom), format!("custom:pdf@{v}"));
}

#[tokio::test]
async fn the_digest_is_sha256_of_the_published_preimage() {
    let dir = tempfile::tempdir().unwrap();
    let path = shard(dir.path(), "a", &[("s1", "alpha"), ("s2", "beta")], true).await;
    let idx = CorpusIndex::open(&path).await.unwrap();
    let (a, b) = (Sha256Hash::of_str("alpha"), Sha256Hash::of_str("beta"));
    let (a, b) = (a.to_hex(), b.to_hex());
    let preimage = oicp_types::evidence::texts_digest_preimage([
        (a.as_str(), None, "x@1"),
        (b.as_str(), None, "x@1"),
    ]);
    let got = texts_digest(&idx).await.unwrap().unwrap();
    assert_eq!(got, Sha256Hash::of_str(&preimage));
    // Re-reading after more chunks (no new texts) is the same digest.
    idx.insert_batch(&[]).await.unwrap();
    assert_eq!(texts_digest(&idx).await.unwrap().unwrap(), got);
}

#[tokio::test]
async fn a_merge_keeps_every_text_only_when_every_shard_had_them() {
    let dir = tempfile::tempdir().unwrap();
    let a = shard(dir.path(), "a", &[("s1", "alpha")], true).await;
    let b = shard(dir.path(), "b", &[("s2", "beta")], true).await;
    let merged = dir.path().join("merged");
    crate::sharding::merge_shards(&[a.clone(), b.clone()], &merged)
        .await
        .unwrap();
    let idx = CorpusIndex::open(&merged).await.unwrap();
    for text in ["alpha", "beta"] {
        let got = idx.text(&Sha256Hash::of_str(text)).await.unwrap().unwrap();
        assert_eq!(got.text, text, "both shards' texts are in the union");
    }

    let old = shard(dir.path(), "old", &[("s3", "gamma")], false).await;
    let partial = dir.path().join("partial");
    crate::sharding::merge_shards(&[a, old], &partial)
        .await
        .unwrap();
    let idx = CorpusIndex::open(&partial).await.unwrap();
    assert_eq!(
        idx.text(&Sha256Hash::of_str("alpha")).await.unwrap(),
        Err(TextAbsence::TextsNotStored),
        "a shard without a store makes the merge answer texts not stored"
    );
}
