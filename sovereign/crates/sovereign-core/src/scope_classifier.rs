// SPDX-License-Identifier: AGPL-3.0-or-later
//! Binary classifier for the "is this query about the user's own
//! history?" axis. Orthogonal to intent — a `KnowledgeQuery` and a
//! `DeepQuery` can both carry `scope = "personal"`, and downstream
//! retrieval restricts to user-owned corpora when they do.
//!
//! ## Why this exists
//!
//! Intent (KnowledgeQuery / DeepQuery / etc.) and scope (personal /
//! external) are independent axes. We tried to bolt scope onto
//! the existing k=1 NN intent classifier (`EmbedRouter`) via
//! per-exemplar `scope = "personal"` tags. That collapsed for two
//! reasons:
//!
//! 1. k=1 NN gives ONE intent its scope tag — but the bench
//!    questions land near exemplars from *other* intents
//!    (`MetalingualQuery` exemplars stole "Have I ever discussed
//!    cuneiform" because "discuss" looked metalingual), erasing
//!    the scope hint entirely.
//! 2. Adding more personal-scope exemplars to "fix" the
//!    misroutes ended up coaching to the bench — every new
//!    exemplar was a paraphrase of a bench question, and the
//!    real-world generalisation didn't survive culling them.
//!
//! Scope deserves its own decision surface. This module is that
//! surface: a centroid-of-embeddings classifier trained on a
//! deliberately small, shape-diverse, bench-disjoint example set
//! (`sovereign/router/scope_examples.toml`).
//!
//! ## Algorithm
//!
//! - Load `[personal]` and `[external]` example arrays from the
//!   TOML.
//! - Embed each example with the same model the retrieval pipeline
//!   uses (`InferenceProvider::embed_query`), L2-normalise.
//! - Per class, sum + L2-normalise → centroid in
//!   embedding space.
//! - At query time, embed the query once (the router was already
//!   doing this for `EmbedRouter`), score
//!   `sim_p - sim_e` where `sim_x = cos(q, centroid_x)`.
//! - Return `Some("personal")` when BOTH:
//!   - `sim_p >= min_personal_sim` (the query actually looks
//!     personal in absolute terms, not just relative), AND
//!   - `sim_p - sim_e >= min_margin` (the personal centroid wins
//!     by a non-trivial gap).
//!
//! Both gates matter. The absolute gate prevents a question with
//! no signal either way ("explain transformers") from drifting
//! into personal just because the personal centroid happened to
//! be marginally closer. The margin gate prevents borderline
//! questions from flipping decisions on small embedding noise.

use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::router_axis::{dot, normalize, AxisGate, AxisScore};
use crate::traits::InferenceProvider;

const DEFAULT_MIN_MARGIN: f32 = 0.02;
const DEFAULT_MIN_PERSONAL_SIM: f32 = 0.45;

#[derive(Debug, Clone, Deserialize)]
struct ScopeExamplesFile {
    #[serde(default)]
    personal: ScopeClass,
    #[serde(default)]
    external: ScopeClass,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ScopeClass {
    #[serde(default)]
    examples: Vec<String>,
}

/// Centroid-based binary classifier. One centroid per class
/// (personal / external). Loaded + embedded at boot; classification
/// is two dot products against the query embedding.
#[derive(Debug, Clone)]
pub struct PersonalScopeClassifier {
    centroid_personal: Vec<f32>,
    centroid_external: Vec<f32>,
    n_personal: usize,
    n_external: usize,
    min_margin: f32,
    min_personal_sim: f32,
}

impl PersonalScopeClassifier {
    /// Load examples from `path`, embed each one, compute per-class
    /// centroids. Sequential embedding (small example counts; the
    /// embedding slot serialises anyway).
    pub async fn load(path: &Path, inference: Arc<dyn InferenceProvider>) -> Result<Self> {
        let raw = std::fs::read_to_string(path).map_err(|e| {
            Error::InvalidInput(format!("read scope examples {}: {e}", path.display()))
        })?;
        Self::from_toml_str(&raw, inference).await
    }

    /// Build from in-memory TOML (the baked default in
    /// [`crate::router_bootstrap`], or any caller-supplied content). Identical
    /// parse + centroid path to [`Self::load`] minus the file read, so a binary
    /// with no on-disk exemplars still gets the classifier — bench/desktop
    /// parity by construction.
    pub async fn from_toml_str(raw: &str, inference: Arc<dyn InferenceProvider>) -> Result<Self> {
        Self::from_toml_str_cached(raw, inference, None).await
    }

    /// [`Self::from_toml_str`] with an optional boot embed cache (see
    /// [`crate::router_embed_cache`]) — the example embeddings are
    /// static per (text, model) and re-embedding them is boot time.
    pub async fn from_toml_str_cached(
        raw: &str,
        inference: Arc<dyn InferenceProvider>,
        mut cache: Option<&mut crate::router_embed_cache::BootEmbedCache>,
    ) -> Result<Self> {
        let parsed: ScopeExamplesFile = toml::from_str(raw)
            .map_err(|e| Error::InvalidInput(format!("parse scope examples: {e}")))?;
        if parsed.personal.examples.is_empty() || parsed.external.examples.is_empty() {
            return Err(Error::InvalidInput(
                "scope examples need non-empty [personal].examples and [external].examples".into(),
            ));
        }

        let centroid_personal =
            compute_centroid(&parsed.personal.examples, &*inference, cache.as_deref_mut()).await?;
        let centroid_external =
            compute_centroid(&parsed.external.examples, &*inference, cache).await?;

        if centroid_personal.len() != centroid_external.len() {
            return Err(Error::InvalidInput(format!(
                "scope centroid dim mismatch: personal={} external={}",
                centroid_personal.len(),
                centroid_external.len()
            )));
        }

        let n_personal = parsed.personal.examples.len();
        let n_external = parsed.external.examples.len();
        tracing::info!(
            target: "router.scope",
            n_personal,
            n_external,
            dims = centroid_personal.len(),
            "personal-scope classifier loaded"
        );

        Ok(Self {
            centroid_personal,
            centroid_external,
            n_personal,
            n_external,
            min_margin: DEFAULT_MIN_MARGIN,
            min_personal_sim: DEFAULT_MIN_PERSONAL_SIM,
        })
    }

    /// Parse-only: the exemplar texts this classifier embeds (`personal`
    /// then `external`), WITHOUT running inference. SSOT for the boot-cache
    /// freshness gate — shares the exact `ScopeExamplesFile` parse the
    /// `embed_query_cached` (`q:`) centroid path uses, so the gate can never
    /// drift from what actually gets cached.
    pub fn exemplar_texts(raw: &str) -> Result<Vec<String>> {
        let parsed: ScopeExamplesFile = toml::from_str(raw)
            .map_err(|e| Error::InvalidInput(format!("parse scope examples: {e}")))?;
        Ok(parsed
            .personal
            .examples
            .into_iter()
            .chain(parsed.external.examples)
            .collect())
    }

    /// Override the default thresholds. Useful for tests + tuning.
    pub fn with_thresholds(mut self, min_personal_sim: f32, min_margin: f32) -> Self {
        self.min_personal_sim = min_personal_sim;
        self.min_margin = min_margin;
        self
    }

    pub fn personal_count(&self) -> usize {
        self.n_personal
    }
    pub fn external_count(&self) -> usize {
        self.n_external
    }

    /// Raw, UNGATED score: cosine to each class centroid.
    ///
    /// `None` only on a dimension mismatch (a query embedded with a
    /// different model). Split out from [`Self::classify_from_embedding`]
    /// so [`crate::router_calibration`] can evaluate any candidate gate
    /// from a single embedding pass — the score does not depend on the
    /// thresholds, only the decision does.
    pub fn score_from_embedding(&self, q_normalized: &[f32]) -> Option<AxisScore> {
        if q_normalized.len() != self.centroid_personal.len() {
            tracing::warn!(
                target: "router.scope",
                q_dim = q_normalized.len(),
                centroid_dim = self.centroid_personal.len(),
                "scope: dimension mismatch — skipping"
            );
            return None;
        }
        Some(AxisScore::new(
            dot(q_normalized, &self.centroid_personal),
            dot(q_normalized, &self.centroid_external),
        ))
    }

    /// The gate currently applied to this axis.
    pub fn gate(&self) -> AxisGate {
        AxisGate::new(self.min_personal_sim, self.min_margin)
    }

    /// Classify against a pre-computed, L2-normalised query embedding.
    /// Returns `Some("personal")` only when both absolute and margin
    /// gates pass. Logs both similarities + margin + the signed
    /// distance to the boundary for glassbox tuning (ARCH §0.1).
    pub fn classify_from_embedding(&self, q_normalized: &[f32]) -> Option<String> {
        let score = self.score_from_embedding(q_normalized)?;
        let gate = self.gate();
        let fires = gate.admits(score);
        tracing::info!(
            target: "router.scope",
            sim_personal = score.sim_positive,
            sim_external = score.sim_negative,
            margin = score.margin(),
            min_personal_sim = self.min_personal_sim,
            min_margin = self.min_margin,
            cushion = gate.cushion(score),
            fires,
            "scope classification"
        );
        fires.then(|| "personal".to_string())
    }

    /// Convenience: embed `query` via `inference` and classify.
    /// Prefer `classify_from_embedding` when the router already has
    /// the query embedding (avoid a redundant embed call).
    pub async fn classify(
        &self,
        query: &str,
        inference: &dyn InferenceProvider,
    ) -> Result<Option<String>> {
        let mut q = inference.embed_query(query).await?;
        normalize(&mut q);
        Ok(self.classify_from_embedding(&q))
    }
}

async fn compute_centroid(
    examples: &[String],
    inference: &dyn InferenceProvider,
    mut cache: Option<&mut crate::router_embed_cache::BootEmbedCache>,
) -> Result<Vec<f32>> {
    let mut sum: Option<Vec<f32>> = None;
    for ex in examples {
        let mut emb = match cache.as_deref_mut() {
            Some(c) => c.embed_query_cached(inference, ex).await?,
            None => inference.embed_query(ex).await?,
        };
        normalize(&mut emb);
        match sum.as_mut() {
            Some(s) => {
                if s.len() != emb.len() {
                    return Err(Error::InvalidInput(format!(
                        "centroid embeddings dim mismatch: {} vs {}",
                        s.len(),
                        emb.len()
                    )));
                }
                for (i, v) in emb.into_iter().enumerate() {
                    s[i] += v;
                }
            }
            None => sum = Some(emb),
        }
    }
    let mut c =
        sum.ok_or_else(|| Error::InvalidInput("compute_centroid: empty example set".into()))?;
    normalize(&mut c);
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot_orthogonal_is_zero() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        assert!((dot(&a, &b)).abs() < 1e-6);
    }

    #[test]
    fn normalize_unit() {
        let mut v = vec![3.0, 4.0];
        normalize(&mut v);
        assert!((v[0] - 0.6).abs() < 1e-6);
        assert!((v[1] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn gates_require_both_absolute_and_margin() {
        // Synthesise a classifier directly (no embedding inference).
        // centroid_personal at [1,0], centroid_external at [0,1].
        let c = PersonalScopeClassifier {
            centroid_personal: vec![1.0, 0.0],
            centroid_external: vec![0.0, 1.0],
            n_personal: 1,
            n_external: 1,
            min_margin: 0.05,
            min_personal_sim: 0.5,
        };
        // Query right at the personal centroid → fires.
        assert!(c.classify_from_embedding(&[1.0, 0.0]).is_some());
        // Query orthogonal to both → sim_p = 0 → absolute gate blocks.
        assert!(c.classify_from_embedding(&[0.0, 0.0]).is_none());
        // Query at external centroid → sim_p = 0 → blocks.
        assert!(c.classify_from_embedding(&[0.0, 1.0]).is_none());
        // Query at midpoint → sim_p == sim_e (margin 0) → blocks.
        let half = (0.5f32).sqrt();
        assert!(c.classify_from_embedding(&[half, half]).is_none());
    }
}
