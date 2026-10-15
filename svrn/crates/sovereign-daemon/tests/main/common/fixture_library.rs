// SPDX-License-Identifier: AGPL-3.0-or-later
//! The conformance suite's fixture library
//! (`cmnwlth/crates/oicp-conformance/fixture/library.json`), stored the way
//! ingest stores it: each document's text through the one writer with the
//! heading paths ingest keeps beside it (v0.6 §2.13), its chunks
//! stamped with the text's name, an FTS index over them. The evidence routes'
//! e2e tests (align, verify) install it through here, so they read one
//! library built one way.

use std::sync::Arc;

use corpus_index::index::{
    CorpusIndex, DocSource, DocumentInput, DocumentRecord, InsertChunk, TextWriter,
};
use kernel_types::{NodeId, Sha256Hash};
use oicp_types::evidence::texts_digest_preimage;
use serde::Deserialize;
use sovereign_daemon::state::AppState;

const LIBRARY: &str =
    include_str!("../../../../../../cmnwlth/crates/oicp-conformance/fixture/library.json");
pub const DIM: usize = 8;
pub const EXTRACTOR: &str = "plain_text@test";

#[derive(Deserialize)]
pub struct Library {
    pub corpus_id: String,
    pub documents: Vec<Doc>,
    pub plant: Plant,
    /// Each document's signature sentence, by name.
    #[serde(default)]
    pub signatures: std::collections::BTreeMap<String, String>,
}

#[derive(Deserialize)]
pub struct Doc {
    pub name: String,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
    pub text: String,
}

#[derive(Deserialize)]
pub struct Plant {
    pub document: String,
    pub sentence: String,
    pub word: String,
    pub planted: String,
}

pub fn library() -> Library {
    serde_json::from_str(LIBRARY).expect("the conformance fixture loads")
}

/// The digest a client re-derives from the records (§2.4), which the port
/// double answers with so the route's carrying of it is what is tested.
pub fn digest_of(rows: &[DocumentRecord]) -> Sha256Hash {
    let hex: Vec<(String, Option<String>, String)> = rows
        .iter()
        .map(|r| {
            (
                r.text_sha256.to_hex(),
                r.source_sha256.map(|s| s.to_hex()),
                r.extractor.clone(),
            )
        })
        .collect();
    Sha256Hash::of_str(&texts_digest_preimage(
        hex.iter()
            .map(|(t, s, e)| (t.as_str(), s.as_deref(), e.as_str())),
    ))
}

/// Install `docs` as `corpus_id`, with a text store unless `texts` is false
/// (a corpus written before texts were stored).
pub async fn install(indexes: &std::path::Path, corpus_id: &str, docs: &[Doc], texts: bool) {
    let index = CorpusIndex::create(
        &indexes.join(corpus_id),
        corpus_id,
        corpus_id,
        "test-embed",
        DIM,
        true,
        "CC0",
    )
    .await
    .unwrap();
    // Opening a writer on an empty index begins its store, so the corpus
    // written before texts never opens one.
    let mut writer = if texts {
        Some(TextWriter::open(&index, EXTRACTOR, true).await.unwrap())
    } else {
        None
    };
    let mut chunks = Vec::new();
    for d in docs {
        let name = match writer.as_mut() {
            Some(w) => w
                .store_document_located(
                    DocumentInput {
                        text: &d.text,
                        source_id: &d.name,
                        ordinal: 0,
                        source: &DocSource::Hashed {
                            sha256: Sha256Hash::of_str(&d.text),
                            extractor: EXTRACTOR.into(),
                        },
                        metadata: d.metadata.as_ref().map(|m| m.to_string().into()),
                    },
                    heading_places(&d.text).as_deref(),
                )
                .unwrap(),
            None => None,
        };
        for para in d.text.split("\n\n").filter(|p| !p.trim().is_empty()) {
            chunks.push((
                InsertChunk {
                    content: para.to_string(),
                    title: Some(d.name.clone()),
                    url: None,
                    metadata: None,
                    content_hash: None,
                    source_doc_id: Some(d.name.clone()),
                    source_file: None,
                    code: Default::default(),
                    unit_id: None,
                    text_sha256: name,
                },
                vec![0.0_f32; DIM],
            ));
        }
    }
    if let Some(w) = writer.as_mut() {
        w.flush(&index).await.unwrap();
    }
    index.insert_batch(&chunks).await.unwrap();
    index.build_indexes(false, true, None).await.unwrap();
    index.mark_ingestion_complete().unwrap();
}

/// The places ingest keeps beside a plain text (corpus-engine's
/// `DeclaredMetadata::read`): its heading paths by the one rule, none for a
/// text with no headings.
fn heading_places(text: &str) -> Option<String> {
    let placed = oicp_types::locator::HeadingOutline::of(text).placed();
    (!placed.0.is_empty()).then(|| serde_json::to_string(&placed).unwrap())
}

pub async fn state_over(indexes: std::path::PathBuf) -> AppState {
    let embed: corpus_index::types::EmbedFn =
        Arc::new(|_t: &str| Box::pin(async { Ok(vec![0.0_f32; DIM]) }));
    let engine = crate::common::reading_double(indexes, embed).on_texts_digest(digest_of);
    AppState::new_with_platform_and_engine(NodeId::from_u128(1), Some(Arc::new(engine)))
}
