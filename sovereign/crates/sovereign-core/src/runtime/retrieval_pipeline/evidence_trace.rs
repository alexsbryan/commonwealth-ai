//! Content identity for the debug-only per-step retrieval trace.
//!
//! ANS has neither chunk IDs nor URLs, and its section titles name many
//! different passages. A count or title trace cannot locate the step that
//! replaced the answer-bearing passage. Hash the full body, not the title or
//! the 200-character eval snippet; emit no source text into the trace.

use corpus_index::types::ScoredChunk;
use sha2::{Digest, Sha256};

pub(super) fn question_fingerprint(question: &str) -> String {
    format!("{:x}", Sha256::digest(question.as_bytes()))[..12].to_owned()
}

pub(crate) fn content_fingerprints(chunks: &[ScoredChunk]) -> Vec<String> {
    chunks
        .iter()
        .map(|chunk| {
            let mut hash = Sha256::new();
            for part in [
                chunk.corpus_id.as_str(),
                chunk.title.as_deref().unwrap_or(""),
                chunk.content.as_str(),
            ] {
                hash.update((part.len() as u64).to_le_bytes());
                hash.update(part.as_bytes());
            }
            format!("{:x}", hash.finalize())[..12].to_owned()
        })
        .collect()
}

/// Continue the same passage-identity trace past the pipeline's scope audit.
/// The handler owns expansion, late summaries and prompt admission, so the
/// pipeline itself cannot observe which of those removed a passage.
pub(crate) fn checkpoint(
    step: &'static str,
    question: &str,
    before: Option<Vec<String>>,
    chunks: &[ScoredChunk],
    admitted: Option<&[(usize, String)]>,
) -> Option<Vec<String>> {
    before.map(|before| {
        let all = content_fingerprints(chunks);
        let after = match admitted {
            Some(rows) => rows
                .iter()
                .filter_map(|(idx, _)| all.get(*idx).cloned())
                .collect(),
            None => all,
        };
        tracing::debug!(
            target: "retrieval.pipeline",
            pipeline = "knowledge_query",
            step,
            query_hash = %question_fingerprint(question),
            before = ?before,
            after = ?after,
            "retrieval.pipeline: passage identities"
        );
        after
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_title_different_passages_have_distinct_trace_ids() {
        let mut first = ScoredChunk {
            corpus_id: "ans".into(),
            title: Some("THE DEMANHUR HOARD".into()),
            content: "Mr. Khayat first brought it to Newell".into(),
            url: None,
            score: 1.0,
            metadata: Default::default(),
            chunk_id: None,
            source_doc_id: None,
            vector_distance: None,
            provenance: corpus_index::index::ChunkProvenance::manufactured("trace_fixture"),
        };
        let gold = content_fingerprints(&[first.clone()]);
        first.content = "A different coin in the same section".into();
        assert_ne!(gold, content_fingerprints(&[first]));
    }

    #[test]
    fn admission_trace_uses_the_formatter_indices_not_titles() {
        let first = ScoredChunk {
            corpus_id: "ans".into(),
            title: Some("THE DEMANHUR HOARD".into()),
            content: "Mr. Khayat brought it to the writer".into(),
            url: None,
            score: 1.0,
            metadata: Default::default(),
            chunk_id: None,
            source_doc_id: None,
            vector_distance: None,
            provenance: corpus_index::index::ChunkProvenance::manufactured("trace_fixture"),
        };
        let mut second = first.clone();
        second.content = "A different passage under the same title".into();
        let chunks = vec![first, second];
        let before = content_fingerprints(&chunks);
        let after = checkpoint(
            "prompt_admission",
            "Who first brought the hoard?",
            Some(before.clone()),
            &chunks,
            Some(&[(1, "rendered second passage".into())]),
        )
        .expect("tracing is enabled by the supplied predecessor");
        assert_eq!(after, vec![before[1].clone()]);
        assert_ne!(after, before);
    }
}
