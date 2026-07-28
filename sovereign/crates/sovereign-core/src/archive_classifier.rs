// SPDX-License-Identifier: AGPL-3.0-or-later
//! Binary classifier for the ARCHIVE-vs-THREAD axis: is this question
//! about the user's PAST conversations (a corpus search over the chat
//! archive) or about THIS conversation (answerable from the message
//! list already in context)?
//!
//! ## Why this exists
//!
//! Measured 2026-07-26: "Have I mentioned kayaking in any of our past
//! chats?" routed to `MetalingualQuery`. The metalingual handler then
//! string-parsed the locator, got [`MetalingualLocator::Unknown`],
//! preferred CODE corpora, found nothing, and emitted the `no_source`
//! empty state. The user's own archive was never searched.
//!
//! The correct intent was *already* top-ranked — `KnowledgeQuery` with
//! `scope = "personal"` at 0.531 — but the embed router's intent floor
//! is 0.55, so it abstained and the LLM classifier decided. The LLM
//! picks metalingual, because the question IS about conversations; it
//! just isn't about *this* one.
//!
//! ## Why not a cheaper fix
//!
//! Both cheaper fixes were tried and rejected with numbers (see the
//! `attempt` note, 2026-07-26):
//!
//! 1. **More exemplars in `exemplars.toml`.** Similarity there is
//!    TOPIC-dominated, not shape-dominated: the same axis-C exemplar
//!    scores 1.000 on its own topic and 0.531 on a near-identical
//!    phrasing about a different topic. Adding rows buys the topics
//!    you add and nothing else.
//! 2. **A rule over the EXISTING axes** — `intent == Metalingual &&
//!    scope == personal && locator margin < 0 → KnowledgeQuery`.
//!    cells_v1's own metalingual question ("What did you mention
//!    earlier about retrieval?") scores scope personal +0.239 and
//!    locator margin −0.106, i.e. MORE negative on the locator axis
//!    than the archive query's −0.079. The two classes are not
//!    separated by either existing discriminator, so any threshold
//!    that catches the archive question flips the bench question.
//!
//! This module is the separate decision surface that leaves. It uses
//! the shape that worked for [`crate::scope_classifier`] (centroid
//! over ~20 varied-domain examples, topic-robust by construction)
//! rather than the shape that failed (k=1 nearest exemplar).
//!
//! ## Algorithm
//!
//! Identical to [`crate::scope_classifier::PersonalScopeClassifier`]:
//! load `[archive]` and `[thread]` example arrays, embed each with the
//! same instruction-prefixed `embed_query` the retrieval pipeline uses,
//! L2-normalise, sum per class, L2-normalise → one centroid per class.
//! At query time score `sim_a - sim_t` and fire only when BOTH an
//! absolute gate and a margin gate pass.
//!
//! ## Calibration (measured, not guessed — 2026-07-26)
//!
//! Qwen3-Embedding-0.6B-Q8_0, centroids from the shipped bank, scored
//! against a held-out set of 6 archive questions, 8 this-thread
//! questions, and 12 world/other questions (see
//! `tests/archive_axis_live.rs`):
//!
//! - **Prefixed vs unprefixed embeddings.** Prefixed (shared with the
//!   embed router, so it costs no extra embed) puts the world
//!   negatives at sim 0.13–0.42, comfortably under the absolute gate,
//!   so that gate does real work. Unprefixed pushes them to 0.32–0.60,
//!   overlapping the positives — "What did Kant say about duty?" lands
//!   at 0.449 against a 0.45 gate, a 0.001 cushion. That is fitting
//!   noise, so this axis uses the SHARED prefixed embedding.
//! - **At the shipped gate (0.50 / 0.04):** 5 of 6 held-out archive
//!   questions fire; 0 of 21 negatives fire. The target case fires at
//!   sim 0.645 / margin +0.077 (cushion 0.037). The cells_v1 hard gate
//!   sits at margin −0.079 (cushion 0.099 — an order of magnitude of
//!   the threshold).
//! - **The absolute gate was earned, not guessed.** It shipped at 0.45
//!   and the routing bench immediately found the shape the held-out
//!   set lacked: long reflective first-person prose
//!   (`voice_H09_journal_think_leak`) at sim 0.452 / margin +0.038,
//!   held out by 0.002. Raising to 0.50 changed no recall — the lowest
//!   firing positive is 0.568 — and moved that case behind 0.048 of
//!   absolute cushion. Both gates now block it independently.
//!
//! The gate is asymmetric on purpose, matching the locator axis: a
//! false positive restricts a world question to personal corpora and
//! is user-visible; a false negative just leaves today's behaviour in
//! place. Tune for precision.

use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::router_axis::{dot, normalize, AxisGate, AxisScore};
use crate::traits::InferenceProvider;

/// Margin gate. Twice the locator axis's 0.02 because these two
/// classes sit genuinely adjacent in embedding space (centroid
/// separation cos = 0.83) — thread questions are the nearest
/// negatives, not distant ones.
const DEFAULT_MIN_MARGIN: f32 = 0.04;
/// Absolute gate. Raised from 0.45 to 0.50 on 2026-07-26 after the
/// routing bench surfaced a near-miss the held-out set did not
/// contain: `voice_routing_v1`'s `voice_H09_journal_think_leak` — a
/// long reflective first-person passage about work and discipline —
/// scores `sim_archive = 0.452`, `margin = +0.038`. At the original
/// 0.45 floor it was held out ONLY by the margin gate, with a 0.002
/// cushion. Long introspective prose is close to the archive centroid
/// (it is first-person and memory-ish) without being an archive
/// question at all.
///
/// 0.50 costs nothing and buys a lot: the lowest-scoring held-out
/// positive that fires sits at 0.568, so recall is unchanged (5/6),
/// while that passage is now blocked by BOTH gates with 0.048 of
/// absolute cushion instead of 0.002 of margin. When the two classes
/// are this adjacent, prefer the gate that fails safe.
const DEFAULT_MIN_ARCHIVE_SIM: f32 = 0.50;

#[derive(Debug, Clone, Deserialize)]
struct ArchiveExamplesFile {
    #[serde(default)]
    archive: ArchiveClass,
    #[serde(default)]
    thread: ArchiveClass,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ArchiveClass {
    #[serde(default)]
    examples: Vec<String>,
}

/// What the axis saw, for the glassbox boot/route log and for the
/// router's user-facing rationale. Returned only when the gate fires.
#[derive(Debug, Clone, PartialEq)]
pub struct ArchiveVerdict {
    /// Cosine to the archive centroid.
    pub sim_archive: f32,
    /// Cosine to the this-thread centroid.
    pub sim_thread: f32,
    /// `sim_archive - sim_thread`.
    pub margin: f32,
}

/// Centroid-based binary classifier. One centroid per class (archive /
/// thread). Loaded + embedded at boot; classification is two dot
/// products against the query embedding the router already computed.
#[derive(Debug, Clone)]
pub struct ConversationArchiveClassifier {
    centroid_archive: Vec<f32>,
    centroid_thread: Vec<f32>,
    n_archive: usize,
    n_thread: usize,
    min_margin: f32,
    min_archive_sim: f32,
}

impl ConversationArchiveClassifier {
    /// Load examples from `path`, embed each one, compute per-class
    /// centroids.
    pub async fn load(path: &Path, inference: Arc<dyn InferenceProvider>) -> Result<Self> {
        let raw = std::fs::read_to_string(path).map_err(|e| {
            Error::InvalidInput(format!("read archive examples {}: {e}", path.display()))
        })?;
        Self::from_toml_str(&raw, inference).await
    }

    /// Build from in-memory TOML (the baked default in
    /// [`crate::router_bootstrap`], or any caller-supplied content).
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
        let parsed: ArchiveExamplesFile = toml::from_str(raw)
            .map_err(|e| Error::InvalidInput(format!("parse archive examples: {e}")))?;
        if parsed.archive.examples.is_empty() || parsed.thread.examples.is_empty() {
            return Err(Error::InvalidInput(
                "archive examples need non-empty [archive].examples and [thread].examples".into(),
            ));
        }

        let centroid_archive =
            compute_centroid(&parsed.archive.examples, &*inference, cache.as_deref_mut()).await?;
        let centroid_thread =
            compute_centroid(&parsed.thread.examples, &*inference, cache).await?;

        if centroid_archive.len() != centroid_thread.len() {
            return Err(Error::InvalidInput(format!(
                "archive centroid dim mismatch: archive={} thread={}",
                centroid_archive.len(),
                centroid_thread.len()
            )));
        }

        let n_archive = parsed.archive.examples.len();
        let n_thread = parsed.thread.examples.len();
        tracing::info!(
            target: "router.archive",
            n_archive,
            n_thread,
            dims = centroid_archive.len(),
            "conversation-archive classifier loaded"
        );

        Ok(Self {
            centroid_archive,
            centroid_thread,
            n_archive,
            n_thread,
            min_margin: DEFAULT_MIN_MARGIN,
            min_archive_sim: DEFAULT_MIN_ARCHIVE_SIM,
        })
    }

    /// Parse-only: the exemplar texts this classifier embeds
    /// (`archive` then `thread`), WITHOUT running inference. SSOT for
    /// the boot-cache freshness gate — shares the exact parse the
    /// centroid path uses, so the gate can never drift from what
    /// actually gets cached.
    pub fn exemplar_texts(raw: &str) -> Result<Vec<String>> {
        let parsed: ArchiveExamplesFile = toml::from_str(raw)
            .map_err(|e| Error::InvalidInput(format!("parse archive examples: {e}")))?;
        Ok(parsed
            .archive
            .examples
            .into_iter()
            .chain(parsed.thread.examples)
            .collect())
    }

    /// Override the default thresholds. Useful for tests + tuning.
    pub fn with_thresholds(mut self, min_archive_sim: f32, min_margin: f32) -> Self {
        self.min_archive_sim = min_archive_sim;
        self.min_margin = min_margin;
        self
    }

    pub fn archive_count(&self) -> usize {
        self.n_archive
    }
    pub fn thread_count(&self) -> usize {
        self.n_thread
    }

    /// Classify against a pre-computed, L2-normalised query embedding.
    /// Returns `Some(verdict)` only when both the absolute and margin
    /// gates pass — i.e. "this asks about the conversation ARCHIVE".
    /// `None` means "not decisively archive", which includes both
    /// this-thread questions and everything else; callers must treat
    /// `None` as "no signal", never as "this thread".
    pub fn classify_from_embedding(&self, q_normalized: &[f32]) -> Option<ArchiveVerdict> {
        let score = self.score_from_embedding(q_normalized)?;
        let gate = self.gate();
        let fires = gate.admits(score);
        tracing::info!(
            target: "router.archive",
            sim_archive = score.sim_positive,
            sim_thread = score.sim_negative,
            margin = score.margin(),
            min_archive_sim = self.min_archive_sim,
            min_margin = self.min_margin,
            cushion = gate.cushion(score),
            fires,
            "archive classification"
        );
        fires.then_some(ArchiveVerdict {
            sim_archive: score.sim_positive,
            sim_thread: score.sim_negative,
            margin: score.margin(),
        })
    }

    /// Raw, UNGATED score: cosine to each class centroid.
    ///
    /// `None` only on a dimension mismatch. Split out from
    /// [`Self::classify_from_embedding`] so [`crate::router_calibration`]
    /// can evaluate any candidate gate from a single embedding pass.
    /// This axis is the reason that matters: its floor moved 0.45 →
    /// 0.50 because one real question was held out by 0.002 of margin,
    /// and finding that by hand cost a bench run and a day.
    pub fn score_from_embedding(&self, q_normalized: &[f32]) -> Option<AxisScore> {
        if q_normalized.len() != self.centroid_archive.len() {
            tracing::warn!(
                target: "router.archive",
                q_dim = q_normalized.len(),
                centroid_dim = self.centroid_archive.len(),
                "archive: dimension mismatch — skipping"
            );
            return None;
        }
        Some(AxisScore::new(
            dot(q_normalized, &self.centroid_archive),
            dot(q_normalized, &self.centroid_thread),
        ))
    }

    /// The gate currently applied to this axis.
    pub fn gate(&self) -> AxisGate {
        AxisGate::new(self.min_archive_sim, self.min_margin)
    }

    /// Convenience: embed `query` via `inference` and classify. Prefer
    /// [`Self::classify_from_embedding`] when the router already has
    /// the query embedding — this axis is calibrated on the SHARED
    /// instruction-prefixed vector and must not be given another.
    pub async fn classify(
        &self,
        query: &str,
        inference: &dyn InferenceProvider,
    ) -> Result<Option<ArchiveVerdict>> {
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
        // `embed_query` (instruction-prefixed) — NOT `embed`. The
        // calibration in the module docs is against this space; the
        // unprefixed space collapses the world negatives into the
        // positive range.
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

    fn synthetic(min_archive_sim: f32, min_margin: f32) -> ConversationArchiveClassifier {
        ConversationArchiveClassifier {
            centroid_archive: vec![1.0, 0.0],
            centroid_thread: vec![0.0, 1.0],
            n_archive: 1,
            n_thread: 1,
            min_margin,
            min_archive_sim,
        }
    }

    #[test]
    fn gates_require_both_absolute_and_margin() {
        let c = synthetic(0.5, 0.05);
        // At the archive centroid → fires.
        assert!(c.classify_from_embedding(&[1.0, 0.0]).is_some());
        // Orthogonal to both → absolute gate blocks.
        assert!(c.classify_from_embedding(&[0.0, 0.0]).is_none());
        // At the thread centroid → blocks.
        assert!(c.classify_from_embedding(&[0.0, 1.0]).is_none());
        // Midpoint → margin 0 → blocks.
        let half = (0.5f32).sqrt();
        assert!(c.classify_from_embedding(&[half, half]).is_none());
    }

    #[test]
    fn margin_gate_blocks_when_absolute_passes() {
        // sim_archive high enough, but the thread centroid is nearly
        // as close — exactly the this-thread failure mode this axis
        // must not fire on.
        let c = synthetic(0.5, 0.10);
        // (0.8, 0.6) normalised: sim_a = 0.8, sim_t = 0.6, margin 0.2 → fires.
        assert!(c.classify_from_embedding(&[0.8, 0.6]).is_some());
        // (0.72, 0.69): margin 0.03 < 0.10 → blocked despite sim_a > 0.5.
        assert!(c.classify_from_embedding(&[0.72, 0.69]).is_none());
    }

    #[test]
    fn verdict_reports_both_similarities() {
        let c = synthetic(0.5, 0.05);
        let v = c.classify_from_embedding(&[1.0, 0.0]).expect("fires");
        assert!((v.sim_archive - 1.0).abs() < 1e-6);
        assert!(v.sim_thread.abs() < 1e-6);
        assert!((v.margin - 1.0).abs() < 1e-6);
    }

    #[test]
    fn dimension_mismatch_is_none_not_panic() {
        let c = synthetic(0.5, 0.05);
        assert!(c.classify_from_embedding(&[1.0, 0.0, 0.0]).is_none());
    }

    #[test]
    fn exemplar_texts_is_archive_then_thread() {
        let raw = r#"
[archive]
examples = ["a1", "a2"]
[thread]
examples = ["t1"]
"#;
        let texts = ConversationArchiveClassifier::exemplar_texts(raw).expect("parse");
        assert_eq!(texts, vec!["a1", "a2", "t1"]);
    }

    #[test]
    fn empty_class_is_rejected() {
        let raw = r#"
[archive]
examples = []
[thread]
examples = ["t1"]
"#;
        assert!(ConversationArchiveClassifier::exemplar_texts(raw).is_ok());
        // The centroid path is what enforces non-empty; parse alone is lenient.
        let parsed: ArchiveExamplesFile = toml::from_str(raw).expect("parse");
        assert!(parsed.archive.examples.is_empty());
    }

    #[test]
    fn shipped_bank_parses_and_is_balanced() {
        let raw = include_str!("../../../router/archive_examples.toml");
        let parsed: ArchiveExamplesFile = toml::from_str(raw).expect("shipped bank parses");
        assert!(
            parsed.archive.examples.len() >= 15,
            "archive class too small: {}",
            parsed.archive.examples.len()
        );
        assert!(
            parsed.thread.examples.len() >= 15,
            "thread class too small: {}",
            parsed.thread.examples.len()
        );
    }

    /// The bank must not contain the questions the axis is evaluated
    /// on. Coaching to the held-out set produces inflated numbers that
    /// hide the architectural problem (scope_examples.toml principle 1).
    #[test]
    fn shipped_bank_is_disjoint_from_evaluation_sets() {
        let raw = include_str!("../../../router/archive_examples.toml");
        let texts = ConversationArchiveClassifier::exemplar_texts(raw).expect("parse");
        let held_out = [
            // cells_v1 metalingual row — the hard gate.
            "What did you mention earlier about retrieval?",
            // archive_axis_live.rs held-out positives.
            "Have I mentioned kayaking in any of our past chats?",
            "Across my past conversations, what have I said about my sleep?",
            "Did I ever bring up learning the cello in a previous chat?",
            "What have I asked you about gardening across all our chats?",
            "Is there an old conversation where I talked about quitting?",
            "In earlier sessions, what did I say my goals were?",
        ];
        for h in held_out {
            let needle = h.to_lowercase();
            assert!(
                !texts.iter().any(|t| t.to_lowercase() == needle),
                "shipped bank contains held-out evaluation question: {h}"
            );
        }
    }
}
