// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **ingest-side** extractor seam: labeled mentions with offsets.
//!
//! Two extractor surfaces exist in this workspace and they are not the
//! same shape:
//!
//! - `sovereign_core::traits::EntityExtractor` — the RETRIEVAL side.
//!   Label-less, lower-cased, deduped strings; enough for jaccard
//!   overlap on a query turn. Both backends already implement it.
//! - [`LabeledEntityExtractor`] (this module) — the INGEST side. Needs
//!   the label and the character span, because every mention becomes a
//!   `chunk_entities` row (`EntityMention::into_row`) that retrieval,
//!   source attribution and the atlas all read back by type.
//!
//! Before P2.1 the ingest side had no seam at all: [`GlinerChunkExtractor`]
//! held a concrete `Arc<GlinerExtractor>`, so the GLiNER2 backend —
//! measured 2.52× faster and ~4.8× lighter on 2026-08-02 (notes
//! `abc4fb34`, `3f47d12e`) — could be proven in a probe but could not
//! reach a corpus. This trait is that seam, and
//! [`load_labeled_extractor`] is the ONE place that decides which
//! generation runs.
//!
//! [`GlinerChunkExtractor`]: crate::GlinerChunkExtractor

use std::collections::HashMap;
use std::sync::Arc;

use sovereign_core::error::Result;

use crate::gliner2::{Gliner2Extractor, GLINER2_DEFAULT_THRESHOLD};
use crate::gliner_ner::{
    model_spec, EntityMention, GlinerExtractor, GlinerGeneration, DEFAULT_LABELS, DEFAULT_MODEL_ID,
    DEFAULT_THRESHOLD,
};

/// Env knob naming the GLiNER model the INGEST path loads. Open set (a
/// model id, resolved through the `KNOWN_MODELS` registry) rather than a
/// boolean, so pointing at a different checkpoint of either generation
/// costs no new code — see ARCH_PRINCIPLES §4.
///
/// Sibling of the existing `SOVEREIGN_GLINER_MODEL_DIR`, which says
/// WHERE models live; this one says WHICH.
pub const MODEL_ID_ENV: &str = "SOVEREIGN_GLINER_MODEL_ID";

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

    /// Which generation this is. Derived from the model id through the
    /// one registry that owns that mapping, so no impl can disagree
    /// with `model_spec` about what it loaded.
    fn generation(&self) -> GlinerGeneration {
        model_spec(self.model_id()).generation
    }
}

/// Collapse a chunk's mentions to one per case-insensitive
/// `(text, label)`, keeping the highest-scoring span and preserving
/// first-seen order.
///
/// The single implementation of that rule. It was written out three
/// times before this function existed (twice in `gliner_ner`, once
/// implicitly in the GLiNER2 probe) — ARCH_PRINCIPLES §10.6.
///
/// Ties keep the FIRST occurrence, matching the reading order callers
/// render in.
pub(crate) fn dedupe_strongest(mentions: Vec<EntityMention>) -> Vec<EntityMention> {
    let mut out: Vec<EntityMention> = Vec::with_capacity(mentions.len());
    let mut seen: HashMap<(String, String), usize> = HashMap::new();
    for mention in mentions {
        if mention.text.is_empty() {
            continue;
        }
        let key = (mention.text.to_lowercase(), mention.label.clone());
        match seen.get(&key) {
            Some(&idx) if out[idx].score >= mention.score => continue,
            Some(&idx) => out[idx] = mention,
            None => {
                seen.insert(key, out.len());
                out.push(mention);
            }
        }
    }
    out
}

/// The model id the ingest path should load: `SOVEREIGN_GLINER_MODEL_ID`
/// when set and non-blank, else [`DEFAULT_MODEL_ID`] (v1).
///
/// Default-off by construction: with the env var unset this returns
/// exactly what every call site hardcoded before P2.1, so wiring the
/// seam changes no behaviour on its own.
pub fn configured_model_id() -> String {
    match std::env::var(MODEL_ID_ENV) {
        Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => DEFAULT_MODEL_ID.to_string(),
    }
}

/// Load the ingest extractor for `model_id`. **The one decider** for
/// which GLiNER generation runs on an ingest path.
///
/// `threshold` overrides the generation's default when supplied — the
/// two generations do not share a tuned cutoff (v1 0.6, GLiNER2 0.5),
/// so there is no single number to hardcode here. Both load the same
/// [`DEFAULT_LABELS`] set, which is what keeps mention sets comparable
/// across a backend swap.
///
/// Never substitutes: a model id that resolves to a generation whose
/// files are missing returns `Err` naming the id. It does not quietly
/// fall back to the other backend (ARCH_PRINCIPLES §18.3).
pub fn load_labeled_extractor(
    model_id: &str,
    threshold: Option<f32>,
) -> Result<Arc<dyn LabeledEntityExtractor>> {
    let generation = model_spec(model_id).generation;
    let extractor: Arc<dyn LabeledEntityExtractor> = match generation {
        GlinerGeneration::V1 => Arc::new(GlinerExtractor::new(
            model_id,
            DEFAULT_LABELS,
            threshold.unwrap_or(DEFAULT_THRESHOLD),
        )?),
        GlinerGeneration::V2 => Arc::new(Gliner2Extractor::new(
            model_id,
            DEFAULT_LABELS,
            threshold.unwrap_or(GLINER2_DEFAULT_THRESHOLD),
        )?),
    };
    tracing::info!(
        model_id,
        generation = ?generation,
        threshold = threshold.unwrap_or(match generation {
            GlinerGeneration::V1 => DEFAULT_THRESHOLD,
            GlinerGeneration::V2 => GLINER2_DEFAULT_THRESHOLD,
        }),
        labels = ?DEFAULT_LABELS,
        env_knob = MODEL_ID_ENV,
        "gliner: ingest entity extractor loaded"
    );
    Ok(extractor)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mention(text: &str, label: &str, score: f32, start: usize) -> EntityMention {
        EntityMention {
            text: text.to_string(),
            label: label.to_string(),
            char_start: start,
            char_end: start + text.len(),
            score,
        }
    }

    /// The rule the two backends must agree on: same entity, same label,
    /// different casing — one row, the confident one.
    #[test]
    fn dedupe_keeps_highest_score_case_insensitively() {
        let out = dedupe_strongest(vec![
            mention("Sosa", "Person", 0.61, 0),
            mention("sosa", "Person", 0.94, 40),
        ]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].score, 0.94);
        assert_eq!(out[0].char_start, 40);
    }

    /// A tie must not churn the ordering — the first span wins, so two
    /// runs over the same chunk produce byte-identical rows.
    #[test]
    fn dedupe_ties_keep_the_first_occurrence() {
        let out = dedupe_strongest(vec![
            mention("BonJour", "Person", 0.8, 0),
            mention("BonJour", "Person", 0.8, 90),
        ]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].char_start, 0);
    }

    /// The same surface form under two labels is two facts, not a
    /// collision — this is precisely the type-collapse signal P2.1 is
    /// measuring, so it must survive dedup.
    #[test]
    fn dedupe_separates_labels() {
        let out = dedupe_strongest(vec![
            mention("Sosa", "Person", 0.9, 0),
            mention("Sosa", "Work", 0.7, 30),
        ]);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn dedupe_drops_empty_text() {
        let out = dedupe_strongest(vec![mention("", "Person", 0.9, 0)]);
        assert!(out.is_empty());
    }

    /// Preserves order of first appearance so downstream row order is a
    /// function of the text, not of HashMap iteration.
    #[test]
    fn dedupe_preserves_first_seen_order() {
        let out = dedupe_strongest(vec![
            mention("Quine", "Person", 0.9, 0),
            mention("Harvard", "Organization", 0.9, 10),
            mention("quine", "Person", 0.95, 20),
        ]);
        assert_eq!(
            out.iter().map(|m| m.label.as_str()).collect::<Vec<_>>(),
            vec!["Person", "Organization"]
        );
    }

    /// Unset env var must resolve to the v1 default — the whole
    /// default-off claim rests on this.
    #[test]
    fn configured_model_id_defaults_to_v1() {
        // Not using `set_var`: this test asserts the fallback branch,
        // and env mutation is process-global under nextest's shared
        // process model. The branch is `Err(_) | Ok(blank) => default`.
        assert_eq!(DEFAULT_MODEL_ID, "gliner_small-v2.1");
        if std::env::var(MODEL_ID_ENV).is_err() {
            assert_eq!(configured_model_id(), DEFAULT_MODEL_ID);
        }
    }
}
