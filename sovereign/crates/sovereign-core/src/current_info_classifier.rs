// SPDX-License-Identifier: AGPL-3.0-or-later
//! Binary classifier for the "does this query need *current / time-
//! sensitive* information (→ external search)?" axis. Replaces the
//! `LlmRouter::needs_current_info` substring heuristic that drives the
//! `force_action` pre-check.
//!
//! ## Why this exists
//!
//! The pre-existing `needs_current_info` did a substring scan for a
//! hard-coded keyword list (`today`, `latest`, `news`, `current`, …).
//! That is the classic keyword-classifier failure: it works in-sample
//! and breaks out-of-sample. The witnessed regression: *"Write an
//! exhaustive essay on the history of Lebanon **from antiquity to
//! today**"* matched `today` → forced ACTION/"external tool" routing →
//! the model treated the 19 retrieved Wikipedia chunks as inadequate
//! and refused to write. "today" there is the *endpoint of a historical
//! range*, not a request for today's news. Other false positives:
//! "the history of the **news** industry", "explain the **stock**
//! market", "the 2024 Olympics" (a fixed past event).
//!
//! Semantic match beats string match here. This module is a centroid-
//! of-embeddings classifier — the same shape as the personal-scope
//! classifier (`scope_classifier.rs`) — over a deliberately small,
//! shape-diverse example set (`sovereign/router/current_info_examples.toml`):
//! a `[current]` class (genuinely time-sensitive: live scores, prices,
//! breaking news, "what's the latest on X") and an `[evergreen]` class
//! (history, definitions, how/why explanations, long-form essays —
//! including the "from antiquity to today" shape that broke the keyword
//! matcher).
//!
//! ## Algorithm (identical to `scope_classifier`)
//!
//! - Embed each example with the retrieval embedding model, L2-normalise.
//! - Per class, sum + L2-normalise → centroid.
//! - At query time, embed the query once, score
//!   `sim_c - sim_e` where `sim_x = cos(q, centroid_x)`.
//! - Return `true` (needs current info) only when BOTH:
//!   - `sim_c >= min_current_sim` (the query actually looks
//!     time-sensitive in absolute terms), AND
//!   - `sim_c - sim_e >= min_margin` (the current centroid wins by a
//!     non-trivial gap).
//!
//! Both gates matter. The absolute gate stops a query with no signal
//! either way from drifting into "current" just because that centroid
//! is marginally closer; the margin gate stops borderline queries from
//! flipping on embedding noise. When neither gate fires the query is
//! treated as evergreen — which routes to knowledge synthesis, the safe
//! default (a wrongly-evergreen current query still retrieves + answers;
//! a wrongly-current evergreen query refuses, the failure we are fixing).

use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::traits::InferenceProvider;

const DEFAULT_MIN_MARGIN: f32 = 0.04;
const DEFAULT_MIN_CURRENT_SIM: f32 = 0.50;

#[derive(Debug, Clone, Deserialize)]
struct CurrentInfoExamplesFile {
    #[serde(default)]
    current: InfoClass,
    #[serde(default)]
    evergreen: InfoClass,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct InfoClass {
    #[serde(default)]
    examples: Vec<String>,
}

/// Centroid-based binary classifier. One centroid per class
/// (current / evergreen). Loaded + embedded at boot; classification is
/// two dot products against the query embedding.
#[derive(Debug, Clone)]
pub struct CurrentInfoClassifier {
    centroid_current: Vec<f32>,
    centroid_evergreen: Vec<f32>,
    n_current: usize,
    n_evergreen: usize,
    min_margin: f32,
    min_current_sim: f32,
}

impl CurrentInfoClassifier {
    /// Load examples from `path`, embed each one, compute per-class
    /// centroids. Sequential embedding (small example counts; the
    /// embedding slot serialises anyway).
    pub async fn load(path: &Path, inference: Arc<dyn InferenceProvider>) -> Result<Self> {
        let raw = std::fs::read_to_string(path).map_err(|e| {
            Error::InvalidInput(format!("read current-info examples {}: {e}", path.display()))
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
        let parsed: CurrentInfoExamplesFile = toml::from_str(raw)
            .map_err(|e| Error::InvalidInput(format!("parse current-info examples: {e}")))?;
        if parsed.current.examples.is_empty() || parsed.evergreen.examples.is_empty() {
            return Err(Error::InvalidInput(
                "current-info examples need non-empty [current].examples and [evergreen].examples"
                    .into(),
            ));
        }

        let centroid_current =
            compute_centroid(&parsed.current.examples, &*inference, cache.as_deref_mut()).await?;
        let centroid_evergreen =
            compute_centroid(&parsed.evergreen.examples, &*inference, cache.as_deref_mut())
                .await?;

        if centroid_current.len() != centroid_evergreen.len() {
            return Err(Error::InvalidInput(format!(
                "current-info centroid dim mismatch: current={} evergreen={}",
                centroid_current.len(),
                centroid_evergreen.len()
            )));
        }

        let n_current = parsed.current.examples.len();
        let n_evergreen = parsed.evergreen.examples.len();
        tracing::info!(
            target: "router.current_info",
            n_current,
            n_evergreen,
            dims = centroid_current.len(),
            "current-info classifier loaded"
        );

        Ok(Self {
            centroid_current,
            centroid_evergreen,
            n_current,
            n_evergreen,
            min_margin: DEFAULT_MIN_MARGIN,
            min_current_sim: DEFAULT_MIN_CURRENT_SIM,
        })
    }

    /// Parse-only: the exemplar texts this classifier embeds (`current` then
    /// `evergreen`), WITHOUT running inference. SSOT for the boot-cache
    /// freshness gate — shares the exact `CurrentInfoExamplesFile` parse the
    /// `embed_query_cached` (`q:`) centroid path uses, so the gate can never
    /// drift from what actually gets cached.
    pub fn exemplar_texts(raw: &str) -> Result<Vec<String>> {
        let parsed: CurrentInfoExamplesFile = toml::from_str(raw)
            .map_err(|e| Error::InvalidInput(format!("parse current-info examples: {e}")))?;
        Ok(parsed
            .current
            .examples
            .into_iter()
            .chain(parsed.evergreen.examples)
            .collect())
    }

    /// Override the default thresholds. Useful for tests + tuning.
    pub fn with_thresholds(mut self, min_current_sim: f32, min_margin: f32) -> Self {
        self.min_current_sim = min_current_sim;
        self.min_margin = min_margin;
        self
    }

    pub fn current_count(&self) -> usize {
        self.n_current
    }
    pub fn evergreen_count(&self) -> usize {
        self.n_evergreen
    }

    /// Classify against a pre-computed, L2-normalised query embedding.
    /// Returns `true` (needs current info → external tool) only when
    /// both the absolute and margin gates pass. Logs both similarities
    /// + margin at info on the opt-in `router.current_info` target for
    /// glassbox gate tuning.
    pub fn classify_from_embedding(&self, q_normalized: &[f32]) -> bool {
        if q_normalized.len() != self.centroid_current.len() {
            tracing::warn!(
                target: "router.current_info",
                q_dim = q_normalized.len(),
                centroid_dim = self.centroid_current.len(),
                "current-info: dimension mismatch — treating as evergreen"
            );
            return false;
        }
        let sim_c = dot(q_normalized, &self.centroid_current);
        let sim_e = dot(q_normalized, &self.centroid_evergreen);
        let margin = sim_c - sim_e;
        let fires = sim_c >= self.min_current_sim && margin >= self.min_margin;
        tracing::info!(
            target: "router.current_info",
            sim_current = sim_c,
            sim_evergreen = sim_e,
            margin,
            min_current_sim = self.min_current_sim,
            min_margin = self.min_margin,
            fires,
            "current-info classification"
        );
        fires
    }

    /// Convenience: embed `query` via `inference` and classify. Prefer
    /// `classify_from_embedding` when the router already has the query
    /// embedding (avoid a redundant embed call).
    pub async fn classify(&self, query: &str, inference: &dyn InferenceProvider) -> Result<bool> {
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

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

fn normalize(v: &mut [f32]) {
    let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        for x in v.iter_mut() {
            *x /= n;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gates_require_both_absolute_and_margin() {
        // centroid_current at [1,0], centroid_evergreen at [0,1].
        let c = CurrentInfoClassifier {
            centroid_current: vec![1.0, 0.0],
            centroid_evergreen: vec![0.0, 1.0],
            n_current: 1,
            n_evergreen: 1,
            min_margin: 0.05,
            min_current_sim: 0.5,
        };
        // Query right at the current centroid → fires.
        assert!(c.classify_from_embedding(&[1.0, 0.0]));
        // Query orthogonal to both → sim_c = 0 → absolute gate blocks.
        assert!(!c.classify_from_embedding(&[0.0, 0.0]));
        // Query at evergreen centroid → sim_c = 0 → blocks.
        assert!(!c.classify_from_embedding(&[0.0, 1.0]));
        // Query at the midpoint → sim_c == sim_e (margin 0) → blocks.
        // This is the "from antiquity to today" shape: pulled toward
        // both centroids, committed to neither → evergreen default.
        let half = (0.5f32).sqrt();
        assert!(!c.classify_from_embedding(&[half, half]));
    }

    #[test]
    fn dimension_mismatch_is_evergreen_not_panic() {
        let c = CurrentInfoClassifier {
            centroid_current: vec![1.0, 0.0, 0.0],
            centroid_evergreen: vec![0.0, 1.0, 0.0],
            n_current: 1,
            n_evergreen: 1,
            min_margin: 0.0,
            min_current_sim: 0.0,
        };
        // Wrong-dim query (exemplars embedded with a different model) →
        // skip rather than panic, default to evergreen.
        assert!(!c.classify_from_embedding(&[1.0, 0.0]));
    }
}
