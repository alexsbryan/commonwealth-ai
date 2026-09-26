// SPDX-License-Identifier: AGPL-3.0-or-later
//! The NER port: labeled mentions with offsets.
//!
//! [`LabeledEntityExtractor`] is the one named-entity port. The retrieval
//! side's label-less [`EntityExtractor`](crate::traits::EntityExtractor) is
//! its narrow view. Moved here from `sovereign-gliner` (phase-b
//! pb-serving-ner) so a host holds the port without linking the ONNX stack
//! that serves it; the historical `sovereign_gliner` paths re-export these.

use std::sync::Arc;

use crate::daemon_wire::conv_tiered::ChunkEntityRow;
use crate::error::Result;
use crate::traits::EntityExtractor;

/// One extracted entity mention with character offsets into the
/// preprocessed (role-marker-stripped) chunk text. Use the
/// `original_offsets_from_processed` helper to map back to offsets
/// in the raw chunk content for highlight rendering.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityMention {
    /// The mention's surface text, whitespace-normalized.
    pub text: String,
    /// Its label, in canonical casing (`Person`, `Work`, …).
    pub label: String,
    /// Start offset, in characters, into the stripped text.
    pub char_start: usize,
    /// End offset, in characters, into the stripped text.
    pub char_end: usize,
    /// The model's score for this span.
    pub score: f32,
}

impl EntityMention {
    /// Promote a stack of mentions into persisted `ChunkEntityRow`s
    /// for one chunk. Callers stamp `extracted_at` from a single
    /// timestamp so all rows in a batch share the same provenance.
    pub fn into_row(
        self,
        corpus_id: &str,
        chunk_id: u64,
        conv_uuid: Option<&str>,
        extracted_at: i64,
    ) -> ChunkEntityRow {
        ChunkEntityRow {
            corpus_id: corpus_id.to_string(),
            chunk_id,
            text: self.text,
            label: self.label,
            char_start: self.char_start as i64,
            char_end: self.char_end as i64,
            score: self.score as f64,
            conv_uuid: conv_uuid.map(|s| s.to_string()),
            extracted_at,
        }
    }
}

/// Which GLiNER generation a model id belongs to.
///
/// This is a closed set on purpose (ARCH_PRINCIPLES §2): each variant
/// implies a different input contract and a different loader, so a
/// generation the code cannot drive must not be nameable in config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlinerGeneration {
    /// gline-rs stack, entities only.
    V1,
    /// Bare-`ort` schema-driven export: entities, types, typed slots.
    V2,
}

/// A per-chunk extractor that reports the label and the span, not just
/// the string.
///
/// One method is required. `extract_mentions_batch` has a looping
/// default so a backend without true batched inference (GLiNER2 drives
/// one graph call per text) is a two-line impl, while v1 — whose
/// gline-rs stack batches natively and gains real throughput from it —
/// overrides it.
pub trait LabeledEntityExtractor: Send + Sync {
    /// The model id this extractor loaded. Logged at every wiring site
    /// so a run's routing is readable from the trace, not inferred.
    fn model_id(&self) -> &str;

    /// The label set handed to the model, in canonical output casing.
    ///
    /// Required, not defaulted: this and [`threshold`](Self::threshold)
    /// are persisted verbatim onto `chunk_entity_progress`, and that row
    /// is the only durable record of WHICH extractor built a corpus. A
    /// default would put a plausible lie in the audit trail.
    fn labels(&self) -> Vec<String>;

    /// The score cutoff this extractor applied. The two generations do
    /// not share one (v1 0.6, GLiNER2 0.5) — see `GLINER2_DEFAULT_THRESHOLD`.
    fn threshold(&self) -> f32;

    /// Mentions in one chunk: threshold-filtered, whitespace-normalized,
    /// and deduped within the chunk by case-insensitive `(text, label)`
    /// with the highest score winning.
    ///
    /// Offsets point into the ROLE-MARKER-STRIPPED text, not the raw
    /// chunk (both backends strip before inference). Callers rendering
    /// highlights over raw content must map back.
    fn extract_mentions(&self, text: &str) -> Result<Vec<EntityMention>>;

    /// Mentions for many chunks, one `Vec` per input, in input order.
    fn extract_mentions_batch(&self, texts: &[&str]) -> Result<Vec<Vec<EntityMention>>> {
        texts.iter().map(|t| self.extract_mentions(t)).collect()
    }

    /// Mentions from the dedicated `Concept` pass (abstract nouns, -isms),
    /// which retrieval's entity-obligation lane reads through
    /// [`EntityExtractor::extract_concepts`]. Default: no concepts, the same
    /// answer as a backend with no such pass (GLiNER2 today).
    fn extract_concept_mentions(&self, _text: &str) -> Result<Vec<EntityMention>> {
        Ok(Vec::new())
    }

    /// Which generation this is. A backend derives it from the model id
    /// through the one registry that owns that mapping (sovereign-gliner's
    /// `model_spec`), so no impl can disagree about what it loaded. Required
    /// because that registry is the backend's, not this crate's.
    fn generation(&self) -> GlinerGeneration;
}

/// The narrow view: [`EntityExtractor`] served over the labeled port. The ONE
/// adapter — no backend implements the narrow port itself, so the retrieval
/// side and the ingest side read the same extractor the same way (ARCH §8).
pub struct NerEntities(pub Arc<dyn LabeledEntityExtractor>);

impl NerEntities {
    /// The labeled handle this view reads.
    pub fn labeled(&self) -> &Arc<dyn LabeledEntityExtractor> {
        &self.0
    }
}

impl EntityExtractor for NerEntities {
    fn extract_entities(&self, text: &str) -> Vec<String> {
        lowered_unique(self.0.extract_mentions(text), self.0.model_id(), "entities")
    }

    fn extract_concepts(&self, text: &str) -> Vec<String> {
        lowered_unique(
            self.0.extract_concept_mentions(text),
            self.0.model_id(),
            "concepts",
        )
    }
}

/// Lower-case and dedupe mention texts, first seen first. An extraction error
/// yields no entities — retrieval falls back to cosine + MMR for the turn —
/// and says so at `warn`.
fn lowered_unique(mentions: Result<Vec<EntityMention>>, model_id: &str, pass: &str) -> Vec<String> {
    let mentions = match mentions {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(model_id, pass, error = %e, "NER extraction failed; returning no entities");
            return Vec::new();
        }
    };
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(mentions.len());
    for m in mentions {
        let key = m.text.to_lowercase();
        if seen.insert(key.clone()) {
            out.push(key);
        }
    }
    out
}
