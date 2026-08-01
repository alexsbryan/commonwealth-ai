// SPDX-License-Identifier: AGPL-3.0-or-later
//! Binary classifier for the P1.4 claim-class axis: is a gate claim a
//! FACTUAL/SPECIFIC assertion (must verify against Leaf source text)
//! or a THEMATIC/STRUCTURAL one (Summary-class evidence may support
//! it)?
//!
//! ## Why this shape
//!
//! The first cut of this decision was a thematic-marker substring list
//! — the classic keyword-classifier failure the router already
//! replaced twice (`current_info_classifier.rs`, `scope_classifier`):
//! it works in-sample, breaks on paraphrase ("the piece meditates on
//! loss" has no marker), and rots as the operator adds tests. Same
//! cure as those siblings: a centroid-of-embeddings classifier over a
//! small, shape-diverse exemplar set
//! (`sovereign/router/claim_class_examples.toml`, baked default via
//! `include_str!`).
//!
//! ## Algorithm (identical to `current_info_classifier`)
//!
//! - Embed each exemplar with the retrieval embedding model, L2-normalise.
//! - Per class, sum + L2-normalise → centroid.
//! - Per claim, embed once; thematic only when BOTH gates pass:
//!   `sim_thematic >= min_sim` AND `sim_thematic - sim_factual >= min_margin`.
//! - Anything else — low signal, thin margin, embed failure, dim
//!   mismatch — is FACTUAL, the conservative direction: a
//!   wrongly-factual thematic claim merely demands leaf support
//!   (honesty unaffected), while a wrongly-thematic factual claim
//!   would let an LLM summary vouch for a fact — the exact failure
//!   P1.4 exists to prevent.
//!
//! Structural overrides (digits, quotations → factual) run in the
//! caller BEFORE embedding: those are features of the claim's form,
//! not its vocabulary, and are not brittle the way marker lists are.

use std::sync::Arc;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::router_axis::{dot, normalize};
use crate::traits::InferenceProvider;

/// Baked exemplar default — the stack works with no on-disk router dir.
pub const BAKED_CLAIM_CLASS_EXAMPLES: &str =
    include_str!("../../../router/claim_class_examples.toml");

const DEFAULT_MIN_MARGIN: f32 = 0.04;
const DEFAULT_MIN_THEMATIC_SIM: f32 = 0.50;

#[derive(Debug, Clone, Deserialize)]
struct ClaimClassExamplesFile {
    #[serde(default)]
    factual: ClassExamples,
    #[serde(default)]
    thematic: ClassExamples,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ClassExamples {
    #[serde(default)]
    examples: Vec<String>,
}

/// Which evidence class a gate claim demands (T1 P1.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimClass {
    /// Needs Leaf (source-text) support.
    Factual,
    /// Summary-class evidence may additionally support it.
    Thematic,
}

/// Centroid-based binary classifier, one centroid per class. Built
/// once per process (exemplars embedded lazily on first gate use);
/// classification is one embed + two dot products.
#[derive(Debug, Clone)]
pub struct ClaimClassClassifier {
    centroid_factual: Vec<f32>,
    centroid_thematic: Vec<f32>,
    min_margin: f32,
    min_thematic_sim: f32,
}

impl ClaimClassClassifier {
    /// Build from TOML exemplars (the baked default or caller-supplied).
    pub async fn from_toml_str(raw: &str, inference: &Arc<dyn InferenceProvider>) -> Result<Self> {
        let parsed: ClaimClassExamplesFile = toml::from_str(raw)
            .map_err(|e| Error::InvalidInput(format!("parse claim-class examples: {e}")))?;
        if parsed.factual.examples.is_empty() || parsed.thematic.examples.is_empty() {
            return Err(Error::InvalidInput(
                "claim-class examples need non-empty [factual].examples and [thematic].examples"
                    .into(),
            ));
        }
        let centroid_factual = centroid_of(&parsed.factual.examples, inference).await?;
        let centroid_thematic = centroid_of(&parsed.thematic.examples, inference).await?;
        if centroid_factual.len() != centroid_thematic.len() {
            return Err(Error::InvalidInput(format!(
                "claim-class centroid dim mismatch: factual={} thematic={}",
                centroid_factual.len(),
                centroid_thematic.len()
            )));
        }
        tracing::info!(
            target: "gate.claim_class",
            n_factual = parsed.factual.examples.len(),
            n_thematic = parsed.thematic.examples.len(),
            dims = centroid_factual.len(),
            "claim-class classifier loaded"
        );
        Ok(Self {
            centroid_factual,
            centroid_thematic,
            min_margin: DEFAULT_MIN_MARGIN,
            min_thematic_sim: DEFAULT_MIN_THEMATIC_SIM,
        })
    }

    /// Override the default gates. Tests + the P1.4 tuning loop.
    pub fn with_thresholds(mut self, min_thematic_sim: f32, min_margin: f32) -> Self {
        self.min_thematic_sim = min_thematic_sim;
        self.min_margin = min_margin;
        self
    }

    /// Build directly from pre-computed centroids — the deterministic
    /// test seam (no inference).
    pub fn from_centroids(centroid_factual: Vec<f32>, centroid_thematic: Vec<f32>) -> Self {
        Self {
            centroid_factual,
            centroid_thematic,
            min_margin: DEFAULT_MIN_MARGIN,
            min_thematic_sim: DEFAULT_MIN_THEMATIC_SIM,
        }
    }

    /// Classify a pre-computed, L2-normalised claim embedding. Logs the
    /// similarities on the opt-in `gate.claim_class` target for
    /// glassbox threshold tuning (the P1.4 calibration loop).
    pub fn classify_from_embedding(&self, q_normalized: &[f32]) -> ClaimClass {
        if q_normalized.len() != self.centroid_factual.len() {
            tracing::warn!(
                target: "gate.claim_class",
                q_dim = q_normalized.len(),
                centroid_dim = self.centroid_factual.len(),
                "claim-class dim mismatch — defaulting factual"
            );
            return ClaimClass::Factual;
        }
        let sim_f = dot(q_normalized, &self.centroid_factual);
        let sim_t = dot(q_normalized, &self.centroid_thematic);
        let class = if sim_t >= self.min_thematic_sim && (sim_t - sim_f) >= self.min_margin {
            ClaimClass::Thematic
        } else {
            ClaimClass::Factual
        };
        tracing::info!(
            target: "gate.claim_class",
            sim_factual = sim_f,
            sim_thematic = sim_t,
            margin = sim_t - sim_f,
            class = ?class,
            "claim-class classification"
        );
        class
    }

    /// Embed + classify one claim. Any embed failure defaults FACTUAL.
    pub async fn classify(
        &self,
        claim: &str,
        inference: &Arc<dyn InferenceProvider>,
    ) -> ClaimClass {
        match inference.embed(claim).await {
            Ok(mut e) => {
                normalize(&mut e);
                self.classify_from_embedding(&e)
            }
            Err(err) => {
                tracing::warn!(
                    target: "gate.claim_class",
                    error = %err,
                    "claim embed failed — defaulting factual"
                );
                ClaimClass::Factual
            }
        }
    }
}

/// Process-wide lazy instance: exemplars are embedded once, on the
/// first gate turn that actually has Summary-class evidence to decide
/// about. `None` (build failure) is cached too — the gate then treats
/// every claim as factual, the pre-P1.4 behavior, rather than
/// re-paying a failing boot on every claim.
static SHARED: tokio::sync::OnceCell<Option<Arc<ClaimClassClassifier>>> =
    tokio::sync::OnceCell::const_new();

pub async fn shared_claim_classifier(
    inference: &Arc<dyn InferenceProvider>,
) -> Option<Arc<ClaimClassClassifier>> {
    SHARED
        .get_or_init(|| async {
            match ClaimClassClassifier::from_toml_str(BAKED_CLAIM_CLASS_EXAMPLES, inference).await {
                Ok(c) => Some(Arc::new(c)),
                Err(e) => {
                    tracing::warn!(
                        target: "gate.claim_class",
                        error = %e,
                        "claim-class classifier unavailable — all claims treated as factual"
                    );
                    None
                }
            }
        })
        .await
        .clone()
}

async fn centroid_of(
    examples: &[String],
    inference: &Arc<dyn InferenceProvider>,
) -> Result<Vec<f32>> {
    let mut sum: Vec<f32> = Vec::new();
    for ex in examples {
        let mut e = inference.embed(ex).await?;
        normalize(&mut e);
        if sum.is_empty() {
            sum = e;
        } else {
            if sum.len() != e.len() {
                return Err(Error::InvalidInput(format!(
                    "claim-class exemplar dim mismatch: {} vs {}",
                    sum.len(),
                    e.len()
                )));
            }
            for (s, v) in sum.iter_mut().zip(e.iter()) {
                *s += v;
            }
        }
    }
    normalize(&mut sum);
    Ok(sum)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conservative_default_on_thin_margin_and_low_signal() {
        // Orthogonal 2-dim centroids; thresholds default (0.50 / 0.04).
        let c = ClaimClassClassifier::from_centroids(vec![1.0, 0.0], vec![0.0, 1.0]);
        // Clearly thematic direction.
        assert_eq!(
            c.classify_from_embedding(&[0.0, 1.0]),
            ClaimClass::Thematic
        );
        // Clearly factual direction.
        assert_eq!(c.classify_from_embedding(&[1.0, 0.0]), ClaimClass::Factual);
        // Ambiguous diagonal: margin 0 → factual (conservative).
        let d = std::f32::consts::FRAC_1_SQRT_2;
        assert_eq!(c.classify_from_embedding(&[d, d]), ClaimClass::Factual);
        // Thematic-leaning but below absolute sim floor → factual.
        let c_strict = ClaimClassClassifier::from_centroids(vec![1.0, 0.0], vec![0.0, 1.0])
            .with_thresholds(0.95, 0.04);
        assert_eq!(
            c_strict.classify_from_embedding(&[0.3, 0.9]),
            ClaimClass::Factual
        );
        // Dim mismatch → factual.
        assert_eq!(c.classify_from_embedding(&[1.0]), ClaimClass::Factual);
    }

    #[test]
    fn baked_exemplars_parse_with_both_classes() {
        let parsed: ClaimClassExamplesFile = toml::from_str(BAKED_CLAIM_CLASS_EXAMPLES).unwrap();
        assert!(parsed.factual.examples.len() >= 8);
        assert!(parsed.thematic.examples.len() >= 8);
    }
}
