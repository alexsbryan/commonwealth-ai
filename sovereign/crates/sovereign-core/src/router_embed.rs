//! Embedding-based intent classifier — k-NN over a hand-authored
//! exemplar TOML, k=1 (max-similarity per intent).
//!
//! Replaces the string-match heuristic pre-checks in `router::LlmRouter`
//! for cases where the embedding has high margin. Falls through to
//! the existing heuristic + LLM cascade when ambiguous.
//!
//! ## Why this exists
//!
//! Pre-2026-05-15 the router had a stack of `looks_like_*` heuristics
//! (`looks_like_metalingual`, `looks_like_conation`, etc.) doing
//! substring matching on the user message and FORCING an intent
//! before the LLM classifier ever saw the message. The heuristics
//! were brittle — "where do" in `DEFINITIONAL_VERBS` matched "where
//! does the deepest rent sit?", routing a factual lookup to
//! MetalingualQuery and producing an empty answer.
//!
//! Semantic match beats string match on every axis except cost.
//! Embedding is ~50ms (one batch call to the local embed slot) vs
//! ~500-2000ms for the small-LLM classifier — fast enough to run
//! before either heuristic OR LLM.
//!
//! ## Iteration loop
//!
//! Exemplars live in a TOML file (path from `$SOVEREIGN_ROUTER_EXEMPLARS`
//! env var, or the default `sovereign/router/exemplars.toml` relative
//! to the cwd). Add a misroute to the TOML → next process picks it
//! up. No rebuild required.
//!
//! ## Confidence + margin gate
//!
//! Returns an intent only when:
//! - top similarity > `MIN_TOP_SIM` (default 0.55 — exemplar must
//!   actually match), AND
//! - margin between top and second intent > `MIN_MARGIN` (default
//!   0.04 — top must be decisively ahead).
//!
//! Ambiguous queries (low margin or low top) fall through. The LLM
//! classifier handles those with full-sentence context.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::traits::InferenceProvider;
use crate::types::Intent;

const DEFAULT_MIN_TOP_SIM: f32 = 0.55;
const DEFAULT_MIN_MARGIN: f32 = 0.04;

/// On-disk exemplar list. Each `[[example]]` row carries an intent
/// name (matches `Intent` debug-format, lowercased + snake_case) and
/// a query string.
#[derive(Debug, Clone, Deserialize)]
struct ExemplarFile {
    #[serde(default)]
    example: Vec<ExemplarRow>,
}

#[derive(Debug, Clone, Deserialize)]
struct ExemplarRow {
    intent: String,
    query: String,
}

/// One embedded exemplar.
#[derive(Debug, Clone)]
struct Exemplar {
    intent: Intent,
    /// L2-normalised embedding. Cosine-similarity reduces to dot
    /// product after normalisation.
    embedding: Vec<f32>,
    /// Kept for diagnostics — surfaced in router rationale.
    query: String,
}

/// Result of an embed-classify call.
#[derive(Debug, Clone)]
pub struct EmbedClassification {
    pub intent: Intent,
    /// Max cosine similarity against any exemplar of the chosen
    /// intent (in `[-1, 1]`; for L2-normalised vectors usually
    /// `[0, 1]`).
    pub top_sim: f32,
    /// `top_sim - second_intent_sim`. Larger = more confident.
    pub margin: f32,
    /// Nearest exemplar text — diagnostic surface for "why did this
    /// route here?". Truncated to 80 chars.
    pub nearest_exemplar: String,
}

/// Hand-authored intent classifier. Pre-embeds every exemplar at
/// load time; classify-time cost is one embedding call plus a flat
/// loop over the exemplar set.
#[derive(Debug, Clone)]
pub struct EmbedRouter {
    exemplars: Vec<Exemplar>,
    min_top_sim: f32,
    min_margin: f32,
}

impl EmbedRouter {
    /// Load exemplars from the given TOML path; embed each one via
    /// `inference.embed_query`. Sequential because exemplar counts
    /// are small (~200) and the embed slot serialises anyway.
    pub async fn load(
        path: &Path,
        inference: Arc<dyn InferenceProvider>,
    ) -> Result<Self> {
        let raw = std::fs::read_to_string(path).map_err(|e| {
            Error::InvalidInput(format!("read exemplars {}: {e}", path.display()))
        })?;
        let parsed: ExemplarFile = toml::from_str(&raw).map_err(|e| {
            Error::InvalidInput(format!("parse exemplars {}: {e}", path.display()))
        })?;

        let mut exemplars = Vec::with_capacity(parsed.example.len());
        for row in parsed.example {
            let intent = parse_intent(&row.intent).map_err(|e| {
                Error::InvalidInput(format!(
                    "exemplar `{}`: {e}",
                    truncate(&row.query, 60)
                ))
            })?;
            let mut emb = inference.embed_query(&row.query).await?;
            normalize(&mut emb);
            exemplars.push(Exemplar {
                intent,
                embedding: emb,
                query: row.query,
            });
        }

        tracing::info!(
            target: "router.embed",
            exemplar_count = exemplars.len(),
            path = %path.display(),
            "embed-router loaded"
        );

        Ok(Self {
            exemplars,
            min_top_sim: DEFAULT_MIN_TOP_SIM,
            min_margin: DEFAULT_MIN_MARGIN,
        })
    }

    /// Override the default thresholds. Useful for tests + tuning.
    pub fn with_thresholds(mut self, min_top_sim: f32, min_margin: f32) -> Self {
        self.min_top_sim = min_top_sim;
        self.min_margin = min_margin;
        self
    }

    pub fn exemplar_count(&self) -> usize {
        self.exemplars.len()
    }

    /// Classify `query` by max-similarity per intent. Returns `Some`
    /// only when both top-similarity and margin gates pass.
    pub async fn classify(
        &self,
        query: &str,
        inference: &dyn InferenceProvider,
    ) -> Result<Option<EmbedClassification>> {
        if self.exemplars.is_empty() {
            return Ok(None);
        }
        let mut q = inference.embed_query(query).await?;
        normalize(&mut q);
        Ok(self.classify_from_embedding(&q))
    }

    /// Classify against a pre-computed query embedding. Public for
    /// callers that already have one (the router could splice this
    /// into the existing search-embedding pipeline to skip a second
    /// embed call).
    pub fn classify_from_embedding(
        &self,
        q_normalized: &[f32],
    ) -> Option<EmbedClassification> {
        if self.exemplars.is_empty() || q_normalized.is_empty() {
            return None;
        }

        // Max similarity per intent + remember the nearest exemplar
        // text for the diagnostic surface.
        let mut per_intent: HashMap<Intent, (f32, &str)> = HashMap::new();
        for ex in &self.exemplars {
            if ex.embedding.len() != q_normalized.len() {
                // Dimension mismatch (exemplars embedded with a
                // different model). Skip rather than panic — caller
                // will see "no result" and fall through.
                continue;
            }
            let sim = dot(q_normalized, &ex.embedding);
            per_intent
                .entry(ex.intent.clone())
                .and_modify(|(best, best_q)| {
                    if sim > *best {
                        *best = sim;
                        *best_q = ex.query.as_str();
                    }
                })
                .or_insert((sim, ex.query.as_str()));
        }
        if per_intent.is_empty() {
            return None;
        }

        let mut ranked: Vec<(Intent, f32, &str)> = per_intent
            .into_iter()
            .map(|(i, (s, q))| (i, s, q))
            .collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let (top_intent, top_sim, nearest) = (
            ranked[0].0.clone(),
            ranked[0].1,
            ranked[0].2,
        );
        let second_sim = ranked.get(1).map(|(_, s, _)| *s).unwrap_or(0.0);
        let margin = top_sim - second_sim;

        if top_sim < self.min_top_sim || margin < self.min_margin {
            return None;
        }
        Some(EmbedClassification {
            intent: top_intent,
            top_sim,
            margin,
            nearest_exemplar: truncate(nearest, 80),
        })
    }
}

// ── Helpers ──────────────────────────────────────────────────────

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

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect::<String>() + "…"
    }
}

/// Parse the snake_case intent label in the exemplar TOML into an
/// `Intent` enum. Accepts the same set the router's classifier
/// emits.
fn parse_intent(s: &str) -> std::result::Result<Intent, String> {
    match s.trim() {
        "simple_query" | "SimpleQuery" => Ok(Intent::SimpleQuery),
        "knowledge_query" | "KnowledgeQuery" => Ok(Intent::KnowledgeQuery),
        "deep_query" | "DeepQuery" => Ok(Intent::DeepQuery),
        "comparison_query" | "ComparisonQuery" => Ok(Intent::ComparisonQuery),
        "complex_task" | "ComplexTask" => Ok(Intent::ComplexTask),
        "metalingual_query" | "MetalingualQuery" => Ok(Intent::MetalingualQuery),
        "conation_query" | "ConationQuery" => Ok(Intent::ConationQuery),
        "commissive_query" | "CommissiveQuery" => Ok(Intent::CommissiveQuery),
        "expressive_query" | "ExpressiveQuery" => Ok(Intent::ExpressiveQuery),
        other => Err(format!("unknown intent label: {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_unit_vector() {
        let mut v = vec![3.0, 4.0];
        normalize(&mut v);
        assert!((v[0] - 0.6).abs() < 1e-6);
        assert!((v[1] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn dot_normalized_is_cosine() {
        let a = vec![0.6, 0.8];
        let b = vec![0.6, 0.8];
        assert!((dot(&a, &b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn parse_intent_snake_and_camel() {
        assert!(matches!(parse_intent("knowledge_query"), Ok(Intent::KnowledgeQuery)));
        assert!(matches!(parse_intent("KnowledgeQuery"), Ok(Intent::KnowledgeQuery)));
        assert!(parse_intent("nonsense").is_err());
    }

    fn make_exemplar(intent: Intent, query: &str, emb: Vec<f32>) -> Exemplar {
        let mut e = emb;
        normalize(&mut e);
        Exemplar {
            intent,
            embedding: e,
            query: query.into(),
        }
    }

    #[test]
    fn classify_picks_max_similarity_intent_with_margin() {
        let r = EmbedRouter {
            exemplars: vec![
                make_exemplar(Intent::KnowledgeQuery, "What is X?", vec![1.0, 0.0, 0.0]),
                make_exemplar(Intent::DeepQuery, "Why did X happen?", vec![0.0, 1.0, 0.0]),
                make_exemplar(Intent::MetalingualQuery, "What does X mean here?", vec![0.0, 0.0, 1.0]),
            ],
            min_top_sim: 0.5,
            min_margin: 0.1,
        };
        // Query close to the KnowledgeQuery exemplar
        let q = vec![0.95_f32, 0.10, 0.10];
        let mut qn = q.clone();
        normalize(&mut qn);
        let out = r.classify_from_embedding(&qn).unwrap();
        assert_eq!(out.intent, Intent::KnowledgeQuery);
        assert!(out.top_sim > 0.9);
        assert!(out.margin > 0.5);
    }

    #[test]
    fn classify_returns_none_below_min_top_sim() {
        let r = EmbedRouter {
            exemplars: vec![make_exemplar(
                Intent::KnowledgeQuery,
                "x",
                vec![1.0, 0.0, 0.0],
            )],
            min_top_sim: 0.9,
            min_margin: 0.0,
        };
        // Orthogonal query → 0 similarity
        let mut q = vec![0.0_f32, 1.0, 0.0];
        normalize(&mut q);
        assert!(r.classify_from_embedding(&q).is_none());
    }

    #[test]
    fn classify_returns_none_below_min_margin() {
        let r = EmbedRouter {
            exemplars: vec![
                make_exemplar(Intent::KnowledgeQuery, "x", vec![1.0, 0.0, 0.0]),
                make_exemplar(Intent::DeepQuery, "y", vec![0.9, 0.1, 0.0]),
            ],
            min_top_sim: 0.0,
            min_margin: 0.2, // tight
        };
        // Query close to both → margin too small to commit
        let mut q = vec![1.0_f32, 0.05, 0.0];
        normalize(&mut q);
        assert!(r.classify_from_embedding(&q).is_none());
    }

    #[test]
    fn classify_returns_none_when_exemplars_empty() {
        let r = EmbedRouter {
            exemplars: vec![],
            min_top_sim: 0.0,
            min_margin: 0.0,
        };
        assert!(r.classify_from_embedding(&[1.0, 0.0]).is_none());
    }
}
