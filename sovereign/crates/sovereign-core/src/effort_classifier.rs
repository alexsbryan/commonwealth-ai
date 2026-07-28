//! Effort classifier — the *how-much-capability* axis of the MECE query
//! taxonomy (`sovereign/docs/QUERY_TAXONOMY_MECE.md`), orthogonal to the
//! [`crate::types::Operation`] axis.
//!
//! Why a centroid classifier (and not the coarse LLM verdict): the deployed 4B
//! coarse classifier *is* non-deterministic even at temperature 0 (MoE routing
//! + Metal float), so its `REASONING` verdict — the thing that would escalate
//! an exhaustive ask to the primary slot — fires only intermittently. A
//! centroid over the (deterministic) query embedding is stable run-to-run.
//!
//! Why embeddings work here despite effort being "lexical": an exhaustiveness
//! request ("give an exhaustive, section-by-section account…") carries a
//! request-shape signal the embedding captures *across topics*. A daemon probe
//! (2026-06-09, `target/ci-bench/effort_centroid_probe.py`) built centroids
//! from GENERAL high/low-effort exemplars (Roman Republic, photosynthesis, WWI…)
//! and separated HELD-OUT chaos questions 6/6: the maximal-essay asks landed on
//! the high centroid (margin +0.11/+0.16), the single-fact lookups on the low
//! (margin −0.08…−0.15). Held-out + general exemplars = genuine generalization,
//! not teaching to the bench.
//!
//! Mirrors [`crate::scope_classifier::PersonalScopeClassifier`] (same
//! centroid + dual-gate shape). The two share the `compute_centroid`/`dot`/
//! `normalize` pattern; kept local (small, standard) rather than refactored
//! into a shared module in this pass.

use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::router_axis::{dot, normalize, AxisGate, AxisScore};
use crate::traits::InferenceProvider;
use crate::types::Effort;

/// High must beat Low by at least this cosine margin. The probe separated all
/// held-out cases with |margin| ≥ 0.079 (high: +0.11/+0.16), so 0.04 clears the
/// low cases while leaving headroom; matches the embed-router's margin default.
const DEFAULT_MIN_MARGIN: f32 = 0.04;
/// High also needs this absolute similarity to the high centroid, so an
/// off-distribution query near neither centroid abstains (→ Low) rather than
/// escalating on a razor-thin margin. The probe's high cases sat at ~0.39.
const DEFAULT_MIN_HIGH_SIM: f32 = 0.30;

#[derive(Debug, Clone, Deserialize)]
struct EffortExamplesFile {
    #[serde(default)]
    high: EffortClass,
    #[serde(default)]
    low: EffortClass,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct EffortClass {
    #[serde(default)]
    examples: Vec<String>,
}

/// Centroid-based binary effort classifier. One centroid per class
/// (high / low), embedded at boot; classification is two dot products against
/// the (already-computed, L2-normalised) query embedding — no extra embed call
/// when the router shares its query embedding.
#[derive(Debug, Clone)]
pub struct EffortClassifier {
    centroid_high: Vec<f32>,
    centroid_low: Vec<f32>,
    n_high: usize,
    n_low: usize,
    min_margin: f32,
    min_high_sim: f32,
}

impl EffortClassifier {
    /// Load examples from `path`, embed each, compute per-class centroids.
    /// Sequential embedding (small example counts; the embed slot serialises).
    pub async fn load(path: &Path, inference: Arc<dyn InferenceProvider>) -> Result<Self> {
        let raw = std::fs::read_to_string(path).map_err(|e| {
            Error::InvalidInput(format!("read effort examples {}: {e}", path.display()))
        })?;
        Self::from_toml_str(&raw, inference).await
    }

    /// Build from in-memory TOML (the baked default in
    /// [`crate::router_bootstrap`], or any caller-supplied content). Identical
    /// parse + centroid path to [`Self::load`] minus the file read, so a binary
    /// with no on-disk exemplars (a desktop `.app`) still gets the classifier —
    /// bench/desktop parity by construction.
    pub async fn from_toml_str(raw: &str, inference: Arc<dyn InferenceProvider>) -> Result<Self> {
        Self::from_toml_str_cached(raw, inference, None).await
    }

    /// [`Self::from_toml_str`] with an optional boot embed cache (see
    /// [`crate::router_embed_cache`]). Cached under the *unprefixed*
    /// `d:` key space — see the centroid comment below.
    pub async fn from_toml_str_cached(
        raw: &str,
        inference: Arc<dyn InferenceProvider>,
        mut cache: Option<&mut crate::router_embed_cache::BootEmbedCache>,
    ) -> Result<Self> {
        let parsed: EffortExamplesFile = toml::from_str(raw)
            .map_err(|e| Error::InvalidInput(format!("parse effort examples: {e}")))?;
        if parsed.high.examples.is_empty() || parsed.low.examples.is_empty() {
            return Err(Error::InvalidInput(
                "effort examples need non-empty [high].examples and [low].examples".into(),
            ));
        }

        let centroid_high =
            compute_centroid(&parsed.high.examples, &*inference, cache.as_deref_mut()).await?;
        let centroid_low = compute_centroid(&parsed.low.examples, &*inference, cache).await?;
        if centroid_high.len() != centroid_low.len() {
            return Err(Error::InvalidInput(format!(
                "effort centroid dim mismatch: high={} low={}",
                centroid_high.len(),
                centroid_low.len()
            )));
        }

        let n_high = parsed.high.examples.len();
        let n_low = parsed.low.examples.len();
        tracing::info!(
            target: "router.effort",
            n_high,
            n_low,
            dims = centroid_high.len(),
            "effort classifier loaded"
        );

        Ok(Self {
            centroid_high,
            centroid_low,
            n_high,
            n_low,
            min_margin: DEFAULT_MIN_MARGIN,
            min_high_sim: DEFAULT_MIN_HIGH_SIM,
        })
    }

    /// Parse-only: the exemplar texts this classifier embeds (`high` then
    /// `low`), WITHOUT running inference. SSOT for the boot-cache freshness
    /// gate. NB effort embeds via the UNPREFIXED `embed_cached` (`d:` space) —
    /// the gate keys these under `d`, not `q`, matching `compute_centroid`.
    pub fn exemplar_texts(raw: &str) -> Result<Vec<String>> {
        let parsed: EffortExamplesFile = toml::from_str(raw)
            .map_err(|e| Error::InvalidInput(format!("parse effort examples: {e}")))?;
        Ok(parsed
            .high
            .examples
            .into_iter()
            .chain(parsed.low.examples)
            .collect())
    }

    /// Override the default gates. Useful for tuning + tests.
    pub fn with_thresholds(mut self, min_high_sim: f32, min_margin: f32) -> Self {
        self.min_high_sim = min_high_sim;
        self.min_margin = min_margin;
        self
    }

    pub fn high_count(&self) -> usize {
        self.n_high
    }
    pub fn low_count(&self) -> usize {
        self.n_low
    }

    /// Classify a pre-computed, L2-normalised query embedding. Returns
    /// `Some(Effort::High)` only when BOTH the absolute and margin gates pass;
    /// otherwise `None` (the caller treats the absence of a High verdict as
    /// Low — the conservative, fast-slot default). Logs both similarities +
    /// margin at info for glassbox gate-tuning (ARCH §0.1 / §9).
    pub fn classify_from_embedding(&self, q_normalized: &[f32]) -> Option<Effort> {
        let score = self.score_from_embedding(q_normalized)?;
        let gate = self.gate();
        let fires = gate.admits(score);
        tracing::info!(
            target: "router.effort",
            sim_high = score.sim_positive,
            sim_low = score.sim_negative,
            margin = score.margin(),
            min_high_sim = self.min_high_sim,
            min_margin = self.min_margin,
            cushion = gate.cushion(score),
            fires,
            "effort classification"
        );
        fires.then_some(Effort::High)
    }

    /// Raw, UNGATED score: cosine to each class centroid.
    ///
    /// `None` only on a dimension mismatch. Split out from
    /// [`Self::classify_from_embedding`] so [`crate::router_calibration`]
    /// can evaluate any candidate gate from a single embedding pass.
    ///
    /// NOTE: this axis is scored in the UNPREFIXED (`d:`) embedding
    /// space — see `compute_centroid`. A calibration run must embed its
    /// cases the same way or the numbers are meaningless.
    pub fn score_from_embedding(&self, q_normalized: &[f32]) -> Option<AxisScore> {
        if q_normalized.len() != self.centroid_high.len() {
            tracing::warn!(
                target: "router.effort",
                q_dim = q_normalized.len(),
                centroid_dim = self.centroid_high.len(),
                "effort: dimension mismatch — skipping"
            );
            return None;
        }
        Some(AxisScore::new(
            dot(q_normalized, &self.centroid_high),
            dot(q_normalized, &self.centroid_low),
        ))
    }

    /// The gate currently applied to this axis.
    pub fn gate(&self) -> AxisGate {
        AxisGate::new(self.min_high_sim, self.min_margin)
    }

    /// Embed `query` (UNPREFIXED — see `compute_centroid`) and classify.
    /// This pays its own embed call rather than reusing the router's
    /// `embed_query` (prefixed) embedding, because the retrieval prefix
    /// destroys the effort signal. The cost is one short embed per turn,
    /// paid only when effort-tier escalation is enabled.
    pub async fn classify(
        &self,
        query: &str,
        inference: &dyn InferenceProvider,
    ) -> Result<Option<Effort>> {
        let mut q = inference.embed(query).await?;
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
        // UNPREFIXED `embed` (not `embed_query`): effort is NOT a retrieval
        // task, and a daemon probe (target/ci-bench/effort_prefix_probe.py,
        // 2026-06-09) showed the retrieval Instruct-prefix collapses the
        // effort signal (chaos "bombing" ask margin +0.123 unprefixed →
        // −0.035 prefixed). Both centroids and queries must use the same
        // unprefixed embedding — `embed_cached` keys this under `d:`,
        // disjoint from the other classifiers' `q:` space.
        let mut emb = match cache.as_deref_mut() {
            Some(c) => c.embed_cached(inference, ex).await?,
            None => inference.embed(ex).await?,
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

    fn synth(min_high_sim: f32, min_margin: f32) -> EffortClassifier {
        // centroid_high at [1,0], centroid_low at [0,1] (no inference).
        EffortClassifier {
            centroid_high: vec![1.0, 0.0],
            centroid_low: vec![0.0, 1.0],
            n_high: 1,
            n_low: 1,
            min_margin,
            min_high_sim,
        }
    }

    #[test]
    fn high_fires_only_when_both_gates_pass() {
        let c = synth(0.5, 0.05);
        // At the high centroid → sim_high=1, sim_low=0 → fires High.
        assert_eq!(c.classify_from_embedding(&[1.0, 0.0]), Some(Effort::High));
        // At the low centroid → sim_high=0 → absolute gate blocks → None (=Low).
        assert_eq!(c.classify_from_embedding(&[0.0, 1.0]), None);
        // Midpoint → margin 0 → blocks.
        let half = (0.5f32).sqrt();
        assert_eq!(c.classify_from_embedding(&[half, half]), None);
        // Near-high but below the absolute floor → blocks (margin alone insufficient).
        let c2 = synth(0.9, 0.04);
        // [0.6,0.0] (unnormalised) → sim_high=0.6 < 0.9 floor → None even though margin>0.
        assert_eq!(c2.classify_from_embedding(&[0.6, 0.0]), None);
    }

    #[test]
    fn dimension_mismatch_abstains() {
        let c = synth(0.3, 0.04);
        assert_eq!(c.classify_from_embedding(&[1.0, 0.0, 0.0]), None);
    }

    #[test]
    fn normalize_and_dot_basics() {
        let mut v = vec![3.0, 4.0];
        normalize(&mut v);
        assert!((v[0] - 0.6).abs() < 1e-6 && (v[1] - 0.8).abs() < 1e-6);
        assert!(dot(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
    }
}
