// SPDX-License-Identifier: AGPL-3.0-or-later
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
    /// Optional scope axis ORTHOGONAL to intent. Forwarded through
    /// `EmbedClassification.scope` so downstream retrieval can bias
    /// corpus selection (e.g., `scope = "personal"` on a
    /// knowledge_query exemplar restricts atlas grounding to
    /// user-owned corpora — `mesh_sharing=false` in IndexInfo).
    /// `None` = no scope hint (current default behavior).
    #[serde(default)]
    scope: Option<String>,
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
    /// Optional scope tag from the source exemplar; propagates to
    /// `EmbedClassification.scope` when this exemplar is the
    /// nearest match.
    scope: Option<String>,
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
    /// Scope tag from the nearest exemplar (when set). Orthogonal
    /// to intent; downstream retrieval consumes this to bias corpus
    /// selection. Values today: `Some("personal")` for
    /// conversation-history / journaling shapes; `None` for the
    /// general / external-knowledge default.
    pub scope: Option<String>,
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
    pub async fn load(path: &Path, inference: Arc<dyn InferenceProvider>) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| Error::InvalidInput(format!("read exemplars {}: {e}", path.display())))?;
        Self::from_toml_str(&raw, inference).await
    }

    /// Build from in-memory TOML (the baked default in
    /// [`crate::router_bootstrap`], or any caller-supplied content). Identical
    /// parse + embed path to [`Self::load`] minus the file read, so a binary
    /// with no on-disk exemplars (a desktop `.app`) still gets the embed router
    /// — bench/desktop parity by construction.
    pub async fn from_toml_str(raw: &str, inference: Arc<dyn InferenceProvider>) -> Result<Self> {
        Self::from_toml_str_cached(raw, inference, None).await
    }

    /// [`Self::from_toml_str`] with an optional boot embed cache —
    /// exemplar embeddings are static per (text, model), and embedding
    /// ~175 of them sequentially at every boot is splash-screen time.
    pub async fn from_toml_str_cached(
        raw: &str,
        inference: Arc<dyn InferenceProvider>,
        mut cache: Option<&mut crate::router_embed_cache::BootEmbedCache>,
    ) -> Result<Self> {
        let parsed: ExemplarFile = toml::from_str(raw)
            .map_err(|e| Error::InvalidInput(format!("parse exemplars: {e}")))?;

        let mut exemplars = Vec::with_capacity(parsed.example.len());
        for row in parsed.example {
            let intent = parse_intent(&row.intent).map_err(|e| {
                Error::InvalidInput(format!("exemplar `{}`: {e}", truncate(&row.query, 60)))
            })?;
            let mut emb = match cache.as_deref_mut() {
                Some(c) => c.embed_query_cached(&*inference, &row.query).await?,
                None => inference.embed_query(&row.query).await?,
            };
            normalize(&mut emb);
            exemplars.push(Exemplar {
                intent,
                embedding: emb,
                query: row.query,
                scope: row.scope,
            });
        }

        tracing::info!(
            target: "router.embed",
            exemplar_count = exemplars.len(),
            "embed-router loaded"
        );

        Ok(Self {
            exemplars,
            min_top_sim: DEFAULT_MIN_TOP_SIM,
            min_margin: DEFAULT_MIN_MARGIN,
        })
    }

    /// Parse-only: the exemplar texts this router embeds, in file order,
    /// WITHOUT running inference. SSOT for the boot-cache freshness gate —
    /// shares the exact `ExemplarFile` parse + `query` field that
    /// `from_toml_str_cached` embeds above (`embed_query_cached`, the `q:`
    /// space), so the gate can never drift from what actually gets cached.
    pub fn exemplar_texts(raw: &str) -> Result<Vec<String>> {
        let parsed: ExemplarFile = toml::from_str(raw)
            .map_err(|e| Error::InvalidInput(format!("parse exemplars: {e}")))?;
        Ok(parsed.example.into_iter().map(|r| r.query).collect())
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

    /// Same as `classify` but returns the L2-normalised query
    /// embedding alongside the verdict, so the caller can reuse the
    /// embedding for downstream classifiers (e.g. the binary
    /// personal-scope classifier) without paying a second embed.
    pub async fn classify_returning_embedding(
        &self,
        query: &str,
        inference: &dyn InferenceProvider,
    ) -> Result<(Option<EmbedClassification>, Vec<f32>)> {
        if self.exemplars.is_empty() {
            // Still embed so caller can run scope classifier; cheap
            // single embed and matches the non-empty path's contract.
            let mut q = inference.embed_query(query).await?;
            normalize(&mut q);
            return Ok((None, q));
        }
        let mut q = inference.embed_query(query).await?;
        normalize(&mut q);
        let intent = self.classify_from_embedding(&q);
        Ok((intent, q))
    }

    /// Classify against a pre-computed query embedding. Public for
    /// callers that already have one (the router could splice this
    /// into the existing search-embedding pipeline to skip a second
    /// embed call).
    pub fn classify_from_embedding(&self, q_normalized: &[f32]) -> Option<EmbedClassification> {
        if self.exemplars.is_empty() || q_normalized.is_empty() {
            return None;
        }

        // Max similarity per intent + remember the nearest exemplar
        // (text + scope) for the diagnostic surface and downstream
        // routing bias.
        let mut per_intent: HashMap<Intent, (f32, &str, Option<&str>)> = HashMap::new();
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
                .and_modify(|(best, best_q, best_scope)| {
                    if sim > *best {
                        *best = sim;
                        *best_q = ex.query.as_str();
                        *best_scope = ex.scope.as_deref();
                    }
                })
                .or_insert((sim, ex.query.as_str(), ex.scope.as_deref()));
        }
        if per_intent.is_empty() {
            return None;
        }

        let mut ranked: Vec<(Intent, f32, &str, Option<&str>)> = per_intent
            .into_iter()
            .map(|(i, (s, q, sc))| (i, s, q, sc))
            .collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let (top_intent, top_sim, nearest, top_scope) =
            (ranked[0].0.clone(), ranked[0].1, ranked[0].2, ranked[0].3);
        let second_sim = ranked.get(1).map(|(_, s, _, _)| *s).unwrap_or(0.0);
        let margin = top_sim - second_sim;

        // Glassbox: the routing decision is the *first level* of the
        // whole stack — if intent classification is wrong, every
        // downstream choice (retrieval, expansion, synthesis) is built
        // on sand. Emit per-query whether the embed router was confident
        // enough to OWN this route (`decided=true`, short-circuiting the
        // heuristic + LLM cascade) or fell through (`decided=false`), with
        // the similarity/margin vs thresholds that drove it. On the
        // `router.embed` target, which the default daemon/eval filter
        // does NOT enable — so this is opt-in (`router.embed=info`) and
        // free in normal operation. Pairs with the second-best intent so
        // near-miss misroutes (the margin-just-cleared case) are visible.
        let second_intent = ranked.get(1).map(|(i, _, _, _)| format!("{i:?}"));
        let decided = top_sim >= self.min_top_sim && margin >= self.min_margin;
        tracing::info!(
            target: "router.embed",
            event = "classify",
            top_intent = ?top_intent,
            top_sim,
            second_intent = ?second_intent,
            second_sim,
            margin,
            min_top_sim = self.min_top_sim,
            min_margin = self.min_margin,
            decided,
            "router.embed: classify decision"
        );

        if !decided {
            return None;
        }
        Some(EmbedClassification {
            intent: top_intent,
            top_sim,
            margin,
            nearest_exemplar: truncate(nearest, 80),
            scope: top_scope.map(String::from),
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
        "code_query" | "CodeQuery" => Ok(Intent::CodeQuery),
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
        assert!(matches!(
            parse_intent("knowledge_query"),
            Ok(Intent::KnowledgeQuery)
        ));
        assert!(matches!(
            parse_intent("KnowledgeQuery"),
            Ok(Intent::KnowledgeQuery)
        ));
        assert!(parse_intent("nonsense").is_err());
    }

    fn make_exemplar(intent: Intent, query: &str, emb: Vec<f32>) -> Exemplar {
        let mut e = emb;
        normalize(&mut e);
        Exemplar {
            intent,
            embedding: e,
            query: query.into(),
            scope: None,
        }
    }

    #[test]
    fn classify_picks_max_similarity_intent_with_margin() {
        let r = EmbedRouter {
            exemplars: vec![
                make_exemplar(Intent::KnowledgeQuery, "What is X?", vec![1.0, 0.0, 0.0]),
                make_exemplar(Intent::DeepQuery, "Why did X happen?", vec![0.0, 1.0, 0.0]),
                make_exemplar(
                    Intent::MetalingualQuery,
                    "What does X mean here?",
                    vec![0.0, 0.0, 1.0],
                ),
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
