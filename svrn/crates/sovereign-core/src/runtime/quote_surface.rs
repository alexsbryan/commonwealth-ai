// SPDX-License-Identifier: AGPL-3.0-or-later
//! The stored texts a turn's evidence was cut from (ADDRESSED_TEXT §5.3,
//! convergence commit 2): what the post-synthesis quote guard verifies
//! against, ahead of the prompt's rendering and the chunks.
//!
//! A chunk is a re-joined, overlapped, title-headed cut of its document, so a
//! quote from the same document past the chunk's edge is verbatim source text
//! the chunks alone cannot see (the class of GR-19/20, one level out). The
//! text is the string the chunks were cut from, so reading it closes that
//! class, and a quote that verifies there is addressed into it.
//!
//! A corpus that keeps no texts, a chunk that names none, a corpus that is
//! not local: each contributes nothing, counted by name in the trace, and the
//! guard then verifies against exactly what it verified against before.

use std::collections::HashSet;
use std::sync::Arc;

use corpus_index::index::TextAbsence;
use corpus_index::source::CorpusReadPort;
use corpus_index::types::ScoredChunk;
use kernel_types::Sha256Hash;

use crate::quote_verification::{verify_answer_against_turn_texts, VerificationResult};
use crate::types::CitationTarget;

/// What the quote guard read for a turn: its verdict, and the stored texts it
/// verified against, in the order `verification.verified[..].source` indexes
/// them (a source index below `texts.len()` is a text).
#[derive(Debug, Clone, Default)]
pub struct TurnVerification {
    /// The guard's verdict over the answer.
    pub verification: VerificationResult,
    /// The stored texts, listed first among the sources.
    pub texts: Vec<EvidenceText>,
}

/// One stored text behind the turn's evidence, and where it is held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceText {
    /// The corpus whose chunk named it.
    pub corpus_id: String,
    /// The text's name.
    pub text_sha256: Sha256Hash,
    /// The text.
    pub text: String,
}

/// The `(corpus, chunk)` handles of a retrieved pool, in pool order; a chunk
/// with no stable row id has none and names no text.
pub(crate) fn targets_of(chunks: &[ScoredChunk]) -> Vec<CitationTarget> {
    chunks
        .iter()
        .filter_map(|c| {
            c.chunk_id.map(|chunk_id| CitationTarget {
                corpus_id: c.corpus_id.clone(),
                chunk_id,
            })
        })
        .collect()
}

/// The post-synthesis quote guard for a turn: read the stored texts behind
/// `targets`, then verify `answer` against them, the prompt's `evidence`
/// rendering and the untruncated `chunks`
/// ([`verify_answer_against_turn_texts`]).
pub(crate) async fn verify_against_turn(
    engine: Option<&Arc<dyn CorpusReadPort>>,
    answer: &str,
    evidence: &str,
    chunks: &[String],
    targets: &[CitationTarget],
) -> TurnVerification {
    let texts = if evidence.trim().is_empty() {
        Vec::new() // the parametric path verifies nothing; read nothing for it
    } else {
        evidence_texts(engine, targets).await
    };
    let sources: Vec<&str> = texts.iter().map(|t| t.text.as_str()).collect();
    let verification = verify_answer_against_turn_texts(answer, evidence, chunks, &sources);
    tracing::debug!(
        verified = verification.verified_count,
        verified_in_stored_texts = verification
            .verified
            .iter()
            .filter(|v| v.source < texts.len())
            .count(),
        demoted = verification.demoted_count,
        texts = texts.len(),
        "quote guard: verified against the turn's stored texts, evidence and chunks"
    );
    TurnVerification {
        verification,
        texts,
    }
}

/// The stored texts behind `targets`, one per distinct `(corpus, text)`, in
/// the order their first chunk appears.
pub(crate) async fn evidence_texts(
    engine: Option<&Arc<dyn CorpusReadPort>>,
    targets: &[CitationTarget],
) -> Vec<EvidenceText> {
    let Some(engine) = engine else {
        tracing::debug!(
            chunks = targets.len(),
            "evidence texts: no corpus engine on this runtime; none read"
        );
        return Vec::new();
    };
    let mut corpora: Vec<(&str, Vec<u64>)> = Vec::new();
    for t in targets {
        match corpora.iter_mut().find(|(k, _)| *k == t.corpus_id) {
            Some((_, ids)) => ids.push(t.chunk_id),
            None => corpora.push((t.corpus_id.as_str(), vec![t.chunk_id])),
        }
    }
    let mut out: Vec<EvidenceText> = Vec::new();
    let (mut not_local, mut unnamed, mut unread) = (0usize, 0usize, 0usize);
    let mut absent = [0usize; 3];
    for (corpus, ids) in corpora {
        let index = match engine.open_index_for_corpus(corpus).await {
            Ok(i) => i,
            Err(e) => {
                tracing::debug!(%corpus, error = %e, "evidence texts: corpus not readable here (a peer's, or gone); its chunks stand alone");
                not_local += ids.len();
                continue;
            }
        };
        let names = match index.chunk_text_sha256s(&ids).await {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(%corpus, error = %e, "evidence texts: chunk text names unread; its chunks stand alone");
                unread += ids.len();
                continue;
            }
        };
        let mut seen = HashSet::new();
        for id in &ids {
            let Some(name) = names.get(id) else {
                unnamed += 1;
                continue;
            };
            if !seen.insert(*name) {
                continue;
            }
            match index.text(name).await {
                Ok(Ok(stored)) => out.push(EvidenceText {
                    corpus_id: corpus.to_string(),
                    text_sha256: *name,
                    text: stored.text,
                }),
                Ok(Err(a)) => {
                    absent[match a {
                        TextAbsence::NotHeld => 0,
                        TextAbsence::TextsNotStored => 1,
                        TextAbsence::TextNotStored => 2,
                    }] += 1
                }
                Err(e) => {
                    tracing::warn!(%corpus, %name, error = %e, "evidence texts: a recorded text is unreadable; not used");
                    unread += 1;
                }
            }
        }
    }
    tracing::debug!(
        chunks = targets.len(),
        texts = out.len(),
        chunks_not_local = not_local,
        chunks_naming_no_text = unnamed,
        unread,
        not_held = absent[0],
        texts_not_stored = absent[1],
        text_not_stored = absent[2],
        "evidence texts: read for the quote guard"
    );
    out
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use corpus_index::index::{CorpusIndex, DocSource, DocumentInput, InsertChunk, TextWriter};
    use corpus_index::ingest_port::double::IngestPortDouble;

    use super::*;

    const DIM: usize = 4;

    /// `<dir>/<id>`, one document whose one chunk is `chunk`, its text stored
    /// when `store`, as ingest writes them.
    async fn install(dir: &Path, id: &str, text: &str, chunk: &str, store: bool) {
        let index = CorpusIndex::create(&dir.join(id), id, id, "test-embed", DIM, true, "CC0")
            .await
            .unwrap();
        let mut writer = if store {
            Some(
                TextWriter::open(&index, "plaintext@test", true)
                    .await
                    .unwrap(),
            )
        } else {
            None
        };
        let name = match writer.as_mut() {
            Some(w) => w
                .store_document(DocumentInput {
                    text,
                    source_id: "doc",
                    ordinal: 0,
                    source: &DocSource::Hashed {
                        sha256: Sha256Hash::of_str(text),
                        extractor: "plaintext@test".into(),
                    },
                    metadata: None,
                })
                .unwrap(),
            None => None,
        };
        if let Some(w) = writer.as_mut() {
            w.flush(&index).await.unwrap();
        }
        let row = InsertChunk {
            content: chunk.to_string(),
            title: None,
            url: None,
            metadata: None,
            content_hash: None,
            source_doc_id: Some("doc".into()),
            source_file: None,
            code: Default::default(),
            unit_id: None,
            text_sha256: name,
        };
        index.insert_batch(&[(row, vec![0.0; DIM])]).await.unwrap();
        index.build_indexes(false, true, None).await.unwrap();
        index.mark_ingestion_complete().unwrap();
    }

    /// The guard reads the stored text behind a retrieved chunk, so a quote
    /// from past the chunk's edge verifies and is addressed into that text. A
    /// corpus that keeps no texts contributes none, and the same quote is
    /// demoted against its chunk alone, as before.
    #[tokio::test]
    async fn the_guard_reads_the_text_behind_a_chunk() {
        let tmp = tempfile::tempdir().unwrap();
        let text = "The ledger was kept in a fair hand. Widow Hetch, who kept The Cold \
                    Lantern, gave her evidence at her own bar with her arms folded.";
        let chunk = "The ledger was kept in a fair hand.";
        install(tmp.path(), "kept", text, chunk, true).await;
        install(tmp.path(), "bare", text, chunk, false).await;
        let engine: Arc<dyn CorpusReadPort> = Arc::new(
            IngestPortDouble::new()
                .with_index_dir(tmp.path())
                .opening_indexes_under_index_dir(),
        );
        let sentence = "Widow Hetch, who kept The Cold Lantern, gave her evidence at her own bar";
        let answer = format!("As recorded: \"{sentence}\".");

        for (corpus, verified, texts) in [("kept", 1, 1), ("bare", 0, 0)] {
            let index = engine.open_index_for_corpus(corpus).await.unwrap();
            let hits = index.search(&[], "ledger fair hand", 5).await.unwrap();
            let chunks: Vec<String> = hits.iter().map(|h| h.content.clone()).collect();
            let turn =
                verify_against_turn(Some(&engine), &answer, chunk, &chunks, &targets_of(&hits))
                    .await;
            assert_eq!(turn.texts.len(), texts, "{corpus}");
            assert_eq!(turn.verification.verified_count, verified, "{corpus}");
            if verified == 1 {
                let v = &turn.verification.verified[0];
                assert_eq!(v.source, 0, "addressed into the stored text");
                assert_eq!(turn.texts[0].corpus_id, "kept");
                assert_eq!(turn.texts[0].text_sha256, Sha256Hash::of_str(text));
                assert_eq!(
                    quote_align::code_point_slice(text, v.source_range.clone()),
                    Some(sentence)
                );
            }
        }
    }
}
